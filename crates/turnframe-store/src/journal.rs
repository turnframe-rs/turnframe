//! The command journal: idempotency admission and persisted outcomes
//! (spec §16.2, §23.1, I14).
//!
//! # Contract
//!
//! * `(account_id, idempotency_key)` is unique. [`CommandJournalWriter::begin`] is the
//!   single admission point: the first call for a key persists the entry and
//!   answers [`JournalAdmission::Fresh`]; every later call for the same key, in
//!   any status, answers [`JournalAdmission::Replay`] carrying the persisted
//!   entry and therefore the persisted outcome. There is never a second
//!   `Fresh` for one key, whatever the interleaving.
//! * A `Replay` may carry an entry whose command differs from the caller's
//!   (the key was reused for another command). The store does not judge that;
//!   the runtime compares with [`CommandJournalEntry::same_command`] and raises
//!   `ExecutionError::IdempotencyMismatch`.
//! * Statuses move along [`CommandJournalStatus::can_transition`]:
//!   `Pending → Executing`, then to `Committed`, `Failed` or `OutcomeUnknown`;
//!   `OutcomeUnknown` is settled by reconciliation to `Committed` or `Failed`.
//!   Re-recording the same terminal outcome is accepted; a different one is a
//!   `Conflict`.
//! * Crash recovery reads [`CommandJournalReader::for_turn`] and
//!   [`CommandJournalReader::pending_for_turn`]: entries in `Pending` or `Executing`
//!   are resumed by idempotency key, `Committed` ones are not re-executed,
//!   `OutcomeUnknown` ones are reconciled (spec §23.1).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use turnframe_core::case::CaseRef;
use turnframe_core::command::{CommandEnvelope, CommandOrigin, IdempotencyKey};
use turnframe_core::error::{DomainRejection, ExecutionError};
use turnframe_core::ids::{AccountId, AttemptId, CaseRevision, CommandId, EventId, TurnId};

use crate::error::StoreError;

/// Lifecycle status of a journal entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandJournalStatus {
    /// Admitted, not yet handed to the executor.
    Pending,
    /// Journaled for a confirmation card; runs only once that card is
    /// confirmed, and crash recovery never resumes it.
    AwaitingConfirmation,
    /// Handed to the executor.
    Executing,
    /// The executor committed; the outcome is `Committed`.
    Committed,
    /// The command ended without effect or with a definite failure.
    Failed,
    /// An effect may exist and its result is unknown; reconcile, never retry
    /// blindly (I15).
    OutcomeUnknown,
}

impl CommandJournalStatus {
    /// Every status, in declaration order.
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::AwaitingConfirmation,
        Self::Executing,
        Self::Committed,
        Self::Failed,
        Self::OutcomeUnknown,
    ];

    /// Returns `true` for `Committed` and `Failed`.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::Failed)
    }

    /// Returns `true` for `Pending` and `Executing`: the entry must be resumed
    /// after a crash.
    #[must_use]
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Pending | Self::Executing)
    }

    /// Legal transitions.
    #[must_use]
    pub fn can_transition(from: Self, to: Self) -> bool {
        matches!(
            (from, to),
            (Self::Pending | Self::AwaitingConfirmation, Self::Executing)
                | (
                    Self::Pending | Self::AwaitingConfirmation | Self::Executing,
                    Self::Committed | Self::Failed | Self::OutcomeUnknown
                )
                | (Self::OutcomeUnknown, Self::Committed)
                | (Self::OutcomeUnknown, Self::Failed)
        )
    }
}

/// The persisted outcome of a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum JournalOutcome {
    /// The executor committed.
    Committed {
        /// Revision after the commit.
        new_revision: CaseRevision,
        /// Events produced.
        event_ids: Vec<EventId>,
    },
    /// The domain refused.
    Rejected {
        /// The rejection.
        rejection: DomainRejection,
    },
    /// The expected revision was stale.
    RevisionConflict {
        /// Revision found.
        current_revision: CaseRevision,
    },
    /// Execution failed with a stable code and no effect.
    Failed {
        /// Stable code.
        code: String,
    },
    /// An effect may exist; its result is unknown (I15).
    OutcomeUnknown {
        /// Attempt identifier for reconciliation.
        attempt_id: AttemptId,
        /// Remote reference, when the remote returned one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_ref: Option<String>,
        /// Stable reason code.
        reason: String,
    },
}

