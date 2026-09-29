//! A store that counts every call and delegates to the in-memory one.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use turnframe_core::case::CaseKey;
use turnframe_core::error::ExecutionError;
use turnframe_core::event::{EventRedaction, OutboxEntry};
use turnframe_core::ids::{
    AccountId, CaseRevision, CommandId, ConversationId, EventId, InteractionId, OptionId, OutboxId,
    RedactionAuthority, TurnId,
};
use turnframe_core::interaction::{Interaction, InteractionStatus};
use turnframe_core::replay::{ReplayRecord, TurnPhase};
use turnframe_core::response::AssistantTurn;
use turnframe_store::commit::{CommitBundle, CommitReceipt, CommitStore};
use turnframe_store::conversation::{
    ConversationReader, ConversationRecord, ConversationWriter, RecoveryScope, StoredTurn,
    StoredUserTurn, TurnPhaseMarker,
};
use turnframe_store::error::StoreError;
use turnframe_store::events::{
    EventBatch, EventCursor, EventJournalReader, EventJournalWriter, EventPage, StoredEvent,
};
use turnframe_store::interaction::{
    InteractionReader, InteractionRecord, InteractionWriter, InvalidationReason, ResolutionOutcome,
};
use turnframe_store::journal::{
    CommandJournalEntry, CommandJournalReader, CommandJournalWriter, JournalAdmission,
    JournalOutcome,
};
use turnframe_store::memory::MemoryStores;
use turnframe_store::outbox::{OutboxReader, OutboxRecord, OutboxWriter};
use turnframe_store::replay::{ReplayReader, ReplayWriter};

/// How many times each store method was called.
///
/// Keys are `"<role>.<method>"`, e.g. `"journal.begin"`, so they read the way
/// the persistence contract is written and sort by role.
#[derive(Default)]
pub struct StoreCallCounter {
    counts: Mutex<BTreeMap<&'static str, usize>>,
}

impl StoreCallCounter {
    /// A counter with nothing recorded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one call.
    pub fn record(&self, method: &'static str) {
        *self.lock().entry(method).or_insert(0) += 1;
    }

    /// How many times `method` was called, `0` when never.
    #[must_use]
    pub fn count(&self, method: &str) -> usize {
        self.lock().get(method).copied().unwrap_or(0)
    }

    /// Every method that was called at least once, in key order.
    #[must_use]
    pub fn counts(&self) -> Vec<(&'static str, usize)> {
        self.lock()
            .iter()
            .map(|(method, count)| (*method, *count))
            .collect()
    }

    /// Total calls across every method.
    #[must_use]
    pub fn total(&self) -> usize {
        self.lock().values().sum()
    }

    /// Forgets everything recorded.
    pub fn reset(&self) {
        self.lock().clear();
    }

    /// Locks, recovering from poisoning: a test that already failed must not
    /// cascade into unrelated failures.
    fn lock(&self) -> MutexGuard<'_, BTreeMap<&'static str, usize>> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for StoreCallCounter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreCallCounter")
            .field("total", &self.total())
            .field("methods", &self.counts())
            .finish()
    }
}

/// Every persistence trait, counted and delegated to one [`MemoryStores`].
///
/// It adds nothing to the semantics: the answers, the errors and the injected
/// failures are the in-memory store's own. Only the tally is new.
#[derive(Debug)]
pub struct CountingStores {
    inner: Arc<MemoryStores>,
    counter: Arc<StoreCallCounter>,
}

impl CountingStores {
    /// Wraps `inner`, recording into `counter`.
    #[must_use]
    pub fn new(inner: Arc<MemoryStores>, counter: Arc<StoreCallCounter>) -> Self {
        Self { inner, counter }
    }

    /// The store being counted.
    #[must_use]
    pub fn inner(&self) -> &Arc<MemoryStores> {
        &self.inner
    }

    /// The tally.
    #[must_use]
    pub fn counter(&self) -> &Arc<StoreCallCounter> {
        &self.counter
    }
}

/// Generates one counted, delegating implementation of a store trait.
///
/// The whole `impl` block is generated at once because `#[async_trait]` has to
/// see the `async fn`s: a macro invocation *inside* the block would still be
/// unexpanded when the attribute runs. Delegation is written as
/// `<MemoryStores as Trait>::method` because several of these traits declare a
/// method called `get`.
///
/// Each role is implemented as its reader half and its writer half, which is
/// how the persistence contract is split; the combined trait
/// (`ConversationStore`, `ReplayStore`, …) follows from its blanket
/// implementation. Both halves of a role share one counter prefix, so a caller
/// still asks for `"conversations.load_turn"` without knowing which half it
/// landed on.
macro_rules! counting_impl {
    (
        $trait:path, $prefix:literal,
        $( fn $name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) -> $ret:ty ; )*
    ) => {
        #[async_trait::async_trait]
        impl $trait for CountingStores {
            $(
                async fn $name(&self, $( $arg : $ty ),*) -> $ret {
                    self.counter.record(concat!($prefix, ".", stringify!($name)));
                    <MemoryStores as $trait>::$name(&self.inner, $( $arg ),*).await
                }
            )*
        }
    };
}

