//! Replay records: one per turn, rewritten as the turn progresses (spec §23.1, I20).
//!
//! # Contract
//!
//! * A replay record is keyed by `(account_id, turn_id)` and **upserted**: the
//!   runtime writes it at `Received` and rewrites it with more detail as the
//!   turn advances, so [`ReplayWriter::put`] replaces any previous version.
//! * Lookups are account-scoped; another tenant's turn is `NotFound`.

use async_trait::async_trait;
use turnframe_core::ids::{AccountId, ConversationId, TurnId};
use turnframe_core::replay::ReplayRecord;

use crate::error::StoreError;

/// The read half of the replay record contract (spec §22.1).
#[async_trait]
pub trait ReplayReader: Send + Sync {
    /// Loads the record of a turn.
    ///
    /// # Errors
    /// * `NotFound` when it does not exist for `account`.
    async fn get(&self, account: &AccountId, turn_id: &TurnId) -> Result<ReplayRecord, StoreError>;

    /// The most recent `limit` records of a conversation in chronological order
    /// (by `recorded_at`, then `turn_id`).
    ///
    /// # Errors
    /// * [`StoreError`] when the listing could not be read.
    async fn list_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<ReplayRecord>, StoreError>;
}

/// The write half of the replay record contract (spec §22.1).
#[async_trait]
pub trait ReplayWriter: Send + Sync {
    /// Inserts or replaces the record of `(record.account_id, record.turn_id)`.
    ///
    /// # Errors
    /// * [`StoreError`] when the record could not be written.
    async fn put(&self, record: ReplayRecord) -> Result<(), StoreError>;
}

/// Persistence of replay records (spec §22.1): both halves.
///
/// There is nothing to implement here: write [`ReplayReader`] and
/// [`ReplayWriter`] and the blanket implementation below supplies this trait.
pub trait ReplayStore: ReplayReader + ReplayWriter {}

impl<T: ReplayReader + ReplayWriter + ?Sized> ReplayStore for T {}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use turnframe_core::ids::EventId;
    use turnframe_core::replay::TurnPhase;

    use super::*;
    use crate::memory::MemoryStores;

    fn record(
        account: &AccountId,
        conversation: ConversationId,
        turn: TurnId,
        at: DateTime<Utc>,
    ) -> ReplayRecord {
        ReplayRecord::received(turn, conversation, account.clone(), at)
    }

    #[tokio::test]
    async fn put_replaces_and_get_is_account_scoped() {
        let store = MemoryStores::new();
        let account = AccountId::from("a");
        let conversation = ConversationId::new();
        let turn = TurnId::new();
        let epoch = DateTime::<Utc>::UNIX_EPOCH;

        store
            .put(record(&account, conversation, turn, epoch))
            .await
            .unwrap();

        // The runtime rewrites the record as the turn advances; the store keeps
        // one version per turn, not a history.
        let mut advanced = record(&account, conversation, turn, epoch + TimeDelta::seconds(1));
        advanced.phase = TurnPhase::Composed;
        advanced.event_ids = vec![EventId::new()];
        store.put(advanced.clone()).await.unwrap();

        assert_eq!(
            ReplayReader::get(&store, &account, &turn).await.unwrap(),
            advanced
        );
        assert_eq!(
            store
                .list_for_conversation(&account, &conversation, 10)
                .await
                .unwrap(),
            vec![advanced]
        );
        assert_eq!(
            ReplayReader::get(&store, &AccountId::from("b"), &turn).await,
            Err(StoreError::NotFound)
        );
        assert!(
            store
                .list_for_conversation(&AccountId::from("b"), &conversation, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn list_returns_the_most_recent_records_chronologically() {
        let store = MemoryStores::new();
        let account = AccountId::from("a");
        let conversation = ConversationId::new();
        let epoch = DateTime::<Utc>::UNIX_EPOCH;
        let mut turns = Vec::new();
        for seconds in 0..4 {
            let turn = TurnId::new();
            turns.push(turn);
            store
                .put(record(
                    &account,
                    conversation,
                    turn,
                    epoch + TimeDelta::seconds(seconds),
                ))
                .await
                .unwrap();
        }
        let recent = store
            .list_for_conversation(&account, &conversation, 2)
            .await
            .unwrap();
        assert_eq!(
            recent
                .iter()
                .map(|record| record.turn_id)
                .collect::<Vec<_>>(),
            vec![turns[2], turns[3]]
        );
    }
}
