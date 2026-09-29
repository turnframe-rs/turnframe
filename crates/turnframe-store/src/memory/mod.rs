//! [`MemoryStores`]: one deterministic implementation of every persistence
//! trait, over a single shared state.
//!
//! It exists for three jobs. It is what tests, examples and the `turnframe`
//! facade use when no database is configured. It is the reference an adapter
//! author reads when the prose in a trait leaves a question open — if
//! PostgreSQL and this store disagree, one of them is wrong. And it is the
//! subject the [`conformance`](crate::conformance) suite is developed against,
//! so a suite failure means the adapter is wrong rather than the suite.
//!
//! # What "deterministic" buys
//!
//! * **Order.** Every collection is a `BTreeMap` keyed by account first, and
//!   every list is sorted by the key its trait documents. Two runs over the
//!   same writes produce the same lists, so `assert_eq!` on a whole list is a
//!   fair test rather than a flaky one.
//! * **Time.** The store never reads the wall clock directly; it asks a
//!   [`Clock`]. With a [`ManualClock`], "the card expired" is something a test
//!   makes true, not something it waits for.
//! * **Failure.** [`MemoryStores::fail_next`] arms a [`FailurePoint`], so the
//!   recovery paths of spec §27.7 are reachable without killing a process.
//! * **Cost.** A commit is all-or-nothing without copying the state: the bundle
//!   is applied in place and every mutation records how to undo itself, so a
//!   failure replays the reversals instead of discarding a clone. A commit
//!   costs what it writes, not what the store already holds, which is what
//!   keeps a suite of thousands of turns from slowing down as it goes.
//!
//! # Concurrency
//!
//! One `std::sync::Mutex` guards the whole state and a second, independent one
//! guards the armed failures. Every trait method locks, runs one synchronous
//! rule function and releases before returning: no guard is ever alive across
//! an await, which is why these futures are `Send` at all. A poisoned lock is
//! reported as [`StoreError::Corrupt`] rather than propagating a panic.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use chrono::TimeDelta;
//! use turnframe_store::memory::{Clock, ManualClock, MemoryStores};
//!
//! let clock = Arc::new(ManualClock::epoch());
//! let stores = MemoryStores::with_clock(clock.clone());
//! assert_eq!(stores.now(), clock.now());
//!
//! clock.advance(TimeDelta::minutes(5));
//! assert_eq!(stores.now(), clock.now());
//! ```

mod clock;
mod fault;
mod impls;
mod state;

use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};

pub use self::clock::{Clock, ManualClock, SystemClock};
use self::fault::FaultQueue;
pub use self::fault::{FailurePoint, UnknownFailurePoint};
use self::state::Inner;
use crate::error::StoreError;

/// A complete, deterministic set of stores backed by one in-process state.
///
/// Implements [`ConversationStore`](crate::conversation::ConversationStore),
/// [`InteractionStore`](crate::interaction::InteractionStore),
/// [`CommandJournal`](crate::journal::CommandJournal),
/// [`EventJournal`](crate::events::EventJournal),
/// [`OutboxStore`](crate::outbox::OutboxStore),
/// [`ReplayStore`](crate::replay::ReplayStore) and
/// [`CommitStore`](crate::commit::CommitStore) over the same state, so a card
/// written through the interaction trait is the card a commit bundle settles.
///
/// Wrap it in [`Stores::from_memory`](crate::stores::Stores::from_memory), or
/// take the shortcut [`Stores::in_memory`](crate::stores::Stores::in_memory).
#[derive(Debug)]
pub struct MemoryStores {
    inner: Mutex<Inner>,
    faults: Mutex<FaultQueue>,
    clock: Arc<dyn Clock>,
}

