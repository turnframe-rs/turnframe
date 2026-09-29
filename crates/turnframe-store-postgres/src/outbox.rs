//! The outbox over `tf_outbox` (spec §16.4, ADR-007).
//!
//! # Why a claim is safe
//!
//! [`claim_due`] selects the due rows `FOR UPDATE SKIP LOCKED` and marks them in
//! the same statement. A row another dispatcher already holds is skipped rather
//! than waited on, so two workers sweeping at the same instant divide the queue
//! between them and never both send the same external request. The claim only
//! becomes visible when the claiming transaction commits, which is why every
//! call here runs inside one.
//!
//! # Why it is not account-scoped
//!
//! Every other table in this schema leads with `account_id`. The outbox is the
//! deliberate exception the persistence contract names: it is a system-owned
//! dispatch queue addressed by [`OutboxId`], never by user input, and it carries
//! `command_id` so a row can be traced back to the account-scoped journal entry
//! that produced it. Nothing here takes an account, so nothing here can leak one
//! tenant's rows to another's request — a dispatcher is not serving a request.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use sqlx::postgres::PgRow;
use turnframe_core::command::IdempotencyKey;
use turnframe_core::event::{OutboxEntry, OutboxStatus};
use turnframe_core::ids::{CommandId, OutboxId};
use turnframe_store::error::{StoreError, invalid_record};
use turnframe_store::outbox::{OutboxClaim, OutboxReader, OutboxRecord, OutboxWriter};
use uuid::Uuid;

use crate::codec::{
    attempts_from_sql, attempts_to_sql, column, from_label, label, limit_to_sql, now, to_json,
};
use crate::error::store_error;
use crate::store::{PgStores, commit};

/// Everything a read needs to rebuild an [`OutboxRecord`].
const RECORD_COLUMNS: &str = "outbox_id, command_id, destination, payload, idempotency_key, \
     status, attempt_count, next_attempt_at, created_at, completed_at, claim_worker_id, \
     claim_taken_at, last_failure, remote_ref";

/// The same columns qualified for the claim statement, which joins the rows it
/// locked and would otherwise leave `outbox_id` ambiguous.
const CLAIMED_COLUMNS: &str = "o.outbox_id, o.command_id, o.destination, o.payload, \
     o.idempotency_key, o.status, o.attempt_count, o.next_attempt_at, o.created_at, \
     o.completed_at, o.claim_worker_id, o.claim_taken_at, o.last_failure, o.remote_ref";

/// The statuses a row never leaves.
const TERMINAL_STATUSES: &str = "('completed', 'failed')";

/// Enqueues a `Pending` row.
pub(crate) async fn enqueue(conn: &mut PgConnection, entry: OutboxEntry) -> Result<(), StoreError> {
    if entry.status != OutboxStatus::Pending {
        return Err(invalid_record());
    }
    sqlx::query(
        "INSERT INTO tf_outbox (
             outbox_id, command_id, destination, payload, idempotency_key, status,
             attempt_count, next_attempt_at, created_at, completed_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(entry.outbox_id.as_uuid())
    .bind(entry.command_id.as_uuid())
    .bind(entry.destination.as_str())
    .bind(to_json(&entry.payload)?)
    .bind(entry.idempotency_key.as_str())
    .bind(label(&entry.status)?)
    .bind(attempts_to_sql(entry.attempt_count)?)
    .bind(entry.next_attempt_at)
    .bind(entry.created_at)
    .bind(entry.completed_at)
    .execute(conn)
    .await
    .map_err(|error| store_error(&error))?;
    Ok(())
}

/// Loads one row.
pub(crate) async fn get(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
) -> Result<OutboxRecord, StoreError> {
    let statement = format!("SELECT {RECORD_COLUMNS} FROM tf_outbox WHERE outbox_id = $1");
    let row = sqlx::query(&statement)
        .bind(outbox_id.as_uuid())
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))?
        .ok_or(StoreError::NotFound)?;
    decode_record(&row)
}

