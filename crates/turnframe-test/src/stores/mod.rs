//! Fake stores: the in-memory persistence layer with the three things a
//! runtime test always ends up needing (spec §27.4, §27.7).
//!
//! `turnframe-store` already ships the deterministic implementation and the
//! conformance suite that proves an adapter right. What it does not ship is the
//! ergonomics of *driving* it from a turn test, and those are the same three
//! every time:
//!
//! * **crash where I say.** Recovery code is never exercised by the happy path,
//!   so [`FakeStores::fail_at`] arms a failure at one of the boundaries of
//!   spec §27.7, and [`crash_boundary`] looks one up by name so a table-driven
//!   chaos test can walk all seven.
//! * **count what was called.** "The turn wrote the journal once" and "the
//!   recovery path did not re-append the events" are assertions about call
//!   counts, not about state. Every trait method is tallied as
//!   `"<role>.<method>"`.
//! * **stop the clock.** Expiry, claim staleness and phase markers are
//!   timestamps the store stamps itself; a frozen clock turns "the card
//!   expired" into something the test makes true instead of waits for.
//!
//! And then: **assert what was persisted**. The accessors on [`FakeStores`]
//! read through the underlying store *without* counting, so checking the
//! outcome of a turn never pollutes the tally the same test is asserting on.
//!
//! # The suite comes with it
//!
//! [`conformance`] is the store crate's own suite, re-exported so an adopter
//! building a store over their own database reaches fixtures, doubles and the
//! contract through this one crate.
//!
//! ```
//! use turnframe_core::ids::{AccountId, ConversationId};
//! use turnframe_store::conversation::{ConversationRecord, ConversationStore};
//! use turnframe_test::stores::FakeStores;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let fake = FakeStores::new();
//! let account = AccountId::from("aurora");
//! let conversation = ConversationId::nil();
//!
//! fake.stores()
//!     .conversations()
//!     .create_conversation(ConversationRecord::new(conversation, account.clone(), fake.now()))
//!     .await?;
//!
//! assert_eq!(fake.call_count("conversations.create_conversation"), 1);
//! # Ok::<(), turnframe_store::error::StoreError>(())
//! # })?;
//! # Ok(())
//! # }
//! ```

mod counting;

use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use turnframe_core::case::CaseKey;
use turnframe_core::ids::{AccountId, CommandId, ConversationId, EventId, InteractionId, TurnId};
use turnframe_core::interaction::Interaction;
use turnframe_core::replay::ReplayRecord;
use turnframe_store::conversation::{ConversationReader, StoredTurn, TurnPhaseMarker};
use turnframe_store::error::StoreError;
use turnframe_store::events::{EventJournalReader, StoredEvent};
use turnframe_store::interaction::{InteractionReader, InteractionRecord};
use turnframe_store::journal::{CommandJournalEntry, CommandJournalReader};
use turnframe_store::outbox::{OutboxReader, OutboxRecord};
use turnframe_store::replay::ReplayReader;

pub use counting::{CountingStores, StoreCallCounter};
pub use turnframe_store::conformance;
pub use turnframe_store::memory::{Clock, FailurePoint, ManualClock, MemoryStores, SystemClock};
pub use turnframe_store::stores::Stores;

/// How many events [`FakeStores::events_for`] reads at most. High enough that a
/// test never has to think about it, low enough to stay a bounded read.
pub const MAX_LISTED_EVENTS: usize = 1024;

/// The crash boundaries of spec §27.7, by name.
///
/// The names are the ones the specification uses, so a chaos test can be
/// written as a table and read next to the paragraph it implements.
pub const CRASH_BOUNDARIES: [(&str, FailurePoint); 7] = [
    (
        "after_interaction_persistence",
        FailurePoint::AfterInteractionPersistence,
    ),
    ("before_journal_insert", FailurePoint::BeforeJournalInsert),
    (
        "after_journal_insert_before_commit",
        FailurePoint::AfterJournalInsertBeforeCommit,
    ),
    (
        "after_commit_before_event_readback",
        FailurePoint::AfterCommitBeforeEventReadback,
    ),
    ("before_outbox_dispatch", FailurePoint::BeforeOutboxDispatch),
    ("after_outbox_dispatch", FailurePoint::AfterOutboxDispatch),
    (
        "before_response_persistence",
        FailurePoint::BeforeResponsePersistence,
    ),
];

