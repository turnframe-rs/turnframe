//! The claim ledger over `tf_domain_event` (spec §17.1, ADR-012).
//!
//! Nothing here deletes, and nothing inserts except the batch insert below.
//! `sequence` is an identity column, so the store assigns the position rather
//! than the caller, and a read ordered by it returns events in the order they
//! were committed.
//!
//! There is exactly one `UPDATE` in this module, [`redact_payload`], and it is
//! how personal data leaves a ledger that may not lose an event. It writes
//! `payload`, `redacted_at` and `redaction_authority` and nothing else, so the
//! primary key, the identity column, the case columns and `occurred_at` are
//! untouched by the statement itself rather than by a promise: an event cannot
//! change position because no statement here can write its position.
//!
//! A batch is one statement. A duplicate identifier — whether it collides with
//! a stored event or with another event of the same batch — violates the
//! primary key, and the statement fails whole: a receipt can never be backed by
//! half of a commit.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use sqlx::postgres::PgRow;
use turnframe_core::case::CaseKey;
use turnframe_core::event::EventRedaction;
use turnframe_core::ids::{AccountId, CaseRevision, CommandId, EventId, RedactionAuthority};
use turnframe_store::error::{StoreError, invalid_record};
use turnframe_store::events::{
    EventBatch, EventCursor, EventJournalReader, EventJournalWriter, EventPage, StoredEvent,
};
use uuid::Uuid;

use crate::codec::{
    column, count_from_sql, cursor_to_sql, limit_to_sql, revision_from_sql, revision_to_sql,
    sequence_from_sql,
};
use crate::error::store_error;
use crate::store::{PgStores, commit};

/// Everything a read needs to rebuild a [`StoredEvent`].
const EVENT_COLUMNS: &str = "sequence, event_id, workflow_key, case_id, case_revision, \
     command_id, event_type, payload, occurred_at, redacted_at, redaction_authority";

/// Appends a batch and returns its identifiers in order.
pub(crate) async fn append(
    conn: &mut PgConnection,
    batch: EventBatch,
) -> Result<Vec<EventId>, StoreError> {
    if batch.is_empty() {
        return Err(invalid_record());
    }
    let ids: Vec<Uuid> = batch.events.iter().map(|e| *e.event_id.as_uuid()).collect();
    let types: Vec<String> = batch.events.iter().map(|e| e.event_type.clone()).collect();
    let payloads: Vec<serde_json::Value> = batch.events.iter().map(|e| e.payload.clone()).collect();
    let occurred: Vec<DateTime<Utc>> = batch.events.iter().map(|e| e.occurred_at).collect();
    // `WITH ORDINALITY` plus `ORDER BY` fixes the order rows are inserted in, so
    // the identity column hands out positions in the batch's own order.
    sqlx::query(
        "INSERT INTO tf_domain_event (
             account_id, event_id, workflow_key, case_id, case_revision, command_id,
             event_type, payload, occurred_at
         )
         SELECT $1, e.event_id, $2, $3, $4, $5, e.event_type, e.payload, e.occurred_at
         FROM unnest($6::uuid[], $7::text[], $8::jsonb[], $9::timestamptz[])
              WITH ORDINALITY AS e(event_id, event_type, payload, occurred_at, position)
         ORDER BY e.position",
    )
    .bind(batch.account_id.as_str())
    .bind(batch.case_key.workflow.as_str())
    .bind(batch.case_key.case_id.as_str())
    .bind(revision_to_sql(batch.revision)?)
    .bind(batch.command_id.as_uuid())
    .bind(ids)
    .bind(types)
    .bind(payloads)
    .bind(occurred)
    .execute(conn)
    .await
    .map_err(|error| store_error(&error))?;
    Ok(batch.event_ids())
}

