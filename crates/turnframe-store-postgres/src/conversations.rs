//! Conversations, turns and the crash-recovery phase marker over `tf_conversation`,
//! `tf_turn` and `tf_turn_phase`.
//!
//! The turn row is written once and never rewritten except to attach the
//! assistant turn, and both sides are stored as whole `jsonb` documents: a
//! reload deserializes the exact [`AssistantTurn`] that was returned to the
//! client, with its blocks in their original order, instead of rebuilding cards
//! from prose (spec §22.3).
//!
//! The phase marker lives in its own table because it moves on every step of a
//! turn while the turn itself does not, and because recovery reads only the
//! marker. Its partial index carries just the unfinished turns, so the sweep of
//! spec §23.1 costs nothing on a system with no interrupted turns.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use turnframe_core::ids::{AccountId, ConversationId, TurnId};
use turnframe_core::replay::TurnPhase;
use turnframe_core::response::AssistantTurn;
use turnframe_store::conversation::{
    ConversationReader, ConversationRecord, ConversationWriter, RecoveryScope, StoredTurn,
    StoredUserTurn, TurnPhaseMarker,
};
use turnframe_store::error::{StoreError, identity_mismatch};
use uuid::Uuid;

use crate::codec::{column, from_json, from_label, label, limit_to_sql, now, to_json};
use crate::error::store_error;
use crate::store::{PgStores, commit};

/// The phases a turn never leaves, as the SQL literals the partial index uses.
const TERMINAL_PHASES: &str = "('delivered', 'failed')";

/// Inserts a conversation.
pub(crate) async fn create_conversation(
    conn: &mut PgConnection,
    record: ConversationRecord,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO tf_conversation (account_id, conversation_id, created_at, metadata)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(record.account_id.as_str())
    .bind(record.id.as_uuid())
    .bind(record.created_at)
    .bind(to_json(&record.metadata)?)
    .execute(conn)
    .await
    .map_err(|error| store_error(&error))?;
    Ok(())
}

/// Loads a conversation of `account`.
pub(crate) async fn load_conversation(
    conn: &mut PgConnection,
    account: &AccountId,
    id: &ConversationId,
) -> Result<ConversationRecord, StoreError> {
    let row = sqlx::query(
        "SELECT created_at, metadata FROM tf_conversation
         WHERE account_id = $1 AND conversation_id = $2",
    )
    .bind(account.as_str())
    .bind(id.as_uuid())
    .fetch_optional(conn)
    .await
    .map_err(|error| store_error(&error))?
    .ok_or(StoreError::NotFound)?;
    Ok(ConversationRecord {
        id: *id,
        account_id: account.clone(),
        created_at: column(&row, "created_at")?,
        metadata: column(&row, "metadata")?,
    })
}

