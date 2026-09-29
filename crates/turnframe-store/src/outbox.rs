//! The outbox: external side effects awaiting dispatch (spec §16.4, ADR-007).
//!
//! # Contract
//!
//! * `(destination, idempotency_key)` is unique: a given external action exists
//!   at most once per destination. A duplicate enqueue is `Conflict`.
//! * [`OutboxWriter::claim_due`] has *skip-locked* semantics: it selects
//!   `Pending` entries whose `next_attempt_at` is absent or not after `now`,
//!   moves them to `Dispatching`, increments `attempt_count`, records the
//!   claiming worker and returns them. An entry claimed by one worker is not
//!   returned to another until it is rescheduled, released or marked.
//! * Status changes follow `OutboxStatus::can_transition` from `turnframe-core`:
//!   `Dispatching` ends in `Completed`, `Failed`, `OutcomeUnknown` or goes back
//!   to `Pending` (retry later); `OutcomeUnknown` is settled by reconciliation
//!   to `Completed` or `Failed`, or rescheduled to `Pending` when the remote
//!   guarantees idempotency. Re-marking a terminal status with the same status
//!   is accepted; anything else illegal is `Conflict`.
//! * The outbox is a system-owned queue read by dispatchers, not by end users;
//!   its rows are addressed by `outbox_id` and carry the `command_id` that
//!   links them to the account-scoped journal.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use turnframe_core::event::OutboxEntry;
use turnframe_core::ids::{CommandId, OutboxId};

use crate::error::StoreError;

/// Who holds an entry in `Dispatching` and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxClaim {
    /// Dispatcher identifier.
    pub worker_id: String,
    /// When the claim was taken.
    pub claimed_at: DateTime<Utc>,
}

/// An outbox entry with the store-owned dispatch metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRecord {
    /// The entry as the core sees it.
    pub entry: OutboxEntry,
    /// Current claim, while `Dispatching`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<OutboxClaim>,
    /// Stable code of the last failure, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    /// Remote reference recorded with an unknown outcome, for reconciliation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_ref: Option<String>,
}

impl OutboxRecord {
    /// Wraps a freshly enqueued entry.
    #[must_use]
    pub fn new(entry: OutboxEntry) -> Self {
        Self {
            entry,
            claim: None,
            last_failure: None,
            remote_ref: None,
        }
    }
}

/// The read half of the outbox (spec §22.1).
///
/// It says what is queued and what happened to it. Every state change —
/// enqueueing, claiming, completing, failing, rescheduling, releasing — is the
/// write half.
#[async_trait]
pub trait OutboxReader: Send + Sync {
    /// Loads one record.
    ///
    /// # Errors
    /// * `NotFound` when it does not exist.
    async fn get(&self, outbox_id: &OutboxId) -> Result<OutboxRecord, StoreError>;

    /// Every record produced by a command, ordered by `created_at` then id.
    ///
    /// # Errors
    /// * [`StoreError`] when the listing could not be read.
    async fn list_for_command(
        &self,
        command_id: &CommandId,
    ) -> Result<Vec<OutboxRecord>, StoreError>;
}

/// The write half of the outbox (spec §22.1).
///
/// [`claim_due`](OutboxWriter::claim_due) is here and not in
/// [`OutboxReader`] on purpose: it looks like a read and is not one. It takes
/// the entries it returns, moving them to `Dispatching` under the caller's
/// worker id, so a second caller cannot get them.
#[async_trait]
pub trait OutboxWriter: Send + Sync {
    /// Enqueues a `Pending` entry.
    ///
    /// # Errors
    /// * `Other(INVALID_RECORD)` when `entry.status` is not `Pending`.
    /// * `Conflict` when `outbox_id` or `(destination, idempotency_key)` exists.
    async fn enqueue(&self, entry: OutboxEntry) -> Result<(), StoreError>;