/// The boundary with this name, or `None`.
#[must_use]
pub fn crash_boundary(name: &str) -> Option<FailurePoint> {
    CRASH_BOUNDARIES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, point)| *point)
}

/// The name of a boundary, or `"unknown"` for one added after this table.
#[must_use]
pub fn boundary_name(point: FailurePoint) -> &'static str {
    CRASH_BOUNDARIES
        .iter()
        .find(|(_, candidate)| *candidate == point)
        .map_or("unknown", |(name, _)| *name)
}

/// A name that is not one of the [`CRASH_BOUNDARIES`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no crash boundary is named `{name}`")]
pub struct UnknownBoundary {
    /// The name that was not found.
    pub name: String,
}

/// The in-memory stores, wrapped for turn tests.
///
/// Cheap to build and self-contained: one [`MemoryStores`] on a frozen
/// [`ManualClock`], a [`CountingStores`] in front of it, and a [`Stores`] built
/// from the counting layer — which is the value the runtime takes.
#[derive(Debug)]
pub struct FakeStores {
    memory: Arc<MemoryStores>,
    counter: Arc<StoreCallCounter>,
    clock: Arc<ManualClock>,
    stores: Stores,
}

impl FakeStores {
    /// Empty stores on a clock frozen at the Unix epoch.
    #[must_use]
    pub fn new() -> Self {
        Self::at(DateTime::<Utc>::UNIX_EPOCH)
    }

    /// Empty stores on a clock frozen at `start`.
    #[must_use]
    pub fn at(start: DateTime<Utc>) -> Self {
        let clock = Arc::new(ManualClock::new(start));
        let memory = Arc::new(MemoryStores::with_clock(clock.clone()));
        let counter = Arc::new(StoreCallCounter::new());
        let counting = Arc::new(CountingStores::new(memory.clone(), counter.clone()));
        let stores = Stores::builder()
            .conversations(counting.clone())
            .interactions(counting.clone())
            .journal(counting.clone())
            .events(counting.clone())
            .outbox(counting.clone())
            .replay(counting.clone())
            .commit(counting)
            .build()
            // Every role was just supplied, so the builder cannot refuse. The
            // fallback keeps this constructor infallible without an unwrap.
            .unwrap_or_else(|_| Stores::from_memory(memory.clone()));
        Self {
            memory,
            counter,
            clock,
            stores,
        }
    }

    /// The set of stores to hand to the code under test.
    #[must_use]
    pub fn stores(&self) -> &Stores {
        &self.stores
    }

    /// The underlying store, for the few things only it can do.
    #[must_use]
    pub fn memory(&self) -> &Arc<MemoryStores> {
        &self.memory
    }

    /// The clock the store stamps its own columns from.
    #[must_use]
    pub fn clock(&self) -> &Arc<ManualClock> {
        &self.clock
    }

    /// The instant the store would stamp a write with right now.
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Moves the clock forward.
    pub fn advance(&self, delta: TimeDelta) {
        self.clock.advance(delta);
    }

    /// Moves the clock to `instant`.
    pub fn set_time(&self, instant: DateTime<Utc>) {
        self.clock.set(instant);
    }

    // ---- failure injection -------------------------------------------------

    /// Arms one failure at `point`: the next call reaching it returns `error`.
    ///
    /// Whether the write before the boundary survives is part of the point's
    /// meaning; see [`FailurePoint`].
    ///
    /// # Errors
    /// * `Corrupt` when the store's internal lock is poisoned.
    pub fn fail_at(&self, point: FailurePoint, error: StoreError) -> Result<(), StoreError> {
        self.memory.fail_next(point, error)
    }

