//! The command journal over `tf_command_journal`: idempotency admission and
//! persisted outcomes (spec §16.2, I14).
//!
//! # Admission is atomic, not optimistic
//!
//! `UNIQUE (account_id, idempotency_key)` is the rule, and
//! `INSERT … ON CONFLICT DO NOTHING` is how this adapter asks the database to
//! apply it. Two callers arriving with the same key at the same instant do not
//! both read "absent" and both insert: the second one's insert waits on the
//! first one's speculative row, then does nothing, and the re-read that follows
//! sees the committed entry and replays it. There is exactly one `Fresh` per
//! key, whatever the interleaving, and a repeat always carries the outcome the
//! first attempt persisted.
//!
//! That re-read depends on each statement taking a fresh snapshot, which is
//! true at `READ COMMITTED` — PostgreSQL's default and what this adapter
//! assumes throughout. Under `REPEATABLE READ` the statement would instead
//! raise a serialization failure, which this adapter reports as `Conflict`;
//! correct, but it turns a routine retry into a caller-visible refusal.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use sqlx::postgres::PgRow;
use turnframe_core::case::CaseRef;
use turnframe_core::command::IdempotencyKey;
use turnframe_core::ids::{AccountId, CommandId, TurnId};
use turnframe_store::error::StoreError;
use turnframe_store::journal::{
    CommandJournalEntry, CommandJournalReader, CommandJournalStatus, CommandJournalWriter,
    JournalAdmission, JournalOutcome,
};
use uuid::Uuid;

use crate::codec::{
    column, from_json, from_label, label, now, revision_from_sql, revision_to_sql, to_json,
};
use crate::error::store_error;
use crate::store::{PgStores, commit};

/// Everything a read needs to rebuild a [`CommandJournalEntry`].
const ENTRY_COLUMNS: &str = "account_id, command_id, idempotency_key, turn_id, workflow_key, \
     case_id, expected_revision, command_type, command_payload, origin, status, result, \
     created_at, completed_at";

/// The statuses a crashed turn is resumed from.
const PENDING_STATUSES: &str = "('pending', 'executing')";

/// Admits a command under `UNIQUE (account_id, idempotency_key)`.
pub(crate) async fn begin(
    conn: &mut PgConnection,
    entry: CommandJournalEntry,
) -> Result<JournalAdmission, StoreError> {
    if let Some(existing) =
        read_by_key(&mut *conn, &entry.account_id, &entry.idempotency_key).await?
    {
        return Ok(JournalAdmission::replay(existing));
    }
    let inserted = sqlx::query(
        "INSERT INTO tf_command_journal (
             account_id, command_id, idempotency_key, turn_id, workflow_key, case_id,
             expected_revision, command_type, command_payload, origin, status, result,
             created_at, completed_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         ON CONFLICT ON CONSTRAINT tf_command_journal_idempotency_key DO NOTHING",
    )
    .bind(entry.account_id.as_str())
    .bind(entry.command_id.as_uuid())
    .bind(entry.idempotency_key.as_str())
    .bind(entry.turn_id.as_uuid())
    .bind(entry.case_ref.workflow.as_str())
    .bind(entry.case_ref.case_id.as_str())
    .bind(revision_to_sql(entry.case_ref.expected_revision)?)
    .bind(entry.command_type.as_str())
    .bind(entry.command_payload.clone())
    .bind(to_json(&entry.origin)?)
    .bind(label(&entry.status)?)
    .bind(entry.result.as_ref().map(to_json).transpose()?)
    .bind(entry.created_at)
    .bind(entry.completed_at)
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if inserted.rows_affected() == 1 {
        return Ok(JournalAdmission::Fresh);
    }
    // Another caller took the key between the read and the insert. Its entry is
    // committed by now, and it is the one that must be replayed.
    read_by_key(conn, &entry.account_id, &entry.idempotency_key)
        .await?
        .map(JournalAdmission::replay)
        .ok_or(StoreError::Corrupt)
}

