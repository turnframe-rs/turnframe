//! The trait implementations of [`MemoryStores`].
//!
//! Each method does the same three things: check whether a failure is armed at
//! the boundary it crosses, take the state lock for exactly one synchronous
//! rule call from [`super::state`], and release it. No `.await` happens while
//! the lock is held — there is no `.await` in this file at all — which is what
//! keeps the returned futures `Send`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use turnframe_core::case::CaseKey;
use turnframe_core::event::{EventRedaction, OutboxEntry};
use turnframe_core::ids::{
    AccountId, CaseRevision, CommandId, ConversationId, EventId, InteractionId, OptionId, OutboxId,
    RedactionAuthority, TurnId,
};
use turnframe_core::interaction::{Interaction, InteractionStatus};
use turnframe_core::replay::{ReplayRecord, TurnPhase};
use turnframe_core::response::AssistantTurn;

use super::MemoryStores;
use super::fault::FailurePoint;
use crate::commit::{CommitBundle, CommitReceipt, CommitStore};
use crate::conversation::{
    ConversationReader, ConversationRecord, ConversationWriter, RecoveryScope, StoredTurn,
    StoredUserTurn, TurnPhaseMarker,
};
use crate::error::StoreError;
use crate::events::{
    EventBatch, EventCursor, EventJournalReader, EventJournalWriter, EventPage, StoredEvent,
};
use crate::interaction::{
    InteractionReader, InteractionRecord, InteractionWriter, InvalidationReason, ResolutionOutcome,
};
use crate::journal::{
    CommandJournalEntry, CommandJournalReader, CommandJournalWriter, JournalAdmission,
    JournalOutcome,
};
use crate::outbox::{OutboxReader, OutboxRecord, OutboxWriter};
use crate::replay::{ReplayReader, ReplayWriter};

#[async_trait]
impl ConversationReader for MemoryStores {
    async fn load_conversation(
        &self,
        account: &AccountId,
        id: &ConversationId,
    ) -> Result<ConversationRecord, StoreError> {
        self.inner()?.load_conversation(account, id)
    }