    /// Claims up to `limit` due `Pending` entries for `worker_id` (see the
    /// module documentation), oldest first by `created_at` then id, and
    /// returns them already in `Dispatching` with `attempt_count` incremented.
    ///
    /// # Errors
    /// * [`StoreError`] when the claim could not be written.
    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        worker_id: &str,
    ) -> Result<Vec<OutboxEntry>, StoreError>;

    /// `Dispatching | OutcomeUnknown → Completed`; clears the claim and stamps
    /// `completed_at`. Accepted without change when already `Completed`.
    ///
    /// # Errors
    /// * `NotFound` / `Conflict` on an illegal transition.
    async fn mark_completed(&self, outbox_id: &OutboxId) -> Result<(), StoreError>;

    /// Records a failure. `reason` is a stable code, never free text, and is
    /// stored on the record either way.
    ///
    /// With `retry_at`, the entry goes back to `Pending` with
    /// `next_attempt_at = retry_at` and no claim; that is legal from
    /// `Dispatching`, `OutcomeUnknown` and `Pending` (which only moves the
    /// time), and refused on a terminal status. Without `retry_at` the entry
    /// becomes `Failed` and stamps `completed_at`; repeating that on an already
    /// `Failed` entry is accepted without change.
    ///
    /// # Errors
    /// * `NotFound` / `Conflict` on an illegal transition.
    async fn mark_failed(
        &self,
        outbox_id: &OutboxId,
        reason: String,
        retry_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError>;

    /// `Dispatching → OutcomeUnknown`, releasing the claim and recording the
    /// remote reference when the remote returned one (I15).
    ///
    /// Repeating it on an already `OutcomeUnknown` entry is accepted and
    /// changes nothing, except that a `remote_ref` is filled in when the entry
    /// had none: reconciliation may learn the reference after the fact, and
    /// losing it would leave the row unreconcilable.
    ///
    /// # Errors
    /// * `NotFound` / `Conflict` on an illegal transition.
    async fn mark_outcome_unknown(
        &self,
        outbox_id: &OutboxId,
        remote_ref: Option<String>,
    ) -> Result<(), StoreError>;

    /// Returns the entry to `Pending` with `next_attempt_at`, releasing any
    /// claim. Legal from `Dispatching`, `OutcomeUnknown` and `Pending` (which
    /// only moves the time).
    ///
    /// # Errors
    /// * `NotFound` / `Conflict` on a terminal status.
    async fn reschedule(
        &self,
        outbox_id: &OutboxId,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    /// Releases every `Dispatching` entry whose claim was taken strictly
    /// before `claimed_before` back to `Pending`, due immediately. Returns the
    /// released identifiers. Meant for a reaper that recovers entries a crashed
    /// dispatcher left behind.
    ///
    /// # Errors
    /// * [`StoreError`] when the sweep could not be written.
    async fn release_expired_claims(
        &self,
        claimed_before: DateTime<Utc>,
    ) -> Result<Vec<OutboxId>, StoreError>;
}

/// The outbox (spec §22.1): both halves.
///
/// There is nothing to implement here: write [`OutboxReader`] and
/// [`OutboxWriter`] and the blanket implementation below supplies this trait.
pub trait OutboxStore: OutboxReader + OutboxWriter {}

impl<T: OutboxReader + OutboxWriter + ?Sized> OutboxStore for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::command::IdempotencyKey;
    use turnframe_core::event::OutboxStatus;

    #[test]
    fn record_round_trips() {
        let record = OutboxRecord::new(OutboxEntry {
            outbox_id: OutboxId::nil(),
            command_id: CommandId::nil(),
            destination: "airline".into(),
            payload: serde_json::Value::Null,
            idempotency_key: IdempotencyKey::new("k"),
            status: OutboxStatus::Pending,
            attempt_count: 0,
            next_attempt_at: None,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            completed_at: None,
        });
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<OutboxRecord>(&json).unwrap(), record);
    }
}