/// The events of a case past `since`, in append order.
pub(crate) async fn list_since(
    conn: &mut PgConnection,
    account: &AccountId,
    case_key: &CaseKey,
    since: CaseRevision,
    limit: usize,
) -> Result<Vec<StoredEvent>, StoreError> {
    let statement = format!(
        "SELECT {EVENT_COLUMNS} FROM tf_domain_event
         WHERE account_id = $1 AND workflow_key = $2 AND case_id = $3 AND case_revision > $4
         ORDER BY sequence
         LIMIT $5"
    );
    let rows = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(case_key.workflow.as_str())
        .bind(case_key.case_id.as_str())
        .bind(revision_to_sql(since)?)
        .bind(limit_to_sql(limit))
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    rows.iter().map(|row| decode_event(row, account)).collect()
}

/// The account's next events after `after`, across every case.
///
/// The cursor is the store-assigned sequence, and the sequence is the journal's
/// total order, so `sequence > cursor` is both the paging condition and the
/// ordering: an append always lands beyond every cursor already issued, and a
/// page can therefore never repeat or reorder what an earlier one delivered.
/// Another tenant's events are excluded by the `WHERE` clause rather than
/// filtered afterwards, so they never consume a page's limit.
pub(crate) async fn read_from(
    conn: &mut PgConnection,
    account: &AccountId,
    after: EventCursor,
    limit: usize,
) -> Result<EventPage, StoreError> {
    let statement = format!(
        "SELECT {EVENT_COLUMNS} FROM tf_domain_event
         WHERE account_id = $1 AND sequence > $2
         ORDER BY sequence
         LIMIT $3"
    );
    let rows = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(cursor_to_sql(after))
        .bind(limit_to_sql(limit))
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    let events = rows
        .iter()
        .map(|row| decode_event(row, account))
        .collect::<Result<Vec<StoredEvent>, StoreError>>()?;
    Ok(EventPage::new(events, after))
}

/// The events with the given identifiers, in the order they were asked for.
///
/// Receipt verification reads the ledger back this way, so an identifier that
/// belongs to another tenant is simply absent from the answer rather than an
/// error: a receipt citing it fails to verify, which is the point.
pub(crate) async fn get_by_ids(
    conn: &mut PgConnection,
    account: &AccountId,
    ids: &[EventId],
) -> Result<Vec<StoredEvent>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let wanted: Vec<Uuid> = ids.iter().map(|id| *id.as_uuid()).collect();
    let statement = format!(
        "SELECT {EVENT_COLUMNS} FROM tf_domain_event
         WHERE account_id = $1 AND event_id = ANY($2)"
    );
    let rows = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(wanted)
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    let found = rows
        .iter()
        .map(|row| decode_event(row, account))
        .collect::<Result<Vec<StoredEvent>, StoreError>>()?;
    Ok(ids
        .iter()
        .filter_map(|id| found.iter().find(|event| event.event_id == *id).cloned())
        .collect())
}

/// Empties one event's payload and records the erasure on the same row.
///
/// One statement does both, so there is no window in which a payload is gone
/// and nobody knows who removed it. `COALESCE` is what makes a repeated request
/// a no-op that keeps the **first** record: an erasure retried by an operator
/// or a queue must not rewrite the history of who erased what, and the second
/// call has nothing left to erase. `RETURNING` then hands back the record the
/// row now carries, whichever call wrote it.
///
/// The `WHERE` clause is account-scoped, so an identifier belonging to another
/// tenant updates nothing and is reported as `NotFound`, exactly like one that
/// never existed: a write is not a way to probe for a neighbour's data.
pub(crate) async fn redact_payload(
    conn: &mut PgConnection,
    account: &AccountId,
    event_id: &EventId,
    authority: &RedactionAuthority,
) -> Result<EventRedaction, StoreError> {
    let row = sqlx::query(
        "UPDATE tf_domain_event
            SET payload = 'null'::jsonb,
                redacted_at = COALESCE(redacted_at, now()),
                redaction_authority = COALESCE(redaction_authority, $3)
          WHERE account_id = $1 AND event_id = $2
      RETURNING redacted_at, redaction_authority",
    )
    .bind(account.as_str())
    .bind(event_id.as_uuid())
    .bind(authority.as_str())
    .fetch_optional(conn)
    .await
    .map_err(|error| store_error(&error))?
    .ok_or(StoreError::NotFound)?;
    decode_redaction(&row)?.ok_or(StoreError::Corrupt)
}

