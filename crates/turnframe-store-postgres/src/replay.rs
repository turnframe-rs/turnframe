//! Replay records over `tf_replay` (spec §23.1, I20).
//!
//! One row per turn, rewritten as the turn advances: the runtime writes the
//! record at `Received` and again with more detail at every step, so the write
//! is an upsert and the table never grows a second row for one turn. The record
//! is stored whole as `jsonb`; the columns beside it are the keys the lookups
//! need, extracted so that "the records of this conversation, most recent first"
//! is an index scan rather than a scan of every document.

use async_trait::async_trait;
use sqlx::PgConnection;
use sqlx::postgres::PgRow;
use turnframe_core::ids::{AccountId, ConversationId, TurnId};
use turnframe_core::replay::ReplayRecord;
use turnframe_store::error::StoreError;
use turnframe_store::replay::{ReplayReader, ReplayWriter};

use crate::codec::{column, from_json, label, limit_to_sql, to_json};
use crate::error::store_error;
use crate::store::{PgStores, commit};

/// Inserts or replaces the record of one turn.
pub(crate) async fn put(conn: &mut PgConnection, record: ReplayRecord) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO tf_replay (account_id, turn_id, conversation_id, phase, recorded_at, record)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (account_id, turn_id) DO UPDATE
         SET conversation_id = EXCLUDED.conversation_id,
             phase = EXCLUDED.phase,
             recorded_at = EXCLUDED.recorded_at,
             record = EXCLUDED.record",
    )
    .bind(record.account_id.as_str())
    .bind(record.turn_id.as_uuid())
    .bind(record.conversation_id.as_uuid())
    .bind(label(&record.phase)?)
    .bind(record.recorded_at)
    .bind(to_json(&record)?)
    .execute(conn)
    .await
    .map_err(|error| store_error(&error))?;
    Ok(())
}

/// Loads the record of a turn.
pub(crate) async fn get(
    conn: &mut PgConnection,
    account: &AccountId,
    turn_id: &TurnId,
) -> Result<ReplayRecord, StoreError> {
    let row = sqlx::query("SELECT record FROM tf_replay WHERE account_id = $1 AND turn_id = $2")
        .bind(account.as_str())
        .bind(turn_id.as_uuid())
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))?
        .ok_or(StoreError::NotFound)?;
    decode_record(&row)
}

/// The most recent `limit` records of a conversation, oldest of them first.
pub(crate) async fn list_for_conversation(
    conn: &mut PgConnection,
    account: &AccountId,
    conversation: &ConversationId,
    limit: usize,
) -> Result<Vec<ReplayRecord>, StoreError> {
    let rows = sqlx::query(
        "SELECT record FROM tf_replay
         WHERE account_id = $1 AND conversation_id = $2
         ORDER BY recorded_at DESC, turn_id DESC
         LIMIT $3",
    )
    .bind(account.as_str())
    .bind(conversation.as_uuid())
    .bind(limit_to_sql(limit))
    .fetch_all(conn)
    .await
    .map_err(|error| store_error(&error))?;
    let mut records = rows
        .iter()
        .map(decode_record)
        .collect::<Result<Vec<ReplayRecord>, StoreError>>()?;
    records.reverse();
    Ok(records)
}

/// Rebuilds a record from its row.
fn decode_record(row: &PgRow) -> Result<ReplayRecord, StoreError> {
    from_json(column(row, "record")?)
}

#[async_trait]
impl ReplayReader for PgStores {
    async fn get(&self, account: &AccountId, turn_id: &TurnId) -> Result<ReplayRecord, StoreError> {
        let mut conn = self.connection().await?;
        get(&mut conn, account, turn_id).await
    }

    async fn list_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<ReplayRecord>, StoreError> {
        let mut conn = self.connection().await?;
        list_for_conversation(&mut conn, account, conversation, limit).await
    }
}

#[async_trait]
impl ReplayWriter for PgStores {
    async fn put(&self, record: ReplayRecord) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        put(&mut transaction, record).await?;
        commit(transaction).await
    }
}