/// The rows one command produced, oldest first.
pub(crate) async fn list_for_command(
    conn: &mut PgConnection,
    command_id: &CommandId,
) -> Result<Vec<OutboxRecord>, StoreError> {
    let statement = format!(
        "SELECT {RECORD_COLUMNS} FROM tf_outbox
         WHERE command_id = $1
         ORDER BY created_at, outbox_id"
    );
    let rows = sqlx::query(&statement)
        .bind(command_id.as_uuid())
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    rows.iter().map(decode_record).collect()
}

/// Claims up to `limit` due rows for `worker_id`.
///
/// `SKIP LOCKED` is what makes two dispatchers safe to run at once: a row
/// another transaction is already claiming is passed over instead of waited on.
pub(crate) async fn claim_due(
    conn: &mut PgConnection,
    at: DateTime<Utc>,
    limit: usize,
    worker_id: &str,
) -> Result<Vec<OutboxEntry>, StoreError> {
    let statement = format!(
        "WITH due AS (
             SELECT outbox_id FROM tf_outbox
             WHERE status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= $1)
             ORDER BY created_at, outbox_id
             LIMIT $2
             FOR UPDATE SKIP LOCKED
         )
         UPDATE tf_outbox o
         SET status = 'dispatching',
             attempt_count = o.attempt_count + 1,
             claim_worker_id = $3,
             claim_taken_at = $1
         FROM due
         WHERE o.outbox_id = due.outbox_id
         RETURNING {CLAIMED_COLUMNS}"
    );
    let rows = sqlx::query(&statement)
        .bind(at)
        .bind(limit_to_sql(limit))
        .bind(worker_id)
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    let mut claimed = rows
        .iter()
        .map(decode_record)
        .collect::<Result<Vec<OutboxRecord>, StoreError>>()?;
    // `RETURNING` has no order of its own, and the contract names one.
    claimed.sort_by(|a, b| {
        a.entry
            .created_at
            .cmp(&b.entry.created_at)
            .then_with(|| a.entry.outbox_id.cmp(&b.entry.outbox_id))
    });
    Ok(claimed.into_iter().map(|record| record.entry).collect())
}

/// Settles a row as delivered.
pub(crate) async fn mark_completed(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE tf_outbox
         SET status = 'completed', completed_at = $2, claim_worker_id = NULL, claim_taken_at = NULL
         WHERE outbox_id = $1 AND status IN ('dispatching', 'outcome_unknown')",
    )
    .bind(outbox_id.as_uuid())
    .bind(at)
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if updated.rows_affected() == 1 {
        return Ok(());
    }
    accept_if(conn, outbox_id, OutboxStatus::Completed).await
}

/// Records a failure, either requeueing the row or settling it.
pub(crate) async fn mark_failed(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
    reason: String,
    retry_at: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    match retry_at {
        Some(retry_at) => requeue(conn, outbox_id, Some(reason), retry_at).await,
        None => fail(conn, outbox_id, reason, at).await,
    }
}

/// Puts a row back in the queue, due at `retry_at`.
async fn requeue(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
    reason: Option<String>,
    retry_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let statement = format!(
        "UPDATE tf_outbox
         SET status = 'pending', next_attempt_at = $2,
             last_failure = COALESCE($3, last_failure),
             claim_worker_id = NULL, claim_taken_at = NULL
         WHERE outbox_id = $1 AND status NOT IN {TERMINAL_STATUSES}"
    );
    let updated = sqlx::query(&statement)
        .bind(outbox_id.as_uuid())
        .bind(retry_at)
        .bind(reason)
        .execute(&mut *conn)
        .await
        .map_err(|error| store_error(&error))?;
    if updated.rows_affected() == 1 {
        return Ok(());
    }
    Err(missing_or_conflict(conn, outbox_id).await?)
}

