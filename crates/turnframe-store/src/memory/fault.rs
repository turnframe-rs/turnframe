//! Failure injection at the boundaries spec §27.7 asks chaos tests to break.
//!
//! Recovery code is the part of a system that is never exercised by the happy
//! path, so the in-memory store lets a test decide exactly where the process
//! "dies". [`MemoryStores::fail_next`](super::MemoryStores::fail_next) arms one
//! failure at a [`FailurePoint`]; the next call that reaches that boundary
//! returns the armed [`StoreError`] and the arming is consumed. Several
//! failures may be armed at once, including several at the same point: they
//! fire in the order they were armed.
//!
//! Whether the write that precedes the boundary survives is the whole point,
//! and it differs per point. [`FailurePoint`] documents it variant by variant;
//! the rule of thumb is that a point named `After…` leaves the preceding write
//! **visible** — that is the partial state recovery has to cope with — while a
//! point named `Before…` writes nothing.
//!
//! Inside [`CommitStore::commit`](crate::commit::CommitStore::commit) the rule
//! is different, and it has to be: the bundle is atomic. A failure armed at a
//! point the bundle passes through aborts the whole bundle, and *nothing* of it
//! becomes visible — not even the stages that had already been applied to the
//! staged state.

use std::collections::VecDeque;

use crate::error::StoreError;

/// A boundary at which the in-memory store can be made to fail (spec §27.7).
///
/// The names describe the moment in a turn, not the method: one point may be
/// reached from more than one method, and the variant documentation names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FailurePoint {
    /// The card was written and the caller is told it was not.
    ///
    /// Fires in [`InteractionWriter::insert`](crate::interaction::InteractionWriter::insert)
    /// and
    /// [`insert_replacing_blocking`](crate::interaction::InteractionWriter::insert_replacing_blocking)
    /// **after** the insert: the interaction (and any invalidation the insert
    /// performed) stays visible. Spec §15.5 requires interaction persistence to
    /// succeed before the response mentions the card, so the truthful reaction
    /// is to omit the card, not to claim it.
    ///
    /// Inside a commit bundle it fires after stage 5 (interaction inserts) and
    /// aborts the bundle.
    AfterInteractionPersistence,
    /// The command was never admitted.
    ///
    /// Fires in [`CommandJournalWriter::begin`](crate::journal::CommandJournalWriter::begin)
    /// **before** anything is written, so the key stays free and the command
    /// may be admitted again from scratch.
    BeforeJournalInsert,
    /// The command was admitted and then the process died before the domain
    /// commit.
    ///
    /// Fires in [`CommandJournalWriter::begin`](crate::journal::CommandJournalWriter::begin)
    /// **after** the entry is persisted: the entry survives in `Pending`, which
    /// is exactly the state
    /// [`pending_for_turn`](crate::journal::CommandJournalReader::pending_for_turn)
    /// exists to find, and recovery must resume it by idempotency key rather
    /// than admitting a second command (spec §23.1).
    ///
    /// Inside a commit bundle it fires after stage 1 (journal completions) and
    /// aborts the bundle.
    AfterJournalInsertBeforeCommit,
    /// The events were appended and the caller could not read them back.
    ///
    /// Fires in [`EventJournalWriter::append`](crate::events::EventJournalWriter::append)
    /// **after** the batch is appended: the events are in the ledger, so a
    /// recovery that re-reads by command finds them and must not append them a
    /// second time.
    ///
    /// Inside a commit bundle it fires after stage 2 (event batches) and aborts
    /// the bundle.
    AfterCommitBeforeEventReadback,
    /// The dispatcher never got the work.
    ///
    /// Fires in [`OutboxWriter::claim_due`](crate::outbox::OutboxWriter::claim_due)
    /// **before** anything is claimed: no row moves to `Dispatching` and the
    /// next sweep picks the same rows up.
    BeforeOutboxDispatch,
    /// The external call was made and its result could not be recorded.
    ///
    /// Fires in [`mark_completed`](crate::outbox::OutboxWriter::mark_completed),
    /// [`mark_failed`](crate::outbox::OutboxWriter::mark_failed) and
    /// [`mark_outcome_unknown`](crate::outbox::OutboxWriter::mark_outcome_unknown)
    /// **before** the write: the row stays `Dispatching` with its claim, which
    /// is the state
    /// [`release_expired_claims`](crate::outbox::OutboxWriter::release_expired_claims)
    /// exists to reap. It is the worst case of spec §16.5 — an effect may exist
    /// and nothing local says so — so a retry is only safe when the remote
    /// guarantees idempotency.
    AfterOutboxDispatch,
    /// The answer was composed and never stored.
    ///
    /// Fires in
    /// [`append_assistant_turn`](crate::conversation::ConversationWriter::append_assistant_turn)
    /// **before** the write. The turn keeps its user side and its phase marker,
    /// so recovery regenerates the response from committed events and stored
    /// answer tasks instead of re-executing anything (spec §23.1).
    BeforeResponsePersistence,
}