counting_impl! {
    ConversationReader, "conversations",
    fn load_conversation(
        account: &AccountId,
        id: &ConversationId,
    ) -> Result<ConversationRecord, StoreError>;
    fn load_recent_turns(
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<StoredTurn>, StoreError>;
    fn load_turn(account: &AccountId, turn_id: &TurnId) -> Result<StoredTurn, StoreError>;
    fn turn_phase(
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<TurnPhaseMarker, StoreError>;
    fn list_unfinished_turns(
        scope: RecoveryScope,
        limit: usize,
    ) -> Result<Vec<TurnPhaseMarker>, StoreError>;
}

counting_impl! {
    ConversationWriter, "conversations",
    fn create_conversation(record: ConversationRecord) -> Result<(), StoreError>;
    fn append_user_turn(turn: StoredUserTurn) -> Result<(), StoreError>;
    fn append_assistant_turn(
        account: &AccountId,
        turn: AssistantTurn,
    ) -> Result<(), StoreError>;
    fn set_turn_phase(
        account: &AccountId,
        turn_id: &TurnId,
        phase: TurnPhase,
    ) -> Result<TurnPhaseMarker, StoreError>;
}

counting_impl! {
    InteractionReader, "interactions",
    fn get(
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, StoreError>;
    fn list_open_for_conversation(
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Vec<Interaction>, StoreError>;
    fn list_open_for_case(
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<Interaction>, StoreError>;
    fn blocking_answered_at(
        account: &AccountId,
        case_key: &CaseKey,
        revision: CaseRevision,
    ) -> Result<bool, StoreError>;
}

counting_impl! {
    InteractionWriter, "interactions",
    fn insert(interaction: Interaction) -> Result<(), StoreError>;
    fn insert_replacing_blocking(
        interaction: Interaction,
    ) -> Result<Vec<InteractionId>, StoreError>;
    fn begin_resolution(
        account: &AccountId,
        id: &InteractionId,
        expected_status: InteractionStatus,
        option_id: OptionId,
        resolved_by: TurnId,
    ) -> Result<InteractionRecord, StoreError>;
    fn finish_resolution(
        account: &AccountId,
        id: &InteractionId,
        outcome: ResolutionOutcome,
    ) -> Result<InteractionRecord, StoreError>;
    fn invalidate_for_case(
        account: &AccountId,
        case_key: &CaseKey,
        new_revision: CaseRevision,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError>;
    fn invalidate_case_cards(
        account: &AccountId,
        case_key: &CaseKey,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError>;
    fn expire_due(now: DateTime<Utc>) -> Result<Vec<InteractionId>, StoreError>;
}

counting_impl! {
    CommandJournalReader, "journal",
    fn get(
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<CommandJournalEntry, StoreError>;
    fn for_turn(
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError>;
    fn pending_for_turn(
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError>;
}

counting_impl! {
    CommandJournalWriter, "journal",
    fn begin(entry: CommandJournalEntry) -> Result<JournalAdmission, StoreError>;
    fn mark_executing(
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<(), StoreError>;
    fn complete(
        account: &AccountId,
        command_id: &CommandId,
        outcome: JournalOutcome,
    ) -> Result<(), StoreError>;
    fn fail(
        account: &AccountId,
        command_id: &CommandId,
        error: &ExecutionError,
    ) -> Result<(), StoreError>;
}

counting_impl! {
    EventJournalWriter, "events",
    fn append(batch: EventBatch) -> Result<Vec<EventId>, StoreError>;
    fn redact_payload(
        account: &AccountId,
        event_id: &EventId,
        authority: &RedactionAuthority,
    ) -> Result<EventRedaction, StoreError>;
}

counting_impl! {
    EventJournalReader, "events",
    fn list_since(
        account: &AccountId,
        case_key: &CaseKey,
        since: CaseRevision,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError>;
    fn read_from(
        account: &AccountId,
        after: EventCursor,
        limit: usize,
    ) -> Result<EventPage, StoreError>;
    fn get_by_ids(
        account: &AccountId,
        ids: &[EventId],
    ) -> Result<Vec<StoredEvent>, StoreError>;
    fn count(account: &AccountId, case_key: &CaseKey) -> Result<u64, StoreError>;
}

counting_impl! {
    OutboxReader, "outbox",
    fn get(outbox_id: &OutboxId) -> Result<OutboxRecord, StoreError>;
    fn list_for_command(command_id: &CommandId) -> Result<Vec<OutboxRecord>, StoreError>;
}

counting_impl! {
    OutboxWriter, "outbox",
    fn enqueue(entry: OutboxEntry) -> Result<(), StoreError>;
    fn claim_due(
        now: DateTime<Utc>,
        limit: usize,
        worker_id: &str,
    ) -> Result<Vec<OutboxEntry>, StoreError>;
    fn mark_completed(outbox_id: &OutboxId) -> Result<(), StoreError>;
    fn mark_failed(
        outbox_id: &OutboxId,
        reason: String,
        retry_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError>;
    fn mark_outcome_unknown(
        outbox_id: &OutboxId,
        remote_ref: Option<String>,
    ) -> Result<(), StoreError>;
    fn reschedule(
        outbox_id: &OutboxId,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    fn release_expired_claims(
        claimed_before: DateTime<Utc>,
    ) -> Result<Vec<OutboxId>, StoreError>;
}

counting_impl! {
    ReplayReader, "replay",
    fn get(account: &AccountId, turn_id: &TurnId) -> Result<ReplayRecord, StoreError>;
    fn list_for_conversation(
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<ReplayRecord>, StoreError>;
}

counting_impl! {
    ReplayWriter, "replay",
    fn put(record: ReplayRecord) -> Result<(), StoreError>;
}

counting_impl! {
    CommitStore, "commit",
    fn commit(
        account: &AccountId,
        bundle: CommitBundle,
    ) -> Result<CommitReceipt, StoreError>;
}
