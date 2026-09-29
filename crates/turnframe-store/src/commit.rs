//! The atomicity model: one [`CommitBundle`], one all-or-nothing write
//! (spec §16.3, §23 step N, ADR-006 point 8).
//!
//! After the executor has committed the domain state, the runtime still has the
//! journal outcomes, the committed events, the card resolutions, the cards the
//! new state requires and the ones the new revision invalidates, the outbox
//! rows, the replay record and the phase marker to write. A partial write across
//! those is an invariant violation, not a degraded success.
//!
//! So [`CommitStore::commit`] takes the whole bundle and applies it all or
//! nothing, and "nothing" covers the records a bundle *changes*, not only the
//! ones it creates — which is what
//! [`crate::conformance::check_commit_bundle_restores_modified_records`] holds
//! an implementation to.
//!
//! Why the executor's own commit is deliberately outside that transaction, and
//! what makes the seam safe anyway, is in
//! [`docs/persistence.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/persistence.md).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use turnframe_core::case::CaseKey;
use turnframe_core::event::OutboxEntry;
use turnframe_core::ids::{AccountId, CaseRevision, CommandId, EventId, InteractionId, TurnId};
use turnframe_core::interaction::Interaction;
use turnframe_core::replay::{ReplayRecord, TurnPhase};

use crate::error::{StoreError, bundle_account_mismatch, invalid_record};
use crate::events::EventBatch;
use crate::interaction::{InvalidationReason, ResolutionOutcome};
use crate::journal::JournalOutcome;

/// Records the outcome of one journaled command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalCompletion {
    /// The command.
    pub command_id: CommandId,
    /// Its outcome.
    pub outcome: JournalOutcome,
}

/// Settles one `Resolving` interaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionFinish {
    /// The interaction.
    pub interaction_id: InteractionId,
    /// How it is settled.
    pub outcome: ResolutionOutcome,
}

/// Inserts one new interaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionInsert {
    /// The interaction, in status `Active`.
    pub interaction: Interaction,
    /// `true` to invalidate an `Active` blocking occupant of the same case
    /// (see `InteractionWriter::insert_replacing_blocking`); `false` to fail with
    /// `Conflict` when the slot is taken.
    pub replace_blocking: bool,
}

/// Invalidates the revision-bound interactions of one case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseInvalidation {
    /// The case.
    pub case_key: CaseKey,
    /// The revision the case moved to.
    pub new_revision: CaseRevision,
    /// The reason recorded on every invalidated interaction.
    pub reason: InvalidationReason,
}

/// Writes the phase marker of a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnPhaseUpdate {
    /// The turn.
    pub turn_id: TurnId,
    /// The new phase.
    pub phase: TurnPhase,
}

/// Everything one commit writes, applied all or nothing.
///
/// Compared with [`PartialEq`] only, because the replay record it may carry
/// holds a sampling temperature and therefore a float.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CommitBundle {
    /// Journal outcomes to record.
    #[serde(default)]
    pub journal_completions: Vec<JournalCompletion>,
    /// Event batches to append.
    #[serde(default)]
    pub events: Vec<EventBatch>,
    /// Interactions to settle.
    #[serde(default)]
    pub interaction_finishes: Vec<InteractionFinish>,
    /// Cases whose revision-bound interactions are invalidated.
    #[serde(default)]
    pub interaction_invalidations: Vec<CaseInvalidation>,
    /// Interactions to insert.
    #[serde(default)]
    pub interaction_inserts: Vec<InteractionInsert>,
    /// Outbox rows to enqueue.
    #[serde(default)]
    pub outbox_entries: Vec<OutboxEntry>,
    /// Replay record to upsert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_record: Option<ReplayRecord>,
    /// Turn phase to write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_phase: Option<TurnPhaseUpdate>,
}

impl CommitBundle {
    /// An empty bundle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a journal completion.
    #[must_use]
    pub fn with_journal_completion(
        mut self,
        command_id: CommandId,
        outcome: JournalOutcome,
    ) -> Self {
        self.journal_completions.push(JournalCompletion {
            command_id,
            outcome,
        });
        self
    }

    /// Adds an event batch.
    #[must_use]
    pub fn with_events(mut self, batch: EventBatch) -> Self {
        self.events.push(batch);
        self
    }

    /// Adds an interaction finish.
    #[must_use]
    pub fn with_interaction_finish(
        mut self,
        interaction_id: InteractionId,
        outcome: ResolutionOutcome,
    ) -> Self {
        self.interaction_finishes.push(InteractionFinish {
            interaction_id,
            outcome,
        });
        self
    }

    /// Adds a case invalidation.
    #[must_use]
    pub fn with_invalidation(
        mut self,
        case_key: CaseKey,
        new_revision: CaseRevision,
        reason: InvalidationReason,
    ) -> Self {
        self.interaction_invalidations.push(CaseInvalidation {
            case_key,
            new_revision,
            reason,
        });
        self
    }

    /// Adds an interaction insert.
    #[must_use]
    pub fn with_interaction_insert(
        mut self,
        interaction: Interaction,
        replace_blocking: bool,
    ) -> Self {
        self.interaction_inserts.push(InteractionInsert {
            interaction,
            replace_blocking,
        });
        self
    }