    /// Arms one failure at the boundary with this name (see
    /// [`CRASH_BOUNDARIES`]).
    ///
    /// # Errors
    /// * [`UnknownBoundary`] when no boundary carries that name.
    pub fn fail_at_boundary(&self, name: &str, error: StoreError) -> Result<(), UnknownBoundary> {
        let point = crash_boundary(name).ok_or_else(|| UnknownBoundary {
            name: name.to_owned(),
        })?;
        // A poisoned lock cannot be reported through this signature and is not
        // what the caller is testing; the arming is simply dropped.
        let _ = self.memory.fail_next(point, error);
        Ok(())
    }

    /// The boundaries still armed, in arming order.
    #[must_use]
    pub fn armed_failures(&self) -> Vec<FailurePoint> {
        self.memory.armed_failures().unwrap_or_default()
    }

    /// Disarms every pending failure.
    pub fn clear_failures(&self) {
        let _ = self.memory.clear_failures();
    }

    // ---- call counting -----------------------------------------------------

    /// How many times `method` was called, as `"<role>.<method>"`.
    #[must_use]
    pub fn call_count(&self, method: &str) -> usize {
        self.counter.count(method)
    }

    /// Every method called at least once, in key order.
    #[must_use]
    pub fn calls(&self) -> Vec<(&'static str, usize)> {
        self.counter.counts()
    }

    /// Total calls across every store method.
    #[must_use]
    pub fn total_calls(&self) -> usize {
        self.counter.total()
    }

    /// Forgets the tally, keeping the data. Use it between the arrange and act
    /// phases of a test.
    pub fn reset_calls(&self) {
        self.counter.reset();
    }

    // ---- what was persisted ------------------------------------------------
    //
    // These read the store directly rather than through the counting layer: an
    // assertion about the outcome must not change the tally the same test is
    // asserting on.

    /// Every event of a case, in append order.
    ///
    /// # Errors
    /// * Whatever the store returns; `NotFound` is never one of them, an
    ///   unknown case simply has no events.
    pub async fn events_for(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        EventJournalReader::list_since(
            self.memory.as_ref(),
            account,
            case_key,
            turnframe_core::ids::CaseRevision::ZERO,
            MAX_LISTED_EVENTS,
        )
        .await
    }

