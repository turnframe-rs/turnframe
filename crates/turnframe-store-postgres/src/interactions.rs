//! Interactions over `tf_interaction`: the one-blocking-card slot,
//! compare-and-swap resolution, revision invalidation and expiry.
//!
//! # Where the rules live
//!
//! The slot of I5 — at most one open blocking card per case — is a partial
//! unique index, not a check this adapter performs. Two concurrent inserts do
//! not race for it: one of them loses on the index and is refused with
//! `Conflict` having written nothing.
//!
//! Every status change is the `WHERE` clause of the statement that writes it, so
//! the state a caller expected and the write that depends on it can never be
//! separated. Zero affected rows means the precondition failed, and a second,
//! read-only statement then says whether that was because the row does not
//! exist for this tenant (`NotFound`) or because it had moved on (`Conflict`).
//!
//! # What a row holds
//!
//! `interaction` is the whole [`Interaction`] as it was created and is never
//! rewritten. Beside it are the keys the indexes need and the lifecycle state
//! this store owns — status, the chosen option, the resolving turn, the events
//! that backed a resolution, the invalidation. A read deserializes the document
//! and overlays those columns, so there is exactly one source of truth for
//! everything that moves.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use sqlx::postgres::PgRow;
use turnframe_core::case::CaseKey;
use turnframe_core::ids::{
    AccountId, CaseRevision, ConversationId, EventId, InteractionId, OptionId, TurnId,
};
use turnframe_core::interaction::{Interaction, InteractionStatus};
use turnframe_store::error::{StoreError, invalid_record};
use turnframe_store::interaction::{
    InteractionReader, InteractionRecord, InteractionWriter, InvalidationReason,
    InvalidationRecord, ResolutionOutcome,
};
use uuid::Uuid;

use crate::codec::{column, from_json, from_label, label, now, revision_to_sql, to_json};
use crate::error::store_error;
use crate::store::{PgStores, commit};

/// Everything a read needs to rebuild an [`InteractionRecord`].
const RECORD_COLUMNS: &str = "interaction, status, resolved_at, resolved_option_id, \
     resolved_by_turn, resolution_event_ids, failure_code, invalidation";

/// The statuses that hold the blocking slot, as the SQL literals the partial
/// unique index uses.
const OPEN_STATUSES: &str = "('active', 'resolving')";
/// The statuses a user's own answer leaves behind.
const ANSWERED_STATUSES: &str = "('resolved', 'declined', 'dismissed')";

/// Inserts a new card, optionally replacing the blocking occupant of its case.
///
/// Returns the cards it invalidated: the occupant when one was replaced, empty
/// otherwise.
pub(crate) async fn insert_interaction(
    conn: &mut PgConnection,
    interaction: Interaction,
    replace_blocking: bool,
    at: DateTime<Utc>,
) -> Result<Vec<InteractionId>, StoreError> {
    if interaction.status != InteractionStatus::Active {
        return Err(invalid_record());
    }
    let mut invalidated = Vec::new();
    if interaction.blocking
        && let Some((occupant, status)) = lock_blocking_slot(
            &mut *conn,
            &interaction.account_id,
            &interaction.case_ref.key(),
        )
        .await?
    {
        // A card whose commands are executing is never swept away
        // underneath them, whatever the caller asked for.
        if !replace_blocking || status != InteractionStatus::Active {
            return Err(StoreError::Conflict);
        }
        invalidate(
            &mut *conn,
            &interaction.account_id,
            &occupant,
            &InvalidationRecord {
                reason: InvalidationReason::Superseded { by: interaction.id },
                new_revision: None,
                at,
            },
        )
        .await?;
        invalidated.push(occupant);
    }
    write_row(conn, &interaction).await?;
    Ok(invalidated)
}

/// Takes the blocking slot of a case, so a concurrent replacement waits instead
/// of racing, and reports who holds it.
async fn lock_blocking_slot(
    conn: &mut PgConnection,
    account: &AccountId,
    case: &CaseKey,
) -> Result<Option<(InteractionId, InteractionStatus)>, StoreError> {
    let statement = format!(
        "SELECT interaction_id, status FROM tf_interaction
         WHERE account_id = $1 AND workflow_key = $2 AND case_id = $3
           AND blocking AND status IN {OPEN_STATUSES}
         FOR UPDATE"
    );
    let row = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(case.workflow.as_str())
        .bind(case.case_id.as_str())
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: Uuid = column(&row, "interaction_id")?;
    let status: String = column(&row, "status")?;
    Ok(Some((InteractionId::from(id), from_label(&status)?)))
}