impl JournalOutcome {
    /// The status an entry takes when this outcome is recorded.
    #[must_use]
    pub fn status(&self) -> CommandJournalStatus {
        match self {
            Self::Committed { .. } => CommandJournalStatus::Committed,
            Self::Rejected { .. } | Self::RevisionConflict { .. } | Self::Failed { .. } => {
                CommandJournalStatus::Failed
            }
            Self::OutcomeUnknown { .. } => CommandJournalStatus::OutcomeUnknown,
        }
    }

    /// Maps an execution error to the outcome the journal records for it.
    ///
    /// `Timeout` and `OutcomeUnknown` become [`JournalOutcome::OutcomeUnknown`]
    /// because an effect may exist; every other variant becomes a definite
    /// outcome without effect.
    #[must_use]
    pub fn from_execution_error(command_id: CommandId, error: &ExecutionError) -> Self {
        match error {
            ExecutionError::RevisionConflict(conflict) => Self::RevisionConflict {
                current_revision: conflict.current_revision,
            },
            ExecutionError::Rejected(rejection) => Self::Rejected {
                rejection: rejection.clone(),
            },
            ExecutionError::OutcomeUnknown(unknown) => Self::OutcomeUnknown {
                attempt_id: unknown.attempt_id.clone(),
                remote_ref: unknown.remote_ref.clone(),
                reason: unknown.reason.clone(),
            },
            ExecutionError::Timeout => Self::OutcomeUnknown {
                attempt_id: AttemptId::new(command_id.to_string()),
                remote_ref: None,
                reason: "execution_timeout".to_owned(),
            },
            ExecutionError::Store(store) => Self::Failed {
                code: format!("store.{}", store_code(store)),
            },
            ExecutionError::IdempotencyMismatch { .. } => Self::Failed {
                code: "idempotency_mismatch".to_owned(),
            },
            ExecutionError::ScopeViolation => Self::Failed {
                code: "scope_violation".to_owned(),
            },
            ExecutionError::Erasure(_) => Self::Failed {
                code: "erasure".to_owned(),
            },
            ExecutionError::Other { code } => Self::Failed { code: code.clone() },
            _ => Self::Failed {
                code: "other".to_owned(),
            },
        }
    }
}

fn store_code(error: &StoreError) -> &str {
    match error {
        StoreError::NotFound => "not_found",
        StoreError::Conflict => "conflict",
        StoreError::Unavailable => "unavailable",
        StoreError::Timeout => "timeout",
        StoreError::Serialization => "serialization",
        StoreError::Corrupt => "corrupt",
        StoreError::Other { code } => code,
        _ => "other",
    }
}

/// One row of the command journal (spec §22.2, `tf_command_journal`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandJournalEntry {
    /// The command.
    pub command_id: CommandId,
    /// Owning tenant.
    pub account_id: AccountId,
    /// Idempotency key, unique per account (I14).
    pub idempotency_key: IdempotencyKey,
    /// Turn that produced the command.
    pub turn_id: TurnId,
    /// Target case and expected revision.
    pub case_ref: CaseRef,
    /// Stable command type label (e.g. `"trip.add_extra"`).
    pub command_type: String,
    /// The command payload as JSON.
    pub command_payload: serde_json::Value,
    /// Authorizing origin.
    pub origin: CommandOrigin,
    /// Current status.
    pub status: CommandJournalStatus,
    /// Persisted outcome, once recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JournalOutcome>,
    /// Admission time, supplied by the caller.
    pub created_at: DateTime<Utc>,
    /// When the outcome was recorded, stamped by the store's clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

impl CommandJournalEntry {
    /// Builds a `Pending` entry from a typed envelope, serializing the command.
    ///
    /// # Errors
    /// * `Serialization` when the command cannot be rendered as JSON.
    pub fn from_envelope<C: Serialize>(
        envelope: &CommandEnvelope<C>,
        command_type: impl Into<String>,
        created_at: DateTime<Utc>,
    ) -> Result<Self, StoreError> {
        let command_payload =
            serde_json::to_value(&envelope.command).map_err(|_| StoreError::Serialization)?;
        Ok(Self {
            command_id: envelope.command_id,
            account_id: envelope.actor.account_id.clone(),
            idempotency_key: envelope.idempotency_key.clone(),
            turn_id: envelope.turn_id,
            case_ref: envelope.case_ref.clone(),
            command_type: command_type.into(),
            command_payload,
            origin: envelope.origin.clone(),
            status: CommandJournalStatus::Pending,
            result: None,
            created_at,
            completed_at: None,
        })
    }