/// How many events a case has.
pub(crate) async fn count(
    conn: &mut PgConnection,
    account: &AccountId,
    case_key: &CaseKey,
) -> Result<u64, StoreError> {
    let row = sqlx::query(
        "SELECT count(*) AS total FROM tf_domain_event
         WHERE account_id = $1 AND workflow_key = $2 AND case_id = $3",
    )
    .bind(account.as_str())
    .bind(case_key.workflow.as_str())
    .bind(case_key.case_id.as_str())
    .fetch_one(conn)
    .await
    .map_err(|error| store_error(&error))?;
    count_from_sql(column(&row, "total")?)
}

/// Rebuilds the erasure record of a row, when it carries one.
///
/// The two columns are constrained to agree, so half a record is stored data
/// violating an invariant the adapter relies on: `Corrupt`, not a guess.
fn decode_redaction(row: &PgRow) -> Result<Option<EventRedaction>, StoreError> {
    let redacted_at: Option<DateTime<Utc>> = column(row, "redacted_at")?;
    let authority: Option<String> = column(row, "redaction_authority")?;
    match (redacted_at, authority) {
        (None, None) => Ok(None),
        (Some(redacted_at), Some(authority)) => Ok(Some(EventRedaction {
            redacted_at,
            authority: RedactionAuthority::from(authority),
        })),
        _ => Err(StoreError::Corrupt),
    }
}

/// Rebuilds a stored event from its row.
fn decode_event(row: &PgRow, account: &AccountId) -> Result<StoredEvent, StoreError> {
    let sequence: i64 = column(row, "sequence")?;
    let event_id: Uuid = column(row, "event_id")?;
    let workflow_key: String = column(row, "workflow_key")?;
    let case_id: String = column(row, "case_id")?;
    let revision: i64 = column(row, "case_revision")?;
    let command_id: Uuid = column(row, "command_id")?;
    Ok(StoredEvent {
        sequence: sequence_from_sql(sequence)?,
        event_id: EventId::from(event_id),
        account_id: account.clone(),
        case_key: CaseKey::new(workflow_key, case_id),
        case_revision: revision_from_sql(revision)?,
        command_id: CommandId::from(command_id),
        event_type: column(row, "event_type")?,
        payload: column(row, "payload")?,
        occurred_at: column(row, "occurred_at")?,
        redaction: decode_redaction(row)?,
    })
}

#[async_trait]
impl EventJournalReader for PgStores {
    async fn list_since(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        since: CaseRevision,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let mut conn = self.connection().await?;
        list_since(&mut conn, account, case_key, since, limit).await
    }

    async fn read_from(
        &self,
        account: &AccountId,
        after: EventCursor,
        limit: usize,
    ) -> Result<EventPage, StoreError> {
        let mut conn = self.connection().await?;
        read_from(&mut conn, account, after, limit).await
    }

    async fn get_by_ids(
        &self,
        account: &AccountId,
        ids: &[EventId],
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let mut conn = self.connection().await?;
        get_by_ids(&mut conn, account, ids).await
    }

    async fn count(&self, account: &AccountId, case_key: &CaseKey) -> Result<u64, StoreError> {
        let mut conn = self.connection().await?;
        count(&mut conn, account, case_key).await
    }
}

#[async_trait]
impl EventJournalWriter for PgStores {
    async fn append(&self, batch: EventBatch) -> Result<Vec<EventId>, StoreError> {
        let mut transaction = self.transaction().await?;
        let ids = append(&mut transaction, batch).await?;
        commit(transaction).await?;
        Ok(ids)
    }

    async fn redact_payload(
        &self,
        account: &AccountId,
        event_id: &EventId,
        authority: &RedactionAuthority,
    ) -> Result<EventRedaction, StoreError> {
        let mut transaction = self.transaction().await?;
        let record = redact_payload(&mut transaction, account, event_id, authority).await?;
        commit(transaction).await?;
        Ok(record)
    }
}