/// Reads the entry holding an idempotency key, if any.
async fn read_by_key(
    conn: &mut PgConnection,
    account: &AccountId,
    key: &IdempotencyKey,
) -> Result<Option<CommandJournalEntry>, StoreError> {
    let statement = format!(
        "SELECT {ENTRY_COLUMNS} FROM tf_command_journal
         WHERE account_id = $1 AND idempotency_key = $2"
    );
    let row = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(key.as_str())
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))?;
    row.as_ref().map(decode_entry).transpose()
}

/// Moves `Pending` or `AwaitingConfirmation` to `Executing`; repeating it on an
/// `Executing` entry changes nothing.
pub(crate) async fn mark_executing(
    conn: &mut PgConnection,
    account: &AccountId,
    command_id: &CommandId,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE tf_command_journal SET status = 'executing'
         WHERE account_id = $1 AND command_id = $2
           AND status IN ('pending', 'awaiting_confirmation')",
    )
    .bind(account.as_str())
    .bind(command_id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if updated.rows_affected() == 1 {
        return Ok(());
    }
    match read_entry(conn, account, command_id).await? {
        None => Err(StoreError::NotFound),
        Some(entry) if entry.status == CommandJournalStatus::Executing => Ok(()),
        Some(_) => Err(StoreError::Conflict),
    }
}

/// Records the outcome of a command.
///
/// The legal source statuses are computed from the contract's transition table
/// and go into the same statement as the write, so an entry that moved on
/// between a caller's read and its write is refused rather than overwritten.
/// Recording an outcome an entry already carries is accepted; recording a
/// different one is a `Conflict`.
pub(crate) async fn complete(
    conn: &mut PgConnection,
    account: &AccountId,
    command_id: &CommandId,
    outcome: JournalOutcome,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let target = outcome.status();
    let sources = legal_sources(target)?;
    let updated = sqlx::query(
        "UPDATE tf_command_journal SET status = $3, result = $4, completed_at = $5
         WHERE account_id = $1 AND command_id = $2 AND status = ANY($6)",
    )
    .bind(account.as_str())
    .bind(command_id.as_uuid())
    .bind(label(&target)?)
    .bind(to_json(&outcome)?)
    .bind(at)
    .bind(sources)
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if updated.rows_affected() == 1 {
        return Ok(());
    }
    match read_entry(conn, account, command_id).await? {
        None => Err(StoreError::NotFound),
        Some(entry) if entry.status == target && entry.result.as_ref() == Some(&outcome) => Ok(()),
        Some(_) => Err(StoreError::Conflict),
    }
}

/// The statuses `target` may be reached from, as their column labels.
fn legal_sources(target: CommandJournalStatus) -> Result<Vec<String>, StoreError> {
    CommandJournalStatus::ALL
        .into_iter()
        .filter(|from| CommandJournalStatus::can_transition(*from, target))
        .map(|from| label(&from))
        .collect()
}

/// Loads one entry.
pub(crate) async fn get(
    conn: &mut PgConnection,
    account: &AccountId,
    command_id: &CommandId,
) -> Result<CommandJournalEntry, StoreError> {
    read_entry(conn, account, command_id)
        .await?
        .ok_or(StoreError::NotFound)
}

/// Reads an entry without deciding what its absence means.
async fn read_entry(
    conn: &mut PgConnection,
    account: &AccountId,
    command_id: &CommandId,
) -> Result<Option<CommandJournalEntry>, StoreError> {
    let statement = format!(
        "SELECT {ENTRY_COLUMNS} FROM tf_command_journal
         WHERE account_id = $1 AND command_id = $2"
    );
    let row = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(command_id.as_uuid())
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))?;
    row.as_ref().map(decode_entry).transpose()
}