/// Settles a row as definitively refused.
async fn fail(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
    reason: String,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE tf_outbox
         SET status = 'failed', completed_at = $2, last_failure = $3,
             claim_worker_id = NULL, claim_taken_at = NULL
         WHERE outbox_id = $1 AND status IN ('dispatching', 'outcome_unknown')",
    )
    .bind(outbox_id.as_uuid())
    .bind(at)
    .bind(reason.as_str())
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if updated.rows_affected() == 1 {
        return Ok(());
    }
    // Repeating a definite failure is accepted, and records the latest reason.
    let repeated = sqlx::query(
        "UPDATE tf_outbox SET last_failure = $2 WHERE outbox_id = $1 AND status = 'failed'",
    )
    .bind(outbox_id.as_uuid())
    .bind(reason.as_str())
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if repeated.rows_affected() == 1 {
        return Ok(());
    }
    Err(missing_or_conflict(conn, outbox_id).await?)
}

/// Records that the remote was called and the result is unknown (I15).
pub(crate) async fn mark_outcome_unknown(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
    remote_ref: Option<String>,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE tf_outbox
         SET status = 'outcome_unknown', remote_ref = $2,
             claim_worker_id = NULL, claim_taken_at = NULL
         WHERE outbox_id = $1 AND status = 'dispatching'",
    )
    .bind(outbox_id.as_uuid())
    .bind(remote_ref.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if updated.rows_affected() == 1 {
        return Ok(());
    }
    // Reconciliation may learn the remote reference after the fact, and losing
    // it would leave the row impossible to reconcile.
    let repeated = sqlx::query(
        "UPDATE tf_outbox SET remote_ref = COALESCE(remote_ref, $2)
         WHERE outbox_id = $1 AND status = 'outcome_unknown'",
    )
    .bind(outbox_id.as_uuid())
    .bind(remote_ref.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if repeated.rows_affected() == 1 {
        return Ok(());
    }
    Err(missing_or_conflict(conn, outbox_id).await?)
}

/// Returns a row to the queue at a chosen time.
pub(crate) async fn reschedule(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
    next_attempt_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    requeue(conn, outbox_id, None, next_attempt_at).await
}

/// Frees the rows a dispatcher died holding.
pub(crate) async fn release_expired_claims(
    conn: &mut PgConnection,
    claimed_before: DateTime<Utc>,
) -> Result<Vec<OutboxId>, StoreError> {
    let rows = sqlx::query(
        "UPDATE tf_outbox
         SET status = 'pending', next_attempt_at = NULL,
             claim_worker_id = NULL, claim_taken_at = NULL
         WHERE status = 'dispatching' AND claim_taken_at < $1
         RETURNING outbox_id",
    )
    .bind(claimed_before)
    .fetch_all(conn)
    .await
    .map_err(|error| store_error(&error))?;
    let mut released = rows
        .iter()
        .map(|row| column::<Uuid>(row, "outbox_id").map(OutboxId::from))
        .collect::<Result<Vec<OutboxId>, StoreError>>()?;
    released.sort_unstable();
    Ok(released)
}

/// Accepts a transition that has already happened, and refuses anything else.
async fn accept_if(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
    settled: OutboxStatus,
) -> Result<(), StoreError> {
    match read_status(conn, outbox_id).await? {
        None => Err(StoreError::NotFound),
        Some(status) if status == settled => Ok(()),
        Some(_) => Err(StoreError::Conflict),
    }
}

/// Whether a compare-and-swap found no row because it does not exist or because
/// it had moved on.
async fn missing_or_conflict(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
) -> Result<StoreError, StoreError> {
    Ok(match read_status(conn, outbox_id).await? {
        None => StoreError::NotFound,
        Some(_) => StoreError::Conflict,
    })
}

/// The current status of a row, if it exists.
async fn read_status(
    conn: &mut PgConnection,
    outbox_id: &OutboxId,
) -> Result<Option<OutboxStatus>, StoreError> {
    let row = sqlx::query("SELECT status FROM tf_outbox WHERE outbox_id = $1")
        .bind(outbox_id.as_uuid())
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))?;
    match row {
        None => Ok(None),
        Some(row) => {
            let status: String = column(&row, "status")?;
            from_label(&status).map(Some)
        }
    }
}