    /// Adds an outbox entry.
    #[must_use]
    pub fn with_outbox_entry(mut self, entry: OutboxEntry) -> Self {
        self.outbox_entries.push(entry);
        self
    }

    /// Sets the replay record.
    #[must_use]
    pub fn with_replay_record(mut self, record: ReplayRecord) -> Self {
        self.replay_record = Some(record);
        self
    }

    /// Sets the turn phase update.
    #[must_use]
    pub fn with_turn_phase(mut self, turn_id: TurnId, phase: TurnPhase) -> Self {
        self.turn_phase = Some(TurnPhaseUpdate { turn_id, phase });
        self
    }

    /// Returns `true` when the bundle writes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.journal_completions.is_empty()
            && self.events.is_empty()
            && self.interaction_finishes.is_empty()
            && self.interaction_invalidations.is_empty()
            && self.interaction_inserts.is_empty()
            && self.outbox_entries.is_empty()
            && self.replay_record.is_none()
            && self.turn_phase.is_none()
    }

    /// Checks the bundle before anything is written: every account-bearing item
    /// must belong to `account`, and no event batch may be empty.
    ///
    /// Implementations call this first; adopters writing their own store should
    /// too, so a foreign item is refused identically everywhere.
    ///
    /// # Errors
    /// * `Other(BUNDLE_ACCOUNT_MISMATCH)` for a foreign item.
    /// * `Other(INVALID_RECORD)` for an empty event batch.
    pub fn validate(&self, account: &AccountId) -> Result<(), StoreError> {
        for batch in &self.events {
            if &batch.account_id != account {
                return Err(bundle_account_mismatch());
            }
            if batch.is_empty() {
                return Err(invalid_record());
            }
        }
        if self
            .interaction_inserts
            .iter()
            .any(|insert| &insert.interaction.account_id != account)
        {
            return Err(bundle_account_mismatch());
        }
        if self
            .replay_record
            .as_ref()
            .is_some_and(|record| &record.account_id != account)
        {
            return Err(bundle_account_mismatch());
        }
        Ok(())
    }
}

/// What a successful commit reports back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReceipt {
    /// Every event appended, in append order.
    pub event_ids: Vec<EventId>,
    /// Interactions inserted, in bundle order.
    pub inserted_interactions: Vec<InteractionId>,
    /// Interactions invalidated by invalidations and replacing inserts, in
    /// application order.
    pub invalidated_interactions: Vec<InteractionId>,
    /// When the commit was written, stamped by the store's clock.
    pub committed_at: DateTime<Utc>,
}

/// The all-or-nothing write of a [`CommitBundle`].
///
/// See the module documentation for the atomicity model. The conformance suite
/// proves the contract with [`crate::conformance::check_commit_bundle_applies_all`],
/// [`crate::conformance::check_commit_bundle_atomic_on_invalid_item`] and
/// [`crate::conformance::check_commit_bundle_rejects_foreign_account_items`].
///
/// This is the one persistence trait with no `…Reader` / `…Writer` split,
/// because it is entirely the write half: a commit *is* the write. It is
/// therefore absent from
/// [`ReadOnlyStores`](crate::stores::ReadOnlyStores) altogether, which is the
/// point — a path that only holds the read half cannot name it.
#[async_trait]
pub trait CommitStore: Send + Sync {
    /// Applies the bundle for `account` atomically.
    ///
    /// # Errors
    /// Any error an item would raise through its own store trait, plus the
    /// refusals of [`CommitBundle::validate`]. On error nothing of the bundle
    /// is visible.
    async fn commit(
        &self,
        account: &AccountId,
        bundle: CommitBundle,
    ) -> Result<CommitReceipt, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{codes, has_code};
    use turnframe_core::ids::ConversationId;

    #[test]
    fn empty_bundle_is_empty_and_valid() {
        let bundle = CommitBundle::new();
        assert!(bundle.is_empty());
        assert!(bundle.validate(&AccountId::from("a")).is_ok());
        let with_phase = bundle.with_turn_phase(TurnId::nil(), TurnPhase::Committed);
        assert!(!with_phase.is_empty());
    }

    #[test]
    fn validate_refuses_foreign_and_empty_items() {
        let account = AccountId::from("a");
        let foreign = CommitBundle::new().with_events(EventBatch::new(
            AccountId::from("b"),
            CaseKey::new("w", "c"),
            CommandId::nil(),
            CaseRevision(1),
            vec![],
        ));
        assert!(has_code(
            &foreign.validate(&account).unwrap_err(),
            codes::BUNDLE_ACCOUNT_MISMATCH
        ));
        let empty = CommitBundle::new().with_events(EventBatch::new(
            account.clone(),
            CaseKey::new("w", "c"),
            CommandId::nil(),
            CaseRevision(1),
            vec![],
        ));
        assert!(has_code(
            &empty.validate(&account).unwrap_err(),
            codes::INVALID_RECORD
        ));
        let replay = CommitBundle::new().with_replay_record(ReplayRecord::received(
            TurnId::nil(),
            ConversationId::nil(),
            AccountId::from("b"),
            DateTime::<Utc>::UNIX_EPOCH,
        ));
        assert!(has_code(
            &replay.validate(&account).unwrap_err(),
            codes::BUNDLE_ACCOUNT_MISMATCH
        ));
    }
}