/// The entries of a turn, in creation order; `only_pending` keeps the ones a
/// crash left to resume.
pub(crate) async fn for_turn(
    conn: &mut PgConnection,
    account: &AccountId,
    turn_id: &TurnId,
    only_pending: bool,
) -> Result<Vec<CommandJournalEntry>, StoreError> {
    let filter = if only_pending {
        format!("AND status IN {PENDING_STATUSES}")
    } else {
        String::new()
    };
    let statement = format!(
        "SELECT {ENTRY_COLUMNS} FROM tf_command_journal
         WHERE account_id = $1 AND turn_id = $2 {filter}
         ORDER BY created_at, command_id"
    );
    let rows = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(turn_id.as_uuid())
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    rows.iter().map(decode_entry).collect()
}

/// Rebuilds an entry from its row.
fn decode_entry(row: &PgRow) -> Result<CommandJournalEntry, StoreError> {
    let account: String = column(row, "account_id")?;
    let command_id: Uuid = column(row, "command_id")?;
    let idempotency_key: String = column(row, "idempotency_key")?;
    let turn_id: Uuid = column(row, "turn_id")?;
    let workflow_key: String = column(row, "workflow_key")?;
    let case_id: String = column(row, "case_id")?;
    let revision: i64 = column(row, "expected_revision")?;
    let status: String = column(row, "status")?;
    let result: Option<serde_json::Value> = column(row, "result")?;
    Ok(CommandJournalEntry {
        command_id: CommandId::from(command_id),
        account_id: AccountId::from(account),
        idempotency_key: IdempotencyKey::new(idempotency_key),
        turn_id: TurnId::from(turn_id),
        case_ref: CaseRef::new(workflow_key, case_id, revision_from_sql(revision)?),
        command_type: column(row, "command_type")?,
        command_payload: column(row, "command_payload")?,
        origin: from_json(column(row, "origin")?)?,
        status: from_label(&status)?,
        result: result.map(from_json).transpose()?,
        created_at: column(row, "created_at")?,
        completed_at: column(row, "completed_at")?,
    })
}

#[async_trait]
impl CommandJournalReader for PgStores {
    async fn get(
        &self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<CommandJournalEntry, StoreError> {
        let mut conn = self.connection().await?;
        get(&mut conn, account, command_id).await
    }

    async fn for_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError> {
        let mut conn = self.connection().await?;
        for_turn(&mut conn, account, turn_id, false).await
    }

    async fn pending_for_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Vec<CommandJournalEntry>, StoreError> {
        let mut conn = self.connection().await?;
        for_turn(&mut conn, account, turn_id, true).await
    }
}

#[async_trait]
impl CommandJournalWriter for PgStores {
    async fn begin(&self, entry: CommandJournalEntry) -> Result<JournalAdmission, StoreError> {
        let mut transaction = self.transaction().await?;
        let admission = begin(&mut transaction, entry).await?;
        commit(transaction).await?;
        Ok(admission)
    }

    async fn mark_executing(
        &self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        mark_executing(&mut transaction, account, command_id).await?;
        commit(transaction).await
    }

    async fn complete(
        &self,
        account: &AccountId,
        command_id: &CommandId,
        outcome: JournalOutcome,
    ) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        complete(&mut transaction, account, command_id, outcome, now()).await?;
        commit(transaction).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legal_sources_are_the_contract_transition_table() {
        // A settled command must never be resumed, so `Committed` is reachable
        // from the three unsettled statuses and from nothing else.
        assert_eq!(
            legal_sources(CommandJournalStatus::Committed).unwrap(),
            vec![
                "pending",
                "awaiting_confirmation",
                "executing",
                "outcome_unknown"
            ]
        );
        assert_eq!(
            legal_sources(CommandJournalStatus::Failed).unwrap(),
            vec![
                "pending",
                "awaiting_confirmation",
                "executing",
                "outcome_unknown"
            ]
        );
        assert_eq!(
            legal_sources(CommandJournalStatus::OutcomeUnknown).unwrap(),
            vec!["pending", "awaiting_confirmation", "executing"]
        );
        assert_eq!(
            legal_sources(CommandJournalStatus::Executing).unwrap(),
            vec!["pending", "awaiting_confirmation"]
        );
    }
}