/// Rebuilds a record from its row.
fn decode_record(row: &PgRow) -> Result<OutboxRecord, StoreError> {
    let outbox_id: Uuid = column(row, "outbox_id")?;
    let command_id: Uuid = column(row, "command_id")?;
    let idempotency_key: String = column(row, "idempotency_key")?;
    let status: String = column(row, "status")?;
    let attempts: i32 = column(row, "attempt_count")?;
    let worker_id: Option<String> = column(row, "claim_worker_id")?;
    let claimed_at: Option<DateTime<Utc>> = column(row, "claim_taken_at")?;
    Ok(OutboxRecord {
        entry: OutboxEntry {
            outbox_id: OutboxId::from(outbox_id),
            command_id: CommandId::from(command_id),
            destination: column(row, "destination")?,
            payload: column(row, "payload")?,
            idempotency_key: IdempotencyKey::new(idempotency_key),
            status: from_label(&status)?,
            attempt_count: attempts_from_sql(attempts)?,
            next_attempt_at: column(row, "next_attempt_at")?,
            created_at: column(row, "created_at")?,
            completed_at: column(row, "completed_at")?,
        },
        claim: worker_id
            .zip(claimed_at)
            .map(|(worker_id, claimed_at)| OutboxClaim {
                worker_id,
                claimed_at,
            }),
        last_failure: column(row, "last_failure")?,
        remote_ref: column(row, "remote_ref")?,
    })
}

#[async_trait]
impl OutboxReader for PgStores {
    async fn get(&self, outbox_id: &OutboxId) -> Result<OutboxRecord, StoreError> {
        let mut conn = self.connection().await?;
        get(&mut conn, outbox_id).await
    }

    async fn list_for_command(
        &self,
        command_id: &CommandId,
    ) -> Result<Vec<OutboxRecord>, StoreError> {
        let mut conn = self.connection().await?;
        list_for_command(&mut conn, command_id).await
    }
}

#[async_trait]
impl OutboxWriter for PgStores {
    async fn enqueue(&self, entry: OutboxEntry) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        enqueue(&mut transaction, entry).await?;
        commit(transaction).await
    }

    async fn claim_due(
        &self,
        at: DateTime<Utc>,
        limit: usize,
        worker_id: &str,
    ) -> Result<Vec<OutboxEntry>, StoreError> {
        let mut transaction = self.transaction().await?;
        let claimed = claim_due(&mut transaction, at, limit, worker_id).await?;
        commit(transaction).await?;
        Ok(claimed)
    }

    async fn mark_completed(&self, outbox_id: &OutboxId) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        mark_completed(&mut transaction, outbox_id, now()).await?;
        commit(transaction).await
    }

    async fn mark_failed(
        &self,
        outbox_id: &OutboxId,
        reason: String,
        retry_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        mark_failed(&mut transaction, outbox_id, reason, retry_at, now()).await?;
        commit(transaction).await
    }

    async fn mark_outcome_unknown(
        &self,
        outbox_id: &OutboxId,
        remote_ref: Option<String>,
    ) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        mark_outcome_unknown(&mut transaction, outbox_id, remote_ref).await?;
        commit(transaction).await
    }

    async fn reschedule(
        &self,
        outbox_id: &OutboxId,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        reschedule(&mut transaction, outbox_id, next_attempt_at).await?;
        commit(transaction).await
    }

    async fn release_expired_claims(
        &self,
        claimed_before: DateTime<Utc>,
    ) -> Result<Vec<OutboxId>, StoreError> {
        let mut transaction = self.transaction().await?;
        let released = release_expired_claims(&mut transaction, claimed_before).await?;
        commit(transaction).await?;
        Ok(released)
    }
}