/// Writes the row of a freshly created card.
async fn write_row(conn: &mut PgConnection, interaction: &Interaction) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO tf_interaction (
             account_id, interaction_id, conversation_id, workflow_key, case_id, case_revision,
             kind, blocking, revision_independent, payload_hash, interaction, status,
             created_at, expires_at, resolved_at, resolved_option_id
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
    )
    .bind(interaction.account_id.as_str())
    .bind(interaction.id.as_uuid())
    .bind(interaction.conversation_id.as_uuid())
    .bind(interaction.case_ref.workflow.as_str())
    .bind(interaction.case_ref.case_id.as_str())
    .bind(revision_to_sql(interaction.case_ref.expected_revision)?)
    .bind(label(&interaction.kind)?)
    .bind(interaction.blocking)
    .bind(interaction.revision_independent)
    .bind(interaction.payload_hash.as_str())
    .bind(to_json(interaction)?)
    .bind(label(&interaction.status)?)
    .bind(interaction.created_at)
    .bind(interaction.expires_at)
    .bind(interaction.resolved_at)
    .bind(
        interaction
            .resolved_option_id
            .as_ref()
            .map(OptionId::as_str),
    )
    .execute(conn)
    .await
    .map_err(|error| store_error(&error))?;
    Ok(())
}

/// Moves one `Active` card to `Invalidated`, recording why.
async fn invalidate(
    conn: &mut PgConnection,
    account: &AccountId,
    id: &InteractionId,
    record: &InvalidationRecord,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE tf_interaction SET status = 'invalidated', invalidation = $3
         WHERE account_id = $1 AND interaction_id = $2 AND status = 'active'",
    )
    .bind(account.as_str())
    .bind(id.as_uuid())
    .bind(to_json(record)?)
    .execute(conn)
    .await
    .map_err(|error| store_error(&error))?;
    if updated.rows_affected() == 0 {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

/// Loads a card of `account`.
pub(crate) async fn get_interaction(
    conn: &mut PgConnection,
    account: &AccountId,
    id: &InteractionId,
) -> Result<InteractionRecord, StoreError> {
    read_record(conn, account, id)
        .await?
        .ok_or(StoreError::NotFound)
}

/// Reads a card without deciding what its absence means.
async fn read_record(
    conn: &mut PgConnection,
    account: &AccountId,
    id: &InteractionId,
) -> Result<Option<InteractionRecord>, StoreError> {
    let statement = format!(
        "SELECT {RECORD_COLUMNS} FROM tf_interaction
         WHERE account_id = $1 AND interaction_id = $2"
    );
    let row = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(id.as_uuid())
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))?;
    row.as_ref().map(decode_record).transpose()
}

/// The open cards of a conversation, oldest first.
pub(crate) async fn list_open_for_conversation(
    conn: &mut PgConnection,
    account: &AccountId,
    conversation: &ConversationId,
) -> Result<Vec<Interaction>, StoreError> {
    let statement = format!(
        "SELECT {RECORD_COLUMNS} FROM tf_interaction
         WHERE account_id = $1 AND conversation_id = $2 AND status IN {OPEN_STATUSES}
         ORDER BY created_at, interaction_id"
    );
    let rows = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(conversation.as_uuid())
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    decode_interactions(&rows)
}

/// The open cards of a case, oldest first.
pub(crate) async fn list_open_for_case(
    conn: &mut PgConnection,
    account: &AccountId,
    case_key: &CaseKey,
) -> Result<Vec<Interaction>, StoreError> {
    let statement = format!(
        "SELECT {RECORD_COLUMNS} FROM tf_interaction
         WHERE account_id = $1 AND workflow_key = $2 AND case_id = $3
           AND status IN {OPEN_STATUSES}
         ORDER BY created_at, interaction_id"
    );
    let rows = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(case_key.workflow.as_str())
        .bind(case_key.case_id.as_str())
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    decode_interactions(&rows)
}

/// Whether the user has answered a blocking card of this case at this revision.
///
/// The three statuses are the ones a user causes; a card the case outgrew or
/// that timed out was answered by nobody.
pub(crate) async fn blocking_answered_at(
    conn: &mut PgConnection,
    account: &AccountId,
    case_key: &CaseKey,
    revision: CaseRevision,
) -> Result<bool, StoreError> {
    let statement = format!(
        "SELECT 1 FROM tf_interaction
         WHERE account_id = $1 AND workflow_key = $2 AND case_id = $3
           AND case_revision = $4 AND blocking AND status IN {ANSWERED_STATUSES}
         LIMIT 1"
    );
    let found = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(case_key.workflow.as_str())
        .bind(case_key.case_id.as_str())
        .bind(revision_to_sql(revision)?)
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))?;
    Ok(found.is_some())
}