impl FailurePoint {
    /// Every point, in declaration order.
    pub const ALL: [Self; 7] = [
        Self::AfterInteractionPersistence,
        Self::BeforeJournalInsert,
        Self::AfterJournalInsertBeforeCommit,
        Self::AfterCommitBeforeEventReadback,
        Self::BeforeOutboxDispatch,
        Self::AfterOutboxDispatch,
        Self::BeforeResponsePersistence,
    ];

    /// The boundary's stable name, matching the crash boundaries the
    /// specification enumerates.
    ///
    /// Stable across releases: test kits and fixtures address a boundary by
    /// this string, so renaming one is a breaking change.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AfterInteractionPersistence => "after_interaction_persistence",
            Self::BeforeJournalInsert => "before_journal_insert",
            Self::AfterJournalInsertBeforeCommit => "after_journal_insert_before_commit",
            Self::AfterCommitBeforeEventReadback => "after_commit_before_event_readback",
            Self::BeforeOutboxDispatch => "before_outbox_dispatch",
            Self::AfterOutboxDispatch => "after_outbox_dispatch",
            Self::BeforeResponsePersistence => "before_response_persistence",
        }
    }

    /// Parses a boundary from the name [`as_str`](Self::as_str) renders.
    ///
    /// # Errors
    /// Returns the unknown name when it matches no boundary.
    pub fn parse(name: &str) -> Result<Self, UnknownFailurePoint> {
        Self::ALL
            .into_iter()
            .find(|point| point.as_str() == name)
            .ok_or_else(|| UnknownFailurePoint {
                name: name.to_owned(),
            })
    }

    /// Returns `true` when the write that precedes the boundary stays visible
    /// after the injected failure (outside a commit bundle, which is always
    /// all-or-nothing).
    #[must_use]
    pub fn leaves_write_visible(self) -> bool {
        matches!(
            self,
            Self::AfterInteractionPersistence
                | Self::AfterJournalInsertBeforeCommit
                | Self::AfterCommitBeforeEventReadback
        )
    }
}

impl core::fmt::Display for FailurePoint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A name that matches no crash boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown crash boundary: {name}")]
pub struct UnknownFailurePoint {
    /// The name that did not match.
    pub name: String,
}

/// The queue of armed failures. Ordered, so a test can script a sequence.
#[derive(Debug, Default)]
pub(super) struct FaultQueue {
    armed: VecDeque<(FailurePoint, StoreError)>,
}

impl FaultQueue {
    /// Arms one failure at `point`.
    pub(super) fn arm(&mut self, point: FailurePoint, error: StoreError) {
        self.armed.push_back((point, error));
    }

    /// Consumes and returns the first failure armed at `point`, if any.
    pub(super) fn take(&mut self, point: FailurePoint) -> Option<StoreError> {
        let index = self.armed.iter().position(|(armed, _)| *armed == point)?;
        self.armed.remove(index).map(|(_, error)| error)
    }

    /// The points still armed, in arming order.
    pub(super) fn armed_points(&self) -> Vec<FailurePoint> {
        self.armed.iter().map(|(point, _)| *point).collect()
    }

    /// Disarms everything.
    pub(super) fn clear(&mut self) {
        self.armed.clear();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_boundary_has_a_stable_name_that_round_trips() {
        let mut seen = std::collections::BTreeSet::new();
        for point in FailurePoint::ALL {
            let name = point.as_str();
            assert!(seen.insert(name), "boundary names must be unique: {name}");
            assert_eq!(FailurePoint::parse(name), Ok(point));
            assert_eq!(point.to_string(), name);
        }
        assert!(FailurePoint::parse("nope").is_err());
    }

    use super::*;

    #[test]
    fn queue_fires_in_arming_order_per_point() {
        let mut queue = FaultQueue::default();
        queue.arm(FailurePoint::BeforeJournalInsert, StoreError::Unavailable);
        queue.arm(FailurePoint::AfterOutboxDispatch, StoreError::Timeout);
        queue.arm(FailurePoint::BeforeJournalInsert, StoreError::Timeout);
        assert_eq!(
            queue.armed_points(),
            vec![
                FailurePoint::BeforeJournalInsert,
                FailurePoint::AfterOutboxDispatch,
                FailurePoint::BeforeJournalInsert
            ]
        );
        assert_eq!(
            queue.take(FailurePoint::BeforeJournalInsert),
            Some(StoreError::Unavailable)
        );
        assert_eq!(
            queue.take(FailurePoint::BeforeJournalInsert),
            Some(StoreError::Timeout)
        );
        assert_eq!(queue.take(FailurePoint::BeforeJournalInsert), None);
        assert_eq!(
            queue.armed_points(),
            vec![FailurePoint::AfterOutboxDispatch]
        );
        queue.clear();
        assert!(queue.armed_points().is_empty());
    }

    #[test]
    fn write_visibility_is_declared_per_point() {
        for point in FailurePoint::ALL {
            let expected = matches!(
                point,
                FailurePoint::AfterInteractionPersistence
                    | FailurePoint::AfterJournalInsertBeforeCommit
                    | FailurePoint::AfterCommitBeforeEventReadback
            );
            assert_eq!(point.leaves_write_visible(), expected, "{point:?}");
        }
    }
}