/// Appends a user turn and opens its phase marker at `Received`.
///
/// The insert is conditional on the conversation existing, so a turn addressed
/// to a conversation of another tenant is refused by the same `NotFound` an
/// unknown conversation gets, without a separate lookup that could race.
pub(crate) async fn append_user_turn(
    conn: &mut PgConnection,
    turn: StoredUserTurn,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let account = turn.account_id().clone();
    let turn_id = turn.turn_id();
    let conversation = turn.conversation_id();
    let inserted = sqlx::query(
        "INSERT INTO tf_turn (account_id, turn_id, conversation_id, received_at, user_turn)
         SELECT $1, $2, $3, $4, $5
         WHERE EXISTS (
             SELECT 1 FROM tf_conversation
             WHERE account_id = $1 AND conversation_id = $3
         )",
    )
    .bind(account.as_str())
    .bind(turn_id.as_uuid())
    .bind(conversation.as_uuid())
    .bind(turn.received_at)
    .bind(to_json(&turn.input)?)
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if inserted.rows_affected() == 0 {
        return Err(StoreError::NotFound);
    }
    sqlx::query(
        "INSERT INTO tf_turn_phase (account_id, turn_id, conversation_id, phase, updated_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(account.as_str())
    .bind(turn_id.as_uuid())
    .bind(conversation.as_uuid())
    .bind(label(&TurnPhase::Received)?)
    .bind(at)
    .execute(conn)
    .await
    .map_err(|error| store_error(&error))?;
    Ok(())
}

/// Attaches the assistant turn to the user turn it answers.
pub(crate) async fn append_assistant_turn(
    conn: &mut PgConnection,
    account: &AccountId,
    turn: AssistantTurn,
) -> Result<(), StoreError> {
    let attached = sqlx::query(
        "UPDATE tf_turn SET assistant_turn = $4
         WHERE account_id = $1 AND turn_id = $2 AND conversation_id = $3
           AND assistant_turn IS NULL",
    )
    .bind(account.as_str())
    .bind(turn.turn_id.as_uuid())
    .bind(turn.conversation_id.as_uuid())
    .bind(to_json(&turn)?)
    .execute(&mut *conn)
    .await
    .map_err(|error| store_error(&error))?;
    if attached.rows_affected() == 1 {
        return Ok(());
    }
    // Nothing moved: say which of the three reasons it was, in the order the
    // contract specifies — unknown turn, wrong conversation, already answered.
    let row = sqlx::query(
        "SELECT conversation_id, assistant_turn IS NOT NULL AS answered FROM tf_turn
         WHERE account_id = $1 AND turn_id = $2",
    )
    .bind(account.as_str())
    .bind(turn.turn_id.as_uuid())
    .fetch_optional(conn)
    .await
    .map_err(|error| store_error(&error))?
    .ok_or(StoreError::NotFound)?;
    let stored: Uuid = column(&row, "conversation_id")?;
    if stored != *turn.conversation_id.as_uuid() {
        return Err(identity_mismatch());
    }
    Err(StoreError::Conflict)
}

/// The most recent `limit` turns of a conversation, oldest of them first.
pub(crate) async fn load_recent_turns(
    conn: &mut PgConnection,
    account: &AccountId,
    conversation: &ConversationId,
    limit: usize,
) -> Result<Vec<StoredTurn>, StoreError> {
    // The conversation is looked up first so another tenant's conversation is
    // `NotFound` rather than an empty window.
    load_conversation(&mut *conn, account, conversation).await?;
    let rows = sqlx::query(
        "SELECT t.received_at, t.user_turn, t.assistant_turn, p.phase
         FROM tf_turn t
         JOIN tf_turn_phase p ON p.account_id = t.account_id AND p.turn_id = t.turn_id
         WHERE t.account_id = $1 AND t.conversation_id = $2
         ORDER BY t.received_at DESC, t.turn_id DESC
         LIMIT $3",
    )
    .bind(account.as_str())
    .bind(conversation.as_uuid())
    .bind(limit_to_sql(limit))
    .fetch_all(conn)
    .await
    .map_err(|error| store_error(&error))?;
    let mut turns = rows
        .iter()
        .map(decode_turn)
        .collect::<Result<Vec<StoredTurn>, StoreError>>()?;
    turns.reverse();
    Ok(turns)
}

/// Loads one turn.
pub(crate) async fn load_turn(
    conn: &mut PgConnection,
    account: &AccountId,
    turn_id: &TurnId,
) -> Result<StoredTurn, StoreError> {
    let row = sqlx::query(
        "SELECT t.received_at, t.user_turn, t.assistant_turn, p.phase
         FROM tf_turn t
         JOIN tf_turn_phase p ON p.account_id = t.account_id AND p.turn_id = t.turn_id
         WHERE t.account_id = $1 AND t.turn_id = $2",
    )
    .bind(account.as_str())
    .bind(turn_id.as_uuid())
    .fetch_optional(conn)
    .await
    .map_err(|error| store_error(&error))?
    .ok_or(StoreError::NotFound)?;
    decode_turn(&row)
}

/// Reads the phase marker of a turn.
pub(crate) async fn turn_phase(
    conn: &mut PgConnection,
    account: &AccountId,
    turn_id: &TurnId,
) -> Result<TurnPhaseMarker, StoreError> {
    let row = sqlx::query(
        "SELECT conversation_id, phase, updated_at FROM tf_turn_phase
         WHERE account_id = $1 AND turn_id = $2",
    )
    .bind(account.as_str())
    .bind(turn_id.as_uuid())
    .fetch_optional(conn)
    .await
    .map_err(|error| store_error(&error))?
    .ok_or(StoreError::NotFound)?;
    decode_marker(&row, account, *turn_id)
}

/// Moves the phase marker, refusing to leave a terminal phase.
///
/// The refusal is the `WHERE` clause, so the check and the write are the same
/// statement and a concurrent delivery cannot slip between them. Writing the
/// phase a turn already has is accepted, which is what makes recovery safe to
/// run more than once.
pub(crate) async fn set_turn_phase(
    conn: &mut PgConnection,
    account: &AccountId,
    turn_id: &TurnId,
    phase: TurnPhase,
    at: DateTime<Utc>,
) -> Result<TurnPhaseMarker, StoreError> {
    let statement = format!(
        "UPDATE tf_turn_phase SET phase = $3, updated_at = $4
         WHERE account_id = $1 AND turn_id = $2
           AND (phase = $3 OR phase NOT IN {TERMINAL_PHASES})
         RETURNING conversation_id, phase, updated_at"
    );
    let updated = sqlx::query(&statement)
        .bind(account.as_str())
        .bind(turn_id.as_uuid())
        .bind(label(&phase)?)
        .bind(at)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|error| store_error(&error))?;
    match updated {
        Some(row) => decode_marker(&row, account, *turn_id),
        None => Err(refusal(conn, account, turn_id).await?),
    }
}