/// Compare-and-swap `Active → Resolving`, recording the answer.
pub(crate) async fn begin_resolution(
    conn: &mut PgConnection,
    account: &AccountId,
    id: &InteractionId,
    expected_status: InteractionStatus,
    option_id: OptionId,
    resolved_by: TurnId,
    at: DateTime<Utc>,
) -> Result<InteractionRecord, StoreError> {
    // `Active` is the only status a resolution may start from, so any other
    // expectation cannot be satisfied and no write is attempted.
    let updated = if expected_status == InteractionStatus::Active {
        let statement = format!(
            "UPDATE tf_interaction
             SET status = 'resolving', resolved_option_id = $3, resolved_at = $4,
                 resolved_by_turn = $5
             WHERE account_id = $1 AND interaction_id = $2 AND status = 'active'
             RETURNING {RECORD_COLUMNS}"
        );
        sqlx::query(&statement)
            .bind(account.as_str())
            .bind(id.as_uuid())
            .bind(option_id.as_str())
            .bind(at)
            .bind(resolved_by.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(|error| store_error(&error))?
    } else {
        None
    };
    match updated {
        Some(row) => decode_record(&row),
        None => Err(missing_or_conflict(conn, account, id).await?),
    }
}

/// Settles a `Resolving` card.
///
/// Repeating a settlement with the same data is accepted so that recovery may
/// replay a bundle; settling it differently is a `Conflict`.
pub(crate) async fn finish_resolution(
    conn: &mut PgConnection,
    account: &AccountId,
    id: &InteractionId,
    outcome: ResolutionOutcome,
) -> Result<InteractionRecord, StoreError> {
    let updated = settle(&mut *conn, account, id, &outcome).await?;
    if let Some(row) = updated {
        return decode_record(&row);
    }
    let Some(record) = read_record(conn, account, id).await? else {
        return Err(StoreError::NotFound);
    };
    if record.status() == outcome.target_status() && already_settled(&record, &outcome) {
        return Ok(record);
    }
    Err(StoreError::Conflict)
}

/// The compare-and-swap out of `Resolving`, one statement per outcome.
async fn settle(
    conn: &mut PgConnection,
    account: &AccountId,
    id: &InteractionId,
    outcome: &ResolutionOutcome,
) -> Result<Option<PgRow>, StoreError> {
    let statement = match outcome {
        ResolutionOutcome::Resolved { .. } => format!(
            "UPDATE tf_interaction
             SET status = 'resolved', resolution_event_ids = $3, failure_code = NULL
             WHERE account_id = $1 AND interaction_id = $2 AND status = 'resolving'
             RETURNING {RECORD_COLUMNS}"
        ),
        ResolutionOutcome::Failed { .. } => format!(
            "UPDATE tf_interaction
             SET status = 'failed', failure_code = $3, resolution_event_ids = '{{}}'
             WHERE account_id = $1 AND interaction_id = $2 AND status = 'resolving'
             RETURNING {RECORD_COLUMNS}"
        ),
        // Restoring puts the card back in the user's hands, so every trace of
        // the answer goes with it.
        ResolutionOutcome::RestoreActive => format!(
            "UPDATE tf_interaction
             SET status = 'active', resolution_event_ids = '{{}}', failure_code = NULL,
                 resolved_by_turn = NULL, resolved_option_id = NULL, resolved_at = NULL
             WHERE account_id = $1 AND interaction_id = $2 AND status = 'resolving'
             RETURNING {RECORD_COLUMNS}"
        ),
    };
    let query = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(id.as_uuid());
    let query = match outcome {
        ResolutionOutcome::Resolved { event_ids } => query.bind(
            event_ids
                .iter()
                .map(|id| *id.as_uuid())
                .collect::<Vec<Uuid>>(),
        ),
        ResolutionOutcome::Failed { code } => query.bind(code.clone()),
        ResolutionOutcome::RestoreActive => query,
    };
    query
        .fetch_optional(conn)
        .await
        .map_err(|error| store_error(&error))
}

/// Returns `true` when the card already carries exactly what `outcome` would
/// write, which makes repeating the call a no-op rather than a conflict.
fn already_settled(record: &InteractionRecord, outcome: &ResolutionOutcome) -> bool {
    match outcome {
        ResolutionOutcome::Resolved { event_ids } => &record.resolution_event_ids == event_ids,
        ResolutionOutcome::Failed { code } => record.failure_code.as_deref() == Some(code.as_str()),
        ResolutionOutcome::RestoreActive => {
            record.resolved_by_turn.is_none() && record.interaction.resolved_option_id.is_none()
        }
    }
}

/// Invalidates every `Active` card of the case that is bound to another
/// revision.
///
/// The selection and the write are one statement, so two commits moving the same
/// case race on the row lock rather than on a snapshot: the second one re-reads
/// the row, finds it already invalidated, and reports nothing invalidated
/// instead of overwriting the first one's work.
/// Invalidates every active card of a case whatever revision it is bound to.
///
/// Deliberately ignores `revision_independent` and records no revision: the
/// case did not move, and writing one would say it had.
pub(crate) async fn invalidate_case_cards(
    conn: &mut PgConnection,
    account: &AccountId,
    case_key: &CaseKey,
    reason: InvalidationReason,
    at: DateTime<Utc>,
) -> Result<Vec<InteractionId>, StoreError> {
    let record = InvalidationRecord {
        reason,
        new_revision: None,
        at,
    };
    let rows = sqlx::query(
        "UPDATE tf_interaction SET status = 'invalidated', invalidation = $4
         WHERE account_id = $1 AND workflow_key = $2 AND case_id = $3
           AND status = 'active'
         RETURNING interaction_id, created_at",
    )
    .bind(account.as_str())
    .bind(case_key.workflow.as_str())
    .bind(case_key.case_id.as_str())
    .bind(to_json(&record)?)
    .fetch_all(conn)
    .await
    .map_err(|error| store_error(&error))?;
    // `RETURNING` has no order of its own, and the contract names one.
    let mut invalidated = rows
        .iter()
        .map(|row| {
            let id: Uuid = column(row, "interaction_id")?;
            let created_at: DateTime<Utc> = column(row, "created_at")?;
            Ok((created_at, InteractionId::from(id)))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    invalidated.sort_unstable();
    Ok(invalidated.into_iter().map(|(_, id)| id).collect())
}

pub(crate) async fn invalidate_for_case(
    conn: &mut PgConnection,
    account: &AccountId,
    case_key: &CaseKey,
    new_revision: CaseRevision,
    reason: InvalidationReason,
    at: DateTime<Utc>,
) -> Result<Vec<InteractionId>, StoreError> {
    let record = InvalidationRecord {
        reason,
        new_revision: Some(new_revision),
        at,
    };
    let rows = sqlx::query(
        "UPDATE tf_interaction SET status = 'invalidated', invalidation = $5
         WHERE account_id = $1 AND workflow_key = $2 AND case_id = $3
           AND status = 'active'
           AND NOT revision_independent
           AND case_revision <> $4
         RETURNING interaction_id, created_at",
    )
    .bind(account.as_str())
    .bind(case_key.workflow.as_str())
    .bind(case_key.case_id.as_str())
    .bind(revision_to_sql(new_revision)?)
    .bind(to_json(&record)?)
    .fetch_all(conn)
    .await
    .map_err(|error| store_error(&error))?;
    // `RETURNING` has no order of its own, and the contract names one.
    let mut invalidated = rows
        .iter()
        .map(|row| {
            let id: Uuid = column(row, "interaction_id")?;
            let created_at: DateTime<Utc> = column(row, "created_at")?;
            Ok((created_at, InteractionId::from(id)))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    invalidated.sort_unstable();
    Ok(invalidated.into_iter().map(|(_, id)| id).collect())
}

/// Expires every `Active` card whose deadline has passed, across tenants.
pub(crate) async fn expire_due(
    conn: &mut PgConnection,
    at: DateTime<Utc>,
) -> Result<Vec<InteractionId>, StoreError> {
    let rows = sqlx::query(
        "UPDATE tf_interaction SET status = 'expired'
         WHERE status = 'active' AND expires_at IS NOT NULL AND expires_at <= $1
         RETURNING account_id, interaction_id",
    )
    .bind(at)
    .fetch_all(conn)
    .await
    .map_err(|error| store_error(&error))?;
    let mut expired = rows
        .iter()
        .map(|row| {
            let account: String = column(row, "account_id")?;
            let id: Uuid = column(row, "interaction_id")?;
            Ok((account, InteractionId::from(id)))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    expired.sort_unstable();
    Ok(expired.into_iter().map(|(_, id)| id).collect())
}

/// Whether a compare-and-swap found no row because the card is not this
/// tenant's, or because it had moved on.
async fn missing_or_conflict(
    conn: &mut PgConnection,
    account: &AccountId,
    id: &InteractionId,
) -> Result<StoreError, StoreError> {
    let exists =
        sqlx::query("SELECT 1 FROM tf_interaction WHERE account_id = $1 AND interaction_id = $2")
            .bind(account.as_str())
            .bind(id.as_uuid())
            .fetch_optional(conn)
            .await
            .map_err(|error| store_error(&error))?
            .is_some();
    Ok(if exists {
        StoreError::Conflict
    } else {
        StoreError::NotFound
    })
}

/// Rebuilds the stored record from its row.
fn decode_record(row: &PgRow) -> Result<InteractionRecord, StoreError> {
    let mut interaction: Interaction = from_json(column(row, "interaction")?)?;
    let status: String = column(row, "status")?;
    // The columns, not the document, are the truth about what has moved.
    interaction.status = from_label(&status)?;
    interaction.resolved_at = column(row, "resolved_at")?;
    interaction.resolved_option_id =
        column::<Option<String>>(row, "resolved_option_id")?.map(OptionId::new);
    let resolved_by: Option<Uuid> = column(row, "resolved_by_turn")?;
    let event_ids: Vec<Uuid> = column(row, "resolution_event_ids")?;
    let invalidation: Option<serde_json::Value> = column(row, "invalidation")?;
    Ok(InteractionRecord {
        interaction,
        resolved_by_turn: resolved_by.map(TurnId::from),
        resolution_event_ids: event_ids.into_iter().map(EventId::from).collect(),
        failure_code: column(row, "failure_code")?,
        invalidation: invalidation.map(from_json).transpose()?,
    })
}

/// Rebuilds the core view of every row.
fn decode_interactions(rows: &[PgRow]) -> Result<Vec<Interaction>, StoreError> {
    rows.iter()
        .map(|row| decode_record(row).map(|record| record.interaction))
        .collect()
}

#[async_trait]
impl InteractionReader for PgStores {
    async fn get(
        &self,
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, StoreError> {
        let mut conn = self.connection().await?;
        get_interaction(&mut conn, account, id).await
    }

    async fn list_open_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Vec<Interaction>, StoreError> {
        let mut conn = self.connection().await?;
        list_open_for_conversation(&mut conn, account, conversation).await
    }

    async fn list_open_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<Interaction>, StoreError> {
        let mut conn = self.connection().await?;
        list_open_for_case(&mut conn, account, case_key).await
    }

    async fn blocking_answered_at(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        revision: CaseRevision,
    ) -> Result<bool, StoreError> {
        let mut conn = self.connection().await?;
        blocking_answered_at(&mut conn, account, case_key, revision).await
    }
}

#[async_trait]
impl InteractionWriter for PgStores {
    async fn insert(&self, interaction: Interaction) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        insert_interaction(&mut transaction, interaction, false, now()).await?;
        commit(transaction).await
    }

    async fn insert_replacing_blocking(
        &self,
        interaction: Interaction,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let mut transaction = self.transaction().await?;
        let invalidated = insert_interaction(&mut transaction, interaction, true, now()).await?;
        commit(transaction).await?;
        Ok(invalidated)
    }

    async fn begin_resolution(
        &self,
        account: &AccountId,
        id: &InteractionId,
        expected_status: InteractionStatus,
        option_id: OptionId,
        resolved_by: TurnId,
    ) -> Result<InteractionRecord, StoreError> {
        let mut transaction = self.transaction().await?;
        let record = begin_resolution(
            &mut transaction,
            account,
            id,
            expected_status,
            option_id,
            resolved_by,
            now(),
        )
        .await?;
        commit(transaction).await?;
        Ok(record)
    }

    async fn finish_resolution(
        &self,
        account: &AccountId,
        id: &InteractionId,
        outcome: ResolutionOutcome,
    ) -> Result<InteractionRecord, StoreError> {
        let mut transaction = self.transaction().await?;
        let record = finish_resolution(&mut transaction, account, id, outcome).await?;
        commit(transaction).await?;
        Ok(record)
    }

    async fn invalidate_case_cards(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let mut transaction = self.transaction().await?;
        let invalidated =
            invalidate_case_cards(&mut transaction, account, case_key, reason, now()).await?;
        commit(transaction).await?;
        Ok(invalidated)
    }

    async fn invalidate_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        new_revision: CaseRevision,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let mut transaction = self.transaction().await?;
        let invalidated = invalidate_for_case(
            &mut transaction,
            account,
            case_key,
            new_revision,
            reason,
            now(),
        )
        .await?;
        commit(transaction).await?;
        Ok(invalidated)
    }

    async fn expire_due(&self, at: DateTime<Utc>) -> Result<Vec<InteractionId>, StoreError> {
        let mut transaction = self.transaction().await?;
        let expired = expire_due(&mut transaction, at).await?;
        commit(transaction).await?;
        Ok(expired)
    }
}