    /// Returns `true` when both entries describe the same command: same case
    /// reference, type and payload. Used to detect an idempotency key reused
    /// for a different command.
    #[must_use]
    pub fn same_command(&self, other: &Self) -> bool {
        self.case_ref == other.case_ref
            && self.command_type == other.command_type
            && self.command_payload == other.command_payload
    }
}

/// Result of [`CommandJournalWriter::begin`].
///
/// The entry is boxed because the two answers are wildly different in size and
/// this value is returned from every admission, including the overwhelmingly
/// common `Fresh` one; build it with [`JournalAdmission::replay`] rather than
/// writing the `Box` yourself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalAdmission {
    /// The key was never seen for this account; the entry is now persisted.
    Fresh,
    /// The key exists; the persisted entry (and its outcome, if any) is
    /// returned instead of admitting a second command (spec §16.2).
    Replay(Box<CommandJournalEntry>),
}

impl JournalAdmission {
    /// Builds a [`Self::Replay`] from the persisted entry.
    #[must_use]
    pub fn replay(entry: CommandJournalEntry) -> Self {
        Self::Replay(Box::new(entry))
    }

    /// Returns `true` for [`Self::Fresh`].
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh)
    }

    /// The replayed entry, if any.
    #[must_use]
    pub fn replayed(&self) -> Option<&CommandJournalEntry> {
        match self {
            Self::Fresh => None,
            Self::Replay(entry) => Some(entry),
        }
    }

    /// Takes ownership of the replayed entry, if any.
    #[must_use]
    pub fn into_replayed(self) -> Option<CommandJournalEntry> {
        match self {
            Self::Fresh => None,
            Self::Replay(entry) => Some(*entry),
        }
    }
}

/// The read half of the command journal (spec §22.1).
///
/// Reading the journal says which commands were admitted and how they ended;
/// it admits nothing and settles nothing.
#[async_trait]
pub trait CommandJournalReader: Send + Sync {
    /// Loads one entry.
    ///
    /// # Errors
    /// * `NotFound` when it does not exist for `account`.
    async fn get(
        &self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<CommandJournalEntry, StoreError>;

    /// Every entry of a turn, ordered by `created_at` then `command_id`.
    ///
    /// # Errors
    /// * [`StoreError`] when the listing could not be read.
    async fn for_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError>;

    /// Entries of a turn whose status is `Pending` or `Executing`, ordered by
    /// `created_at` then `command_id` (spec §23.1: resume by idempotency key).
    ///
    /// # Errors
    /// * [`StoreError`] when the listing could not be read.
    async fn pending_for_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError>;
}

/// The write half of the command journal (spec §22.1).
#[async_trait]
pub trait CommandJournalWriter: Send + Sync {
    /// Admits a command under `UNIQUE (account_id, idempotency_key)`.
    ///
    /// The entry is persisted as given (normally `Pending`) when the key is
    /// new. When the key exists, nothing is written and the persisted entry is
    /// returned as [`JournalAdmission::Replay`], whatever its status.
    ///
    /// # Errors
    /// * `Conflict` when `command_id` already exists for the account under
    ///   another idempotency key.
    async fn begin(&self, entry: CommandJournalEntry) -> Result<JournalAdmission, StoreError>;