/// Whether nothing moved because the turn is unknown or because its phase is
/// final.
async fn refusal(
    conn: &mut PgConnection,
    account: &AccountId,
    turn_id: &TurnId,
) -> Result<StoreError, StoreError> {
    let exists = sqlx::query("SELECT 1 FROM tf_turn_phase WHERE account_id = $1 AND turn_id = $2")
        .bind(account.as_str())
        .bind(turn_id.as_uuid())
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

/// The markers of turns that have not finished, oldest first.
pub(crate) async fn list_unfinished_turns(
    conn: &mut PgConnection,
    scope: RecoveryScope,
    limit: usize,
) -> Result<Vec<TurnPhaseMarker>, StoreError> {
    let scoped = match &scope {
        RecoveryScope::Account(account) => Some(account.as_str()),
        RecoveryScope::AllAccounts => None,
    };
    let statement = format!(
        "SELECT p.account_id, p.conversation_id, p.turn_id, p.phase, p.updated_at
         FROM tf_turn_phase p
         JOIN tf_turn t ON t.account_id = p.account_id AND t.turn_id = p.turn_id
         WHERE p.phase NOT IN {TERMINAL_PHASES}
           AND ($1::text IS NULL OR p.account_id = $1)
         ORDER BY t.received_at, p.turn_id
         LIMIT $2"
    );
    let rows = sqlx::query(&statement)
        .bind(scoped)
        .bind(limit_to_sql(limit))
        .fetch_all(conn)
        .await
        .map_err(|error| store_error(&error))?;
    rows.iter()
        .map(|row| {
            let owner: String = column(row, "account_id")?;
            let turn_id: Uuid = column(row, "turn_id")?;
            decode_marker(row, &AccountId::from(owner), TurnId::from(turn_id))
        })
        .collect()
}

/// Rebuilds a stored turn from its row.
fn decode_turn(row: &sqlx::postgres::PgRow) -> Result<StoredTurn, StoreError> {
    let assistant: Option<serde_json::Value> = column(row, "assistant_turn")?;
    let phase: String = column(row, "phase")?;
    Ok(StoredTurn {
        user: StoredUserTurn::new(
            from_json(column(row, "user_turn")?)?,
            column(row, "received_at")?,
        ),
        assistant: assistant.map(from_json).transpose()?,
        phase: from_label(&phase)?,
    })
}

/// Rebuilds a phase marker from its row.
fn decode_marker(
    row: &sqlx::postgres::PgRow,
    account: &AccountId,
    turn_id: TurnId,
) -> Result<TurnPhaseMarker, StoreError> {
    let phase: String = column(row, "phase")?;
    let conversation: Uuid = column(row, "conversation_id")?;
    Ok(TurnPhaseMarker {
        account_id: account.clone(),
        conversation_id: ConversationId::from(conversation),
        turn_id,
        phase: from_label(&phase)?,
        updated_at: column(row, "updated_at")?,
    })
}

#[async_trait]
impl ConversationReader for PgStores {
    async fn load_conversation(
        &self,
        account: &AccountId,
        id: &ConversationId,
    ) -> Result<ConversationRecord, StoreError> {
        let mut conn = self.connection().await?;
        load_conversation(&mut conn, account, id).await
    }

    async fn load_recent_turns(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<StoredTurn>, StoreError> {
        let mut conn = self.connection().await?;
        load_recent_turns(&mut conn, account, conversation, limit).await
    }

    async fn load_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<StoredTurn, StoreError> {
        let mut conn = self.connection().await?;
        load_turn(&mut conn, account, turn_id).await
    }

    async fn turn_phase(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<TurnPhaseMarker, StoreError> {
        let mut conn = self.connection().await?;
        turn_phase(&mut conn, account, turn_id).await
    }

    async fn list_unfinished_turns(
        &self,
        scope: RecoveryScope,
        limit: usize,
    ) -> Result<Vec<TurnPhaseMarker>, StoreError> {
        let mut conn = self.connection().await?;
        list_unfinished_turns(&mut conn, scope, limit).await
    }
}

#[async_trait]
impl ConversationWriter for PgStores {
    async fn create_conversation(&self, record: ConversationRecord) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        create_conversation(&mut transaction, record).await?;
        commit(transaction).await
    }

    async fn append_user_turn(&self, turn: StoredUserTurn) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        append_user_turn(&mut transaction, turn, now()).await?;
        commit(transaction).await
    }

    async fn append_assistant_turn(
        &self,
        account: &AccountId,
        turn: AssistantTurn,
    ) -> Result<(), StoreError> {
        let mut transaction = self.transaction().await?;
        append_assistant_turn(&mut transaction, account, turn).await?;
        commit(transaction).await
    }

    async fn set_turn_phase(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
        phase: TurnPhase,
    ) -> Result<TurnPhaseMarker, StoreError> {
        let mut transaction = self.transaction().await?;
        let marker = set_turn_phase(&mut transaction, account, turn_id, phase, now()).await?;
        commit(transaction).await?;
        Ok(marker)
    }
}