impl MemoryStores {
    /// An empty store on the wall clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    /// An empty store that stamps its own columns from `clock`.
    ///
    /// Keep a clone of the clock to drive it: with a [`ManualClock`] the test
    /// decides when a claim goes stale or a phase marker moves.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            faults: Mutex::new(FaultQueue::default()),
            clock,
        }
    }

    /// The instant this store would stamp a write with right now.
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Arms one failure: the next call reaching `point` returns `error` instead
    /// of its normal result.
    ///
    /// Whether the write that precedes the boundary survives is part of the
    /// point's meaning; see [`FailurePoint`]. Arming several failures is
    /// allowed, including several at the same point, and they fire in arming
    /// order. Inside
    /// [`CommitStore::commit`](crate::commit::CommitStore::commit) a fired
    /// failure discards the whole bundle.
    ///
    /// # Errors
    /// * `Corrupt` when the internal lock is poisoned.
    pub fn fail_next(&self, point: FailurePoint, error: StoreError) -> Result<(), StoreError> {
        self.faults()?.arm(point, error);
        Ok(())
    }

    /// The points still armed, in arming order.
    ///
    /// # Errors
    /// * `Corrupt` when the internal lock is poisoned.
    pub fn armed_failures(&self) -> Result<Vec<FailurePoint>, StoreError> {
        Ok(self.faults()?.armed_points())
    }

    /// Disarms every pending failure.
    ///
    /// # Errors
    /// * `Corrupt` when the internal lock is poisoned.
    pub fn clear_failures(&self) -> Result<(), StoreError> {
        self.faults()?.clear();
        Ok(())
    }

    /// The state guard. Held for one synchronous rule call and released; the
    /// lock is never alive across an await.
    fn inner(&self) -> Result<MutexGuard<'_, Inner>, StoreError> {
        self.inner.lock().map_err(|_| StoreError::Corrupt)
    }

    fn faults(&self) -> Result<MutexGuard<'_, FaultQueue>, StoreError> {
        self.faults.lock().map_err(|_| StoreError::Corrupt)
    }

    /// Returns the armed error for `point`, consuming the arming.
    fn fire(&self, point: FailurePoint) -> Result<(), StoreError> {
        match self.faults()?.take(point) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Default for MemoryStores {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ConversationRecord, ConversationWriter};
    use turnframe_core::ids::{AccountId, ConversationId};

    #[test]
    fn failures_are_armed_consumed_and_cleared() {
        let stores = MemoryStores::new();
        assert!(stores.armed_failures().unwrap().is_empty());
        stores
            .fail_next(FailurePoint::BeforeOutboxDispatch, StoreError::Unavailable)
            .unwrap();
        stores
            .fail_next(FailurePoint::BeforeJournalInsert, StoreError::Timeout)
            .unwrap();
        assert_eq!(
            stores.armed_failures().unwrap(),
            vec![
                FailurePoint::BeforeOutboxDispatch,
                FailurePoint::BeforeJournalInsert
            ]
        );
        assert_eq!(
            stores.fire(FailurePoint::BeforeJournalInsert),
            Err(StoreError::Timeout)
        );
        assert_eq!(
            stores.armed_failures().unwrap(),
            vec![FailurePoint::BeforeOutboxDispatch]
        );
        stores.clear_failures().unwrap();
        assert!(stores.armed_failures().unwrap().is_empty());
        assert_eq!(stores.fire(FailurePoint::BeforeOutboxDispatch), Ok(()));
    }

    #[tokio::test]
    async fn clock_drives_the_stamps_the_store_owns() {
        let clock = Arc::new(ManualClock::epoch());
        let stores = MemoryStores::with_clock(clock.clone());
        let account = AccountId::from("a");
        let conversation = ConversationId::new();
        stores
            .create_conversation(ConversationRecord::new(
                conversation,
                account.clone(),
                clock.now(),
            ))
            .await
            .unwrap();
        assert_eq!(stores.now(), DateTime::<Utc>::UNIX_EPOCH);
        clock.advance(chrono::TimeDelta::seconds(90));
        assert_eq!(
            stores.now(),
            DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::seconds(90)
        );
    }
}