    async fn load_recent_turns(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<StoredTurn>, StoreError> {
        self.inner()?
            .load_recent_turns(account, conversation, limit)
    }

    async fn load_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<StoredTurn, StoreError> {
        self.inner()?.load_turn(account, turn_id)
    }

    async fn turn_phase(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<TurnPhaseMarker, StoreError> {
        self.inner()?.turn_phase(account, turn_id)
    }

    async fn list_unfinished_turns(
        &self,
        scope: RecoveryScope,
        limit: usize,
    ) -> Result<Vec<TurnPhaseMarker>, StoreError> {
        Ok(self.inner()?.list_unfinished_turns(&scope, limit))
    }
}

#[async_trait]
impl ConversationWriter for MemoryStores {
    async fn create_conversation(&self, record: ConversationRecord) -> Result<(), StoreError> {
        self.inner()?.create_conversation(record)
    }

    async fn append_user_turn(&self, turn: StoredUserTurn) -> Result<(), StoreError> {
        let now = self.now();
        self.inner()?.append_user_turn(turn, now)
    }

    async fn append_assistant_turn(
        &self,
        account: &AccountId,
        turn: AssistantTurn,
    ) -> Result<(), StoreError> {
        self.fire(FailurePoint::BeforeResponsePersistence)?;
        self.inner()?.append_assistant_turn(account, turn)
    }

    async fn set_turn_phase(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
        phase: TurnPhase,
    ) -> Result<TurnPhaseMarker, StoreError> {
        let now = self.now();
        self.inner()?.set_turn_phase(account, turn_id, phase, now)
    }
}

#[async_trait]
impl InteractionReader for MemoryStores {
    async fn get(
        &self,
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, StoreError> {
        self.inner()?.get_interaction(account, id)
    }

    async fn list_open_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Vec<Interaction>, StoreError> {
        Ok(self
            .inner()?
            .list_open_for_conversation(account, conversation))
    }

    async fn list_open_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<Interaction>, StoreError> {
        Ok(self.inner()?.list_open_for_case(account, case_key))
    }

    async fn blocking_answered_at(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        revision: CaseRevision,
    ) -> Result<bool, StoreError> {
        Ok(self
            .inner()?
            .blocking_answered_at(account, case_key, revision))
    }
}

#[async_trait]
impl InteractionWriter for MemoryStores {
    async fn insert(&self, interaction: Interaction) -> Result<(), StoreError> {
        let now = self.now();
        self.inner()?.insert_interaction(interaction, false, now)?;
        self.fire(FailurePoint::AfterInteractionPersistence)
    }

    async fn insert_replacing_blocking(
        &self,
        interaction: Interaction,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let now = self.now();
        let invalidated = self.inner()?.insert_interaction(interaction, true, now)?;
        self.fire(FailurePoint::AfterInteractionPersistence)?;
        Ok(invalidated)
    }

    async fn begin_resolution(
        &self,
        account: &AccountId,
        id: &InteractionId,
        expected_status: InteractionStatus,
        option_id: OptionId,
        resolved_by: TurnId,
    ) -> Result<InteractionRecord, StoreError> {
        let now = self.now();
        self.inner()?
            .begin_resolution(account, id, expected_status, option_id, resolved_by, now)
    }

    async fn finish_resolution(
        &self,
        account: &AccountId,
        id: &InteractionId,
        outcome: ResolutionOutcome,
    ) -> Result<InteractionRecord, StoreError> {
        self.inner()?.finish_resolution(account, id, outcome)
    }

    async fn invalidate_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        new_revision: CaseRevision,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let now = self.now();
        self.inner()?
            .invalidate_for_case(account, case_key, new_revision, reason, now)
    }

    async fn invalidate_case_cards(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let now = self.now();
        self.inner()?
            .invalidate_case_cards(account, case_key, reason, now)
    }

    async fn expire_due(&self, now: DateTime<Utc>) -> Result<Vec<InteractionId>, StoreError> {
        self.inner()?.expire_due(now)
    }
}

#[async_trait]
impl CommandJournalReader for MemoryStores {
    async fn get(
        &self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<CommandJournalEntry, StoreError> {
        self.inner()?.journal_get(account, command_id)
    }

    async fn for_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError> {
        Ok(self.inner()?.journal_for_turn(account, turn_id, false))
    }

    async fn pending_for_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError> {
        Ok(self.inner()?.journal_for_turn(account, turn_id, true))
    }
}

#[async_trait]
impl CommandJournalWriter for MemoryStores {
    async fn begin(&self, entry: CommandJournalEntry) -> Result<JournalAdmission, StoreError> {
        self.fire(FailurePoint::BeforeJournalInsert)?;
        let admission = self.inner()?.journal_begin(entry)?;
        if admission.is_fresh() {
            self.fire(FailurePoint::AfterJournalInsertBeforeCommit)?;
        }
        Ok(admission)
    }

    async fn mark_executing(
        &self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<(), StoreError> {
        self.inner()?.journal_mark_executing(account, command_id)
    }

    async fn complete(
        &self,
        account: &AccountId,
        command_id: &CommandId,
        outcome: JournalOutcome,
    ) -> Result<(), StoreError> {
        let now = self.now();
        self.inner()?
            .journal_complete(account, command_id, outcome, now)
    }
}

#[async_trait]
impl EventJournalReader for MemoryStores {
    async fn list_since(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        since: CaseRevision,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        Ok(self
            .inner()?
            .list_events_since(account, case_key, since, limit))
    }

    async fn read_from(
        &self,
        account: &AccountId,
        after: EventCursor,
        limit: usize,
    ) -> Result<EventPage, StoreError> {
        Ok(self.inner()?.read_events_from(account, after, limit))
    }

    async fn get_by_ids(
        &self,
        account: &AccountId,
        ids: &[EventId],
    ) -> Result<Vec<StoredEvent>, StoreError> {
        Ok(self.inner()?.events_by_ids(account, ids))
    }

    async fn count(&self, account: &AccountId, case_key: &CaseKey) -> Result<u64, StoreError> {
        Ok(self.inner()?.count_events(account, case_key))
    }
}

#[async_trait]
impl EventJournalWriter for MemoryStores {
    async fn append(&self, batch: EventBatch) -> Result<Vec<EventId>, StoreError> {
        let appended = self.inner()?.append_events(batch)?;
        self.fire(FailurePoint::AfterCommitBeforeEventReadback)?;
        Ok(appended)
    }

    async fn redact_payload(
        &self,
        account: &AccountId,
        event_id: &EventId,
        authority: &RedactionAuthority,
    ) -> Result<EventRedaction, StoreError> {
        let now = self.now();
        self.inner()?
            .redact_event_payload(account, event_id, authority, now)
    }
}

#[async_trait]
impl OutboxReader for MemoryStores {
    async fn get(&self, outbox_id: &OutboxId) -> Result<OutboxRecord, StoreError> {
        self.inner()?.get_outbox(outbox_id)
    }

    async fn list_for_command(
        &self,
        command_id: &CommandId,
    ) -> Result<Vec<OutboxRecord>, StoreError> {
        Ok(self.inner()?.outbox_for_command(command_id))
    }
}

#[async_trait]
impl OutboxWriter for MemoryStores {
    async fn enqueue(&self, entry: OutboxEntry) -> Result<(), StoreError> {
        self.inner()?.enqueue_outbox(entry)
    }

    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        worker_id: &str,
    ) -> Result<Vec<OutboxEntry>, StoreError> {
        self.fire(FailurePoint::BeforeOutboxDispatch)?;
        Ok(self.inner()?.claim_due(now, limit, worker_id))
    }

    async fn mark_completed(&self, outbox_id: &OutboxId) -> Result<(), StoreError> {
        self.fire(FailurePoint::AfterOutboxDispatch)?;
        let now = self.now();
        self.inner()?.mark_outbox_completed(outbox_id, now)
    }

    async fn mark_failed(
        &self,
        outbox_id: &OutboxId,
        reason: String,
        retry_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError> {
        self.fire(FailurePoint::AfterOutboxDispatch)?;
        let now = self.now();
        self.inner()?
            .mark_outbox_failed(outbox_id, reason, retry_at, now)
    }

    async fn mark_outcome_unknown(
        &self,
        outbox_id: &OutboxId,
        remote_ref: Option<String>,
    ) -> Result<(), StoreError> {
        self.fire(FailurePoint::AfterOutboxDispatch)?;
        self.inner()?
            .mark_outbox_outcome_unknown(outbox_id, remote_ref)
    }

    async fn reschedule(
        &self,
        outbox_id: &OutboxId,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner()?.reschedule_outbox(outbox_id, next_attempt_at)
    }

    async fn release_expired_claims(
        &self,
        claimed_before: DateTime<Utc>,
    ) -> Result<Vec<OutboxId>, StoreError> {
        Ok(self.inner()?.release_expired_claims(claimed_before))
    }
}

#[async_trait]
impl ReplayReader for MemoryStores {
    async fn get(&self, account: &AccountId, turn_id: &TurnId) -> Result<ReplayRecord, StoreError> {
        self.inner()?.get_replay(account, turn_id)
    }

    async fn list_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<ReplayRecord>, StoreError> {
        Ok(self
            .inner()?
            .replays_for_conversation(account, conversation, limit))
    }
}

#[async_trait]
impl ReplayWriter for MemoryStores {
    async fn put(&self, record: ReplayRecord) -> Result<(), StoreError> {
        self.inner()?.put_replay(record);
        Ok(())
    }
}

#[async_trait]
impl CommitStore for MemoryStores {
    async fn commit(
        &self,
        account: &AccountId,
        bundle: CommitBundle,
    ) -> Result<CommitReceipt, StoreError> {
        let now = self.now();
        let faults = &self.faults;
        let mut probe = |point: FailurePoint| match faults.lock() {
            Ok(mut queue) => queue.take(point),
            Err(_) => Some(StoreError::Corrupt),
        };
        // The bundle mutates the state directly and records how to undo each
        // mutation; an error rolls the recorded reversals back before returning,
        // so nothing of a failed bundle is ever visible. That is the
        // all-or-nothing of spec §16.3; see `Inner::apply_bundle` for why it is
        // done this way rather than by copying the state.
        self.inner()?.apply_bundle(account, bundle, now, &mut probe)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use turnframe_core::case::CaseRef;
    use turnframe_core::command::{CommandOrigin, IdempotencyKey};
    use turnframe_core::event::CommittedEvent;
    use turnframe_core::ids::{BlockId, ConversationId, UserId};
    use turnframe_core::interaction::{
        InteractionKind, InteractionOption, InteractionPayload, InteractionSpec,
        StoredInteractionAction,
    };
    use turnframe_core::locale::Locale;
    use turnframe_core::response::{NoticeSeverity, ReplayToken, ResponseBlock, ServerNotice};
    use turnframe_core::turn::{ActorContext, TurnInput};

    use super::*;
    use crate::journal::{CommandJournalStatus, JournalAdmission};
    use crate::memory::{ManualClock, MemoryStores};

    fn account() -> AccountId {
        AccountId::from("chaos")
    }

    fn epoch() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH
    }

    fn case() -> CaseRef {
        CaseRef::new("chaos", "case-1", CaseRevision(1))
    }

    fn stores() -> (Arc<MemoryStores>, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::epoch());
        (Arc::new(MemoryStores::with_clock(clock.clone())), clock)
    }

    fn card(id: InteractionId, blocking: bool) -> Interaction {
        let mut spec = InteractionSpec::new(
            "chaos",
            case(),
            InteractionKind::SingleSelect,
            InteractionPayload::new("chaos card").with_option(InteractionOption::new(
                "ack",
                "Got it",
                StoredInteractionAction::Dismiss,
            )),
        );
        if !blocking {
            spec = spec.non_blocking();
        }
        Interaction::from_spec(
            spec,
            id,
            account(),
            ConversationId::nil(),
            TurnId::nil(),
            epoch(),
        )
        .expect("a valid fixture card")
    }

    fn entry(command_id: CommandId, turn: TurnId, key: &str) -> CommandJournalEntry {
        CommandJournalEntry {
            command_id,
            account_id: account(),
            idempotency_key: IdempotencyKey::new(key),
            turn_id: turn,
            case_ref: case(),
            command_type: "chaos.noop".to_owned(),
            command_payload: serde_json::json!({ "key": key }),
            origin: CommandOrigin::InternalPolicy {
                policy_key: "chaos".to_owned(),
            },
            status: CommandJournalStatus::Pending,
            result: None,
            created_at: epoch(),
            completed_at: None,
        }
    }

    fn batch(command_id: CommandId, ids: &[EventId]) -> EventBatch {
        EventBatch::new(
            account(),
            case().key(),
            command_id,
            CaseRevision(2),
            ids.iter()
                .map(|id| CommittedEvent {
                    event_id: *id,
                    event_type: "chaos.happened".to_owned(),
                    occurred_at: epoch(),
                    payload: serde_json::Value::Null,
                })
                .collect(),
        )
    }

    async fn conversation_with_turn(store: &MemoryStores, turn: TurnId) -> ConversationId {
        let conversation = ConversationId::new();
        store
            .create_conversation(ConversationRecord::new(conversation, account(), epoch()))
            .await
            .expect("the conversation is created");
        store
            .append_user_turn(StoredUserTurn::new(
                TurnInput {
                    turn_id: turn,
                    conversation_id: conversation,
                    actor: ActorContext::new(account(), UserId::from("u")),
                    text: Some("hello".to_owned()),
                    interaction_response: None,
                    attachments: Vec::new(),
                    origin: None,
                    locale: Locale::from("en"),
                    effort: None,
                },
                epoch(),
            ))
            .await
            .expect("the turn is appended");
        conversation
    }

    #[tokio::test]
    async fn a_failure_after_interaction_persistence_leaves_the_card_written() {
        let (store, _clock) = stores();
        let id = InteractionId::new();
        store
            .fail_next(
                FailurePoint::AfterInteractionPersistence,
                StoreError::Unavailable,
            )
            .unwrap();
        assert_eq!(
            InteractionWriter::insert(store.as_ref(), card(id, true)).await,
            Err(StoreError::Unavailable)
        );
        // Spec §15.5: the response must not mention a card whose persistence
        // was reported as failed — but the row is there, and recovery sees it.
        let record = InteractionReader::get(store.as_ref(), &account(), &id)
            .await
            .expect("the card survived the failure");
        assert_eq!(record.status(), InteractionStatus::Active);
        assert!(store.armed_failures().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failure_before_the_journal_insert_leaves_the_key_free() {
        let (store, _clock) = stores();
        let turn = TurnId::new();
        let command = CommandId::new();
        store
            .fail_next(FailurePoint::BeforeJournalInsert, StoreError::Unavailable)
            .unwrap();
        assert_eq!(
            CommandJournalWriter::begin(store.as_ref(), entry(command, turn, "k")).await,
            Err(StoreError::Unavailable)
        );
        assert_eq!(
            CommandJournalReader::get(store.as_ref(), &account(), &command).await,
            Err(StoreError::NotFound)
        );
        // Nothing was written, so the command may be admitted from scratch.
        assert_eq!(
            CommandJournalWriter::begin(store.as_ref(), entry(command, turn, "k")).await,
            Ok(JournalAdmission::Fresh)
        );
    }

    #[tokio::test]
    async fn a_failure_after_the_journal_insert_leaves_a_pending_entry_for_recovery() {
        let (store, _clock) = stores();
        let turn = TurnId::new();
        let command = CommandId::new();
        store
            .fail_next(
                FailurePoint::AfterJournalInsertBeforeCommit,
                StoreError::Timeout,
            )
            .unwrap();
        assert_eq!(
            CommandJournalWriter::begin(store.as_ref(), entry(command, turn, "k")).await,
            Err(StoreError::Timeout)
        );
        let pending = CommandJournalReader::pending_for_turn(store.as_ref(), &account(), &turn)
            .await
            .expect("recovery lists the turn");
        assert_eq!(pending.len(), 1, "spec §23.1: resume by idempotency key");
        assert_eq!(pending[0].command_id, command);
        // And the key is now taken, so a retry replays instead of duplicating.
        let admission =
            CommandJournalWriter::begin(store.as_ref(), entry(CommandId::new(), turn, "k"))
                .await
                .expect("the retry is admitted");
        assert_eq!(
            admission.replayed().map(|entry| entry.command_id),
            Some(command)
        );
    }

    #[tokio::test]
    async fn a_failure_after_the_event_append_leaves_the_events_readable() {
        let (store, _clock) = stores();
        let command = CommandId::new();
        let ids = vec![EventId::new()];
        store
            .fail_next(
                FailurePoint::AfterCommitBeforeEventReadback,
                StoreError::Timeout,
            )
            .unwrap();
        assert_eq!(
            EventJournalWriter::append(store.as_ref(), batch(command, &ids)).await,
            Err(StoreError::Timeout)
        );
        let readback = EventJournalReader::get_by_ids(store.as_ref(), &account(), &ids)
            .await
            .expect("the ledger answers");
        assert_eq!(readback.len(), 1, "the events landed before the failure");
        // Appending them again must not duplicate the claim ledger.
        assert_eq!(
            EventJournalWriter::append(store.as_ref(), batch(command, &ids)).await,
            Err(StoreError::Conflict)
        );
    }

    #[tokio::test]
    async fn outbox_failures_bracket_the_dispatch() {
        let (store, _clock) = stores();
        let outbox_id = OutboxId::new();
        let entry = OutboxEntry {
            outbox_id,
            command_id: CommandId::new(),
            destination: "chaos".to_owned(),
            payload: serde_json::Value::Null,
            idempotency_key: IdempotencyKey::new("k"),
            status: turnframe_core::event::OutboxStatus::Pending,
            attempt_count: 0,
            next_attempt_at: None,
            created_at: epoch(),
            completed_at: None,
        };
        OutboxWriter::enqueue(store.as_ref(), entry)
            .await
            .expect("the row is enqueued");

        store
            .fail_next(FailurePoint::BeforeOutboxDispatch, StoreError::Unavailable)
            .unwrap();
        assert_eq!(
            OutboxWriter::claim_due(store.as_ref(), epoch(), 10, "w").await,
            Err(StoreError::Unavailable)
        );
        let untouched = OutboxReader::get(store.as_ref(), &outbox_id)
            .await
            .expect("the row is readable");
        assert_eq!(untouched.entry.attempt_count, 0, "nothing was claimed");

        let claimed = OutboxWriter::claim_due(store.as_ref(), epoch(), 10, "w")
            .await
            .expect("the row is claimed");
        assert_eq!(claimed.len(), 1);
        store
            .fail_next(FailurePoint::AfterOutboxDispatch, StoreError::Timeout)
            .unwrap();
        assert_eq!(
            OutboxWriter::mark_completed(store.as_ref(), &outbox_id).await,
            Err(StoreError::Timeout)
        );
        // The worst case of spec §16.5: the call may have happened and nothing
        // local says so. Only the reaper gets the row moving again.
        let stranded = OutboxReader::get(store.as_ref(), &outbox_id)
            .await
            .expect("the row is readable");
        assert_eq!(
            stranded.entry.status,
            turnframe_core::event::OutboxStatus::Dispatching
        );
        assert!(stranded.claim.is_some());
        let released = OutboxWriter::release_expired_claims(
            store.as_ref(),
            epoch() + chrono::TimeDelta::minutes(5),
        )
        .await
        .expect("the reaper runs");
        assert_eq!(released, vec![outbox_id]);
    }

    #[tokio::test]
    async fn a_failure_before_response_persistence_keeps_the_turn_answerable() {
        let (store, _clock) = stores();
        let turn = TurnId::new();
        let conversation = conversation_with_turn(store.as_ref(), turn).await;
        let assistant = AssistantTurn {
            turn_id: turn,
            conversation_id: conversation,
            blocks: vec![ResponseBlock::Notice(ServerNotice {
                block_id: BlockId::from("b1"),
                code: "turnframe.notice.chaos".to_owned(),
                severity: NoticeSeverity::Info,
                text: "hello".into(),
            })],
            subjects: Vec::new(),
            expectations: Vec::new(),
            replay_token: ReplayToken::new("t"),
            done: Vec::new(),
            offers: Vec::new(),
        };
        store
            .fail_next(
                FailurePoint::BeforeResponsePersistence,
                StoreError::Unavailable,
            )
            .unwrap();
        assert_eq!(
            ConversationWriter::append_assistant_turn(
                store.as_ref(),
                &account(),
                assistant.clone()
            )
            .await,
            Err(StoreError::Unavailable)
        );
        let stored = ConversationReader::load_turn(store.as_ref(), &account(), &turn)
            .await
            .expect("the turn is still there");
        assert!(stored.assistant.is_none(), "nothing was written");
        // Recovery regenerates and persists it; the slot is still free.
        ConversationWriter::append_assistant_turn(store.as_ref(), &account(), assistant.clone())
            .await
            .expect("the retry succeeds");
        let recovered = ConversationReader::load_turn(store.as_ref(), &account(), &turn)
            .await
            .expect("the turn is readable");
        assert_eq!(recovered.assistant, Some(assistant));
    }

    #[tokio::test]
    async fn a_failure_mid_bundle_makes_nothing_of_it_visible() {
        for point in [
            FailurePoint::AfterJournalInsertBeforeCommit,
            FailurePoint::AfterCommitBeforeEventReadback,
            FailurePoint::AfterInteractionPersistence,
        ] {
            let (store, _clock) = stores();
            let turn = TurnId::new();
            let conversation = conversation_with_turn(store.as_ref(), turn).await;
            let command = CommandId::new();
            CommandJournalWriter::begin(store.as_ref(), entry(command, turn, "k"))
                .await
                .expect("the command is admitted");
            let event_ids = vec![EventId::new()];
            let inserted = InteractionId::new();
            let bundle = CommitBundle::new()
                .with_journal_completion(
                    command,
                    JournalOutcome::Committed {
                        new_revision: CaseRevision(2),
                        event_ids: event_ids.clone(),
                    },
                )
                .with_events(batch(command, &event_ids))
                .with_interaction_insert(card(inserted, true), false)
                .with_replay_record(turnframe_core::replay::ReplayRecord::received(
                    turn,
                    conversation,
                    account(),
                    epoch(),
                ))
                .with_turn_phase(turn, TurnPhase::Committed);

            store.fail_next(point, StoreError::Unavailable).unwrap();
            assert_eq!(
                CommitStore::commit(store.as_ref(), &account(), bundle).await,
                Err(StoreError::Unavailable),
                "{point:?}"
            );

            // Not one stage of the bundle may have survived, including the
            // stages that ran before the injected failure.
            let entry = CommandJournalReader::get(store.as_ref(), &account(), &command)
                .await
                .expect("the entry is readable");
            assert_eq!(entry.status, CommandJournalStatus::Pending, "{point:?}");
            assert!(entry.result.is_none(), "{point:?}");
            assert!(
                EventJournalReader::get_by_ids(store.as_ref(), &account(), &event_ids)
                    .await
                    .expect("the ledger answers")
                    .is_empty(),
                "{point:?}"
            );
            assert_eq!(
                InteractionReader::get(store.as_ref(), &account(), &inserted).await,
                Err(StoreError::NotFound),
                "{point:?}"
            );
            assert_eq!(
                ReplayReader::get(store.as_ref(), &account(), &turn).await,
                Err(StoreError::NotFound),
                "{point:?}"
            );
            assert_eq!(
                ConversationReader::turn_phase(store.as_ref(), &account(), &turn)
                    .await
                    .expect("the marker is readable")
                    .phase,
                TurnPhase::Received,
                "{point:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_clock_stamps_what_the_store_owns() {
        let (store, clock) = stores();
        let turn = TurnId::new();
        conversation_with_turn(store.as_ref(), turn).await;
        clock.advance(chrono::TimeDelta::seconds(42));
        let marker = ConversationWriter::set_turn_phase(
            store.as_ref(),
            &account(),
            &turn,
            TurnPhase::Reduced,
        )
        .await
        .expect("the phase moves");
        assert_eq!(marker.updated_at, epoch() + chrono::TimeDelta::seconds(42));
    }

    #[tokio::test]
    async fn non_blocking_cards_never_take_the_blocking_slot() {
        let (store, _clock) = stores();
        for _ in 0..3 {
            InteractionWriter::insert(store.as_ref(), card(InteractionId::new(), false))
                .await
                .expect("a non-blocking card always fits");
        }
        InteractionWriter::insert(store.as_ref(), card(InteractionId::new(), true))
            .await
            .expect("the slot is still free");
        assert_eq!(
            InteractionWriter::insert(store.as_ref(), card(InteractionId::new(), true)).await,
            Err(StoreError::Conflict)
        );
    }
}