    /// Moves `Pending → Executing`. Calling it on an `Executing` entry is
    /// accepted without change.
    ///
    /// # Errors
    /// * `NotFound` when the entry does not exist for `account`.
    /// * `Conflict` when the status is neither `Pending` nor `Executing`.
    async fn mark_executing(
        &self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<(), StoreError>;

    /// Records the outcome and moves the entry to
    /// [`JournalOutcome::status`]. Recording the same outcome again on an
    /// entry already in that status is accepted without change.
    ///
    /// # Errors
    /// * `NotFound` when the entry does not exist for `account`.
    /// * `Conflict` when the transition is illegal or a different outcome is
    ///   already recorded.
    async fn complete(
        &self,
        account: &AccountId,
        command_id: &CommandId,
        outcome: JournalOutcome,
    ) -> Result<(), StoreError>;

    /// Records an execution error as the outcome, through
    /// [`JournalOutcome::from_execution_error`]. Same rules as
    /// [`Self::complete`].
    ///
    /// The default body is the whole contract; override it only to save a round
    /// trip, and keep the mapping identical.
    ///
    /// # Errors
    /// * Whatever [`Self::complete`] returns.
    async fn fail(
        &self,
        account: &AccountId,
        command_id: &CommandId,
        error: &ExecutionError,
    ) -> Result<(), StoreError> {
        self.complete(
            account,
            command_id,
            JournalOutcome::from_execution_error(*command_id, error),
        )
        .await
    }
}

/// The command journal (spec §22.1): both halves.
///
/// There is nothing to implement here: write [`CommandJournalReader`] and
/// [`CommandJournalWriter`] and the blanket implementation below supplies this
/// trait.
pub trait CommandJournal: CommandJournalReader + CommandJournalWriter {}

impl<T: CommandJournalReader + CommandJournalWriter + ?Sized> CommandJournal for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::error::RevisionConflict;
    use turnframe_core::event::UnknownOutcome;

    #[test]
    fn transition_table() {
        use CommandJournalStatus as S;
        let allowed: &[(S, S)] = &[
            (S::Pending, S::Executing),
            (S::Pending, S::Committed),
            (S::Pending, S::Failed),
            (S::Pending, S::OutcomeUnknown),
            (S::AwaitingConfirmation, S::Executing),
            (S::AwaitingConfirmation, S::Committed),
            (S::AwaitingConfirmation, S::Failed),
            (S::AwaitingConfirmation, S::OutcomeUnknown),
            (S::Executing, S::Committed),
            (S::Executing, S::Failed),
            (S::Executing, S::OutcomeUnknown),
            (S::OutcomeUnknown, S::Committed),
            (S::OutcomeUnknown, S::Failed),
        ];
        for from in S::ALL {
            for to in S::ALL {
                assert_eq!(
                    S::can_transition(from, to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
        assert!(S::Committed.is_terminal());
        assert!(S::Failed.is_terminal());
        assert!(!S::OutcomeUnknown.is_terminal());
        assert!(S::Pending.is_pending());
        assert!(S::Executing.is_pending());
        assert!(!S::AwaitingConfirmation.is_pending());
        assert!(!S::AwaitingConfirmation.is_terminal());
    }

    #[test]
    fn outcome_status_mapping_and_error_conversion() {
        let id = CommandId::nil();
        let conflict = ExecutionError::RevisionConflict(RevisionConflict {
            expected: CaseRef::new("w", "c", CaseRevision(1)),
            current_revision: CaseRevision(2),
        });
        let out = JournalOutcome::from_execution_error(id, &conflict);
        assert_eq!(out.status(), CommandJournalStatus::Failed);
        assert_eq!(
            out,
            JournalOutcome::RevisionConflict {
                current_revision: CaseRevision(2)
            }
        );
        let unknown = ExecutionError::OutcomeUnknown(UnknownOutcome {
            attempt_id: "a".into(),
            remote_ref: Some("r".into()),
            reason: "timeout_after_send".into(),
        });
        assert_eq!(
            JournalOutcome::from_execution_error(id, &unknown).status(),
            CommandJournalStatus::OutcomeUnknown
        );
        assert_eq!(
            JournalOutcome::from_execution_error(id, &ExecutionError::Timeout).status(),
            CommandJournalStatus::OutcomeUnknown
        );
        assert_eq!(
            JournalOutcome::from_execution_error(id, &ExecutionError::Store(StoreError::Timeout)),
            JournalOutcome::Failed {
                code: "store.timeout".into()
            }
        );
        assert_eq!(
            JournalOutcome::from_execution_error(id, &ExecutionError::ScopeViolation).status(),
            CommandJournalStatus::Failed
        );
    }

    #[test]
    fn admission_helpers() {
        assert!(JournalAdmission::Fresh.is_fresh());
        assert!(JournalAdmission::Fresh.replayed().is_none());
        assert!(JournalAdmission::Fresh.into_replayed().is_none());
    }
}