    /// The event types of a case, in append order: the shortest assertion about
    /// what a turn actually committed.
    ///
    /// # Errors
    /// * Whatever the store returns.
    pub async fn event_types(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<String>, StoreError> {
        Ok(self
            .events_for(account, case_key)
            .await?
            .into_iter()
            .map(|event| event.event_type)
            .collect())
    }

    /// The events with these identifiers, for checking a receipt's citations.
    ///
    /// # Errors
    /// * Whatever the store returns.
    pub async fn events_by_ids(
        &self,
        account: &AccountId,
        ids: &[EventId],
    ) -> Result<Vec<StoredEvent>, StoreError> {
        EventJournalReader::get_by_ids(self.memory.as_ref(), account, ids).await
    }

    /// The journal entry of a command.
    ///
    /// # Errors
    /// * `NotFound` when the command was never admitted for this account.
    pub async fn journal_entry(
        &self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<CommandJournalEntry, StoreError> {
        CommandJournalReader::get(self.memory.as_ref(), account, command_id).await
    }

    /// Every journal entry of a turn, in admission order.
    ///
    /// # Errors
    /// * Whatever the store returns.
    pub async fn journal_for_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError> {
        CommandJournalReader::for_turn(self.memory.as_ref(), account, turn_id).await
    }

    /// The open cards of a case.
    ///
    /// # Errors
    /// * Whatever the store returns.
    pub async fn open_interactions(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<Interaction>, StoreError> {
        InteractionReader::list_open_for_case(self.memory.as_ref(), account, case_key).await
    }

    /// One card, whatever its status.
    ///
    /// # Errors
    /// * `NotFound` when it does not exist for this account.
    pub async fn interaction(
        &self,
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, StoreError> {
        InteractionReader::get(self.memory.as_ref(), account, id).await
    }

    /// A stored turn, user side and assistant side.
    ///
    /// # Errors
    /// * `NotFound` when the turn does not exist for this account.
    pub async fn turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<StoredTurn, StoreError> {
        ConversationReader::load_turn(self.memory.as_ref(), account, turn_id).await
    }

    /// The phase marker of a turn.
    ///
    /// # Errors
    /// * `NotFound` when the turn does not exist for this account.
    pub async fn turn_phase(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<TurnPhaseMarker, StoreError> {
        ConversationReader::turn_phase(self.memory.as_ref(), account, turn_id).await
    }

    /// The replay record of a turn.
    ///
    /// # Errors
    /// * `NotFound` when no record was written for this account and turn.
    pub async fn replay_record(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<ReplayRecord, StoreError> {
        ReplayReader::get(self.memory.as_ref(), account, turn_id).await
    }

    /// The outbox rows a command produced.
    ///
    /// # Errors
    /// * Whatever the store returns.
    pub async fn outbox_for_command(
        &self,
        command_id: &CommandId,
    ) -> Result<Vec<OutboxRecord>, StoreError> {
        OutboxReader::list_for_command(self.memory.as_ref(), command_id).await
    }

    /// Every conversation-scoped replay record, most recent first.
    ///
    /// # Errors
    /// * Whatever the store returns.
    pub async fn replay_records_for(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<ReplayRecord>, StoreError> {
        ReplayReader::list_for_conversation(self.memory.as_ref(), account, conversation, limit)
            .await
    }
}

impl Default for FakeStores {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::ids::{BlockId, ConversationId, TurnId};
    use turnframe_core::locale::Locale;
    use turnframe_core::replay::TurnPhase;
    use turnframe_core::response::{
        AssistantTurn, GeneratedTransition, ReplayToken, ResponseBlock,
    };
    use turnframe_core::turn::{ActorContext, TurnInput};
    use turnframe_store::conversation::{ConversationRecord, StoredUserTurn};

    fn account() -> AccountId {
        AccountId::from("aurora")
    }

    fn turn_input(conversation: ConversationId, turn_id: TurnId) -> TurnInput {
        TurnInput {
            turn_id,
            conversation_id: conversation,
            actor: ActorContext::new("aurora", "user-1"),
            text: Some("ciao".to_owned()),
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: Locale::from("it"),
            effort: None,
        }
    }

    fn assistant(turn_id: TurnId, conversation: ConversationId) -> AssistantTurn {
        AssistantTurn {
            turn_id,
            conversation_id: conversation,
            blocks: vec![ResponseBlock::Transition(GeneratedTransition {
                block_id: BlockId::from("t1"),
                text: "fatto".to_owned(),
                facts_used: Vec::new(),
            })],
            replay_token: ReplayToken::from("rt"),
            subjects: Vec::new(),
            expectations: Vec::new(),
            done: Vec::new(),
            offers: Vec::new(),
        }
    }

    /// Creates a conversation and a user turn, then forgets the tally.
    async fn arranged() -> (FakeStores, ConversationId, TurnId) {
        let fake = FakeStores::new();
        let conversation = ConversationId::nil();
        let turn_id = TurnId::nil();
        fake.stores()
            .conversations()
            .create_conversation(ConversationRecord::new(conversation, account(), fake.now()))
            .await
            .unwrap();
        fake.stores()
            .conversations()
            .append_user_turn(StoredUserTurn::new(
                turn_input(conversation, turn_id),
                fake.now(),
            ))
            .await
            .unwrap();
        fake.reset_calls();
        (fake, conversation, turn_id)
    }

    #[tokio::test]
    async fn every_call_through_the_set_is_counted_by_role_and_method() {
        let (fake, _conversation, turn_id) = arranged().await;
        fake.stores()
            .conversations()
            .turn_phase(&account(), &turn_id)
            .await
            .unwrap();
        fake.stores()
            .conversations()
            .turn_phase(&account(), &turn_id)
            .await
            .unwrap();

        assert_eq!(fake.call_count("conversations.turn_phase"), 2);
        assert_eq!(fake.total_calls(), 2);
        assert_eq!(fake.calls(), vec![("conversations.turn_phase", 2)]);
    }

    #[tokio::test]
    async fn reading_what_was_persisted_does_not_move_the_tally() {
        let (fake, conversation, turn_id) = arranged().await;
        fake.stores()
            .conversations()
            .append_assistant_turn(&account(), assistant(turn_id, conversation))
            .await
            .unwrap();

        let stored = fake.turn(&account(), &turn_id).await.unwrap();
        assert!(stored.assistant.is_some());
        assert_eq!(
            fake.calls(),
            vec![("conversations.append_assistant_turn", 1)],
            "the accessor bypasses the counting layer"
        );
    }

    #[tokio::test]
    async fn a_named_boundary_fails_the_next_call_that_reaches_it() {
        let (fake, conversation, turn_id) = arranged().await;
        fake.fail_at_boundary("before_response_persistence", StoreError::Unavailable)
            .unwrap();
        assert_eq!(
            fake.armed_failures(),
            vec![FailurePoint::BeforeResponsePersistence]
        );

        let refused = fake
            .stores()
            .conversations()
            .append_assistant_turn(&account(), assistant(turn_id, conversation))
            .await;

        assert_eq!(refused, Err(StoreError::Unavailable));
        // The point is named `Before…`, so nothing was written.
        assert!(
            fake.turn(&account(), &turn_id)
                .await
                .unwrap()
                .assistant
                .is_none()
        );
        assert!(fake.armed_failures().is_empty());
    }

    #[test]
    fn the_boundary_table_covers_the_specification_and_round_trips() {
        assert_eq!(CRASH_BOUNDARIES.len(), FailurePoint::ALL.len());
        for point in FailurePoint::ALL {
            let name = boundary_name(point);
            assert_ne!(name, "unknown", "{point:?} is missing from the table");
            assert_eq!(crash_boundary(name), Some(point));
        }
        assert_eq!(crash_boundary("nope"), None);

        let fake = FakeStores::new();
        assert_eq!(
            fake.fail_at_boundary("nope", StoreError::Timeout)
                .unwrap_err(),
            UnknownBoundary {
                name: "nope".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn the_clock_only_moves_when_the_test_moves_it() {
        let (fake, _conversation, turn_id) = arranged().await;
        assert_eq!(fake.now(), DateTime::<Utc>::UNIX_EPOCH);
        let first = fake.turn_phase(&account(), &turn_id).await.unwrap();
        assert_eq!(first.updated_at, DateTime::<Utc>::UNIX_EPOCH);

        fake.advance(TimeDelta::minutes(5));
        fake.stores()
            .conversations()
            .set_turn_phase(&account(), &turn_id, TurnPhase::Committed)
            .await
            .unwrap();

        let second = fake.turn_phase(&account(), &turn_id).await.unwrap();
        assert_eq!(second.phase, TurnPhase::Committed);
        assert_eq!(
            second.updated_at,
            DateTime::<Utc>::UNIX_EPOCH + TimeDelta::minutes(5)
        );
        fake.set_time(DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(fake.now(), DateTime::<Utc>::UNIX_EPOCH);
    }

    #[tokio::test]
    async fn armed_failures_can_be_disarmed_again() {
        let fake = FakeStores::default();
        fake.fail_at(FailurePoint::BeforeJournalInsert, StoreError::Timeout)
            .unwrap();
        assert_eq!(fake.armed_failures().len(), 1);
        fake.clear_failures();
        assert!(fake.armed_failures().is_empty());
    }

    #[tokio::test]
    async fn an_empty_case_simply_has_no_events() {
        let fake = FakeStores::new();
        let case_key = CaseKey {
            workflow: turnframe_core::ids::WorkflowKey::from("trip"),
            case_id: turnframe_core::ids::CaseId::from("trip-1"),
        };
        assert!(
            fake.events_for(&account(), &case_key)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            fake.event_types(&account(), &case_key)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
