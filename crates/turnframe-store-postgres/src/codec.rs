//! The value boundary: how Turnframe types become columns and come back.
//!
//! Three rules hold everywhere in this adapter.
//!
//! *Enumerations travel as their serde label.* [`label`] and [`from_label`] go
//! through `serde`, so a column can never drift from the JSON representation the
//! rest of the library uses. The SQL literals that appear in partial indexes and
//! `WHERE` clauses — `'active'`, `'resolving'`, `'delivered'` — are pinned by
//! the tests at the bottom of this module.
//!
//! *Documents travel as `jsonb`.* An interaction, a user turn, an assistant turn
//! and a replay record are stored whole, so a reload deserializes the value that
//! was persisted instead of rebuilding it from columns. `jsonb` does not keep
//! object key order, which is harmless here: `serde_json::Value` compares maps
//! by content, and every hash in `turnframe-core` is taken over *canonical*
//! JSON with sorted keys, so a payload hash still verifies after a round trip.
//!
//! *Numbers are checked, never cast.* A revision is a `u64` in Rust and a
//! `bigint` in PostgreSQL. The conversions here refuse the values that do not
//! fit rather than wrapping: too large going in is an illegal record, and too
//! large coming out means the row disagrees with this adapter.

use chrono::{DateTime, Timelike, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::postgres::PgRow;
use sqlx::{Decode, Row, Type};
use turnframe_core::ids::CaseRevision;
use turnframe_store::error::{StoreError, invalid_record};
use turnframe_store::events::EventCursor;

use crate::error::store_error;

/// Renders a unit-variant enumeration as the text stored in its column.
///
/// # Errors
/// * `Serialization` when the value does not serialize to a JSON string, which
///   would mean it is not a unit-variant enumeration.
pub(crate) fn label<T: Serialize>(value: &T) -> Result<String, StoreError> {
    match serde_json::to_value(value).map_err(|_| StoreError::Serialization)? {
        serde_json::Value::String(text) => Ok(text),
        _ => Err(StoreError::Serialization),
    }
}

/// Reads back what [`label`] wrote.
///
/// # Errors
/// * `Corrupt` when the stored text is not a variant of `T`: the row disagrees
///   with the version of the library reading it.
pub(crate) fn from_label<T: DeserializeOwned>(text: &str) -> Result<T, StoreError> {
    serde_json::from_value(serde_json::Value::String(text.to_owned()))
        .map_err(|_| StoreError::Corrupt)
}

/// Renders a value as the `jsonb` document stored in its column.
///
/// # Errors
/// * `Serialization` when the value cannot be rendered as JSON.
pub(crate) fn to_json<T: Serialize>(value: &T) -> Result<serde_json::Value, StoreError> {
    serde_json::to_value(value).map_err(|_| StoreError::Serialization)
}

/// Reads back what [`to_json`] wrote.
///
/// # Errors
/// * `Corrupt` when the document does not describe a `T`.
pub(crate) fn from_json<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, StoreError> {
    serde_json::from_value(value).map_err(|_| StoreError::Corrupt)
}

/// Reads one column, reporting a decode failure as a store error.
///
/// # Errors
/// * `Corrupt` when the column is missing or holds something else.
pub(crate) fn column<'r, T>(row: &'r PgRow, name: &str) -> Result<T, StoreError>
where
    T: Decode<'r, sqlx::Postgres> + Type<sqlx::Postgres>,
{
    row.try_get(name).map_err(|error| store_error(&error))
}

/// A `CaseRevision` as the `bigint` its column holds.
///
/// # Errors
/// * `Other(INVALID_RECORD)` when the revision is past `i64::MAX`, which no
///   real case reaches and which `bigint` cannot hold.
pub(crate) fn revision_to_sql(revision: CaseRevision) -> Result<i64, StoreError> {
    i64::try_from(revision.value()).map_err(|_| invalid_record())
}

/// A `bigint` column as a `CaseRevision`.
///
/// # Errors
/// * `Corrupt` when the column is negative, which the `CHECK` forbids.
pub(crate) fn revision_from_sql(value: i64) -> Result<CaseRevision, StoreError> {
    u64::try_from(value)
        .map(CaseRevision)
        .map_err(|_| StoreError::Corrupt)
}

/// An attempt count as the `integer` its column holds.
///
/// # Errors
/// * `Other(INVALID_RECORD)` when the count is past `i32::MAX`.
pub(crate) fn attempts_to_sql(attempts: u32) -> Result<i32, StoreError> {
    i32::try_from(attempts).map_err(|_| invalid_record())
}

/// An `integer` column as an attempt count.
///
/// # Errors
/// * `Corrupt` when the column is negative, which the `CHECK` forbids.
pub(crate) fn attempts_from_sql(value: i32) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|_| StoreError::Corrupt)
}

/// A store-assigned event position as the `bigint` its column holds.
///
/// # Errors
/// * `Corrupt` when the identity column went negative, which cannot happen
///   while the sequence is the one this schema creates.
pub(crate) fn sequence_from_sql(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt)
}

/// A row count as the `bigint` `count(*)` returns.
///
/// # Errors
/// * `Corrupt` when the count is negative.
pub(crate) fn count_from_sql(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt)
}

/// A caller-supplied list bound as the `LIMIT` its query takes.
///
/// A bound past `i64::MAX` cannot mean anything but "no bound in practice", so
/// it saturates instead of failing a read.
#[must_use]
pub(crate) fn limit_to_sql(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// An event cursor bound as the `sequence` its query compares against.
///
/// A cursor past `i64::MAX` cannot name a row this schema can hold, so it
/// saturates: the answer is an empty page either way.
#[must_use]
pub(crate) fn cursor_to_sql(cursor: EventCursor) -> i64 {
    i64::try_from(cursor.value()).unwrap_or(i64::MAX)
}

/// The instant this adapter stamps a write with.
///
/// Truncated to microseconds because that is the resolution of `timestamptz`: a
/// value stamped here and read back later must compare equal, and a nanosecond
/// tail would silently disappear in the round trip.
#[must_use]
pub(crate) fn now() -> DateTime<Utc> {
    truncate_to_micros(Utc::now())
}

/// Drops the sub-microsecond part of an instant.
#[must_use]
pub(crate) fn truncate_to_micros(at: DateTime<Utc>) -> DateTime<Utc> {
    let micros = at.timestamp_subsec_micros();
    at.with_nanosecond(micros.saturating_mul(1_000))
        .unwrap_or(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::event::OutboxStatus;
    use turnframe_core::interaction::{InteractionKind, InteractionStatus};
    use turnframe_core::replay::TurnPhase;
    use turnframe_store::journal::CommandJournalStatus;

    #[test]
    fn interaction_status_labels_match_the_partial_index() {
        // `tf_one_open_blocking_interaction_per_case` names these two literals,
        // and several `WHERE` clauses name the rest. If a rename ever changed
        // one of them, the index would stop covering the rows it exists for.
        for (status, expected) in [
            (InteractionStatus::Active, "active"),
            (InteractionStatus::Resolving, "resolving"),
            (InteractionStatus::Resolved, "resolved"),
            (InteractionStatus::Declined, "declined"),
            (InteractionStatus::Dismissed, "dismissed"),
            (InteractionStatus::Invalidated, "invalidated"),
            (InteractionStatus::Expired, "expired"),
            (InteractionStatus::Failed, "failed"),
        ] {
            assert_eq!(label(&status).unwrap(), expected);
            assert_eq!(from_label::<InteractionStatus>(expected).unwrap(), status);
        }
    }

    #[test]
    fn turn_phase_labels_match_the_recovery_index() {
        for (phase, expected) in [
            (TurnPhase::Received, "received"),
            (TurnPhase::Interpreted, "interpreted"),
            (TurnPhase::Reduced, "reduced"),
            (TurnPhase::Executing, "executing"),
            (TurnPhase::Committed, "committed"),
            (TurnPhase::Composed, "composed"),
            (TurnPhase::Delivered, "delivered"),
            (TurnPhase::Failed, "failed"),
        ] {
            assert_eq!(label(&phase).unwrap(), expected);
            assert_eq!(from_label::<TurnPhase>(expected).unwrap(), phase);
        }
    }

    #[test]
    fn outbox_and_journal_labels_match_their_where_clauses() {
        for (status, expected) in [
            (OutboxStatus::Pending, "pending"),
            (OutboxStatus::Dispatching, "dispatching"),
            (OutboxStatus::OutcomeUnknown, "outcome_unknown"),
            (OutboxStatus::Completed, "completed"),
            (OutboxStatus::Failed, "failed"),
        ] {
            assert_eq!(label(&status).unwrap(), expected);
        }
        for (status, expected) in [
            (CommandJournalStatus::Pending, "pending"),
            (
                CommandJournalStatus::AwaitingConfirmation,
                "awaiting_confirmation",
            ),
            (CommandJournalStatus::Executing, "executing"),
            (CommandJournalStatus::Committed, "committed"),
            (CommandJournalStatus::Failed, "failed"),
            (CommandJournalStatus::OutcomeUnknown, "outcome_unknown"),
        ] {
            assert_eq!(label(&status).unwrap(), expected);
        }
        assert_eq!(
            label(&InteractionKind::SingleSelect).unwrap(),
            "single_select"
        );
    }

    #[test]
    fn an_unknown_label_is_corrupt_rather_than_a_silent_default() {
        assert_eq!(
            from_label::<InteractionStatus>("nonsense").unwrap_err(),
            StoreError::Corrupt
        );
    }

    #[test]
    fn numbers_are_refused_rather_than_wrapped() {
        assert_eq!(revision_to_sql(CaseRevision(7)).unwrap(), 7);
        assert!(revision_to_sql(CaseRevision(u64::MAX)).is_err());
        assert_eq!(revision_from_sql(7).unwrap(), CaseRevision(7));
        assert_eq!(revision_from_sql(-1).unwrap_err(), StoreError::Corrupt);
        assert_eq!(attempts_to_sql(3).unwrap(), 3);
        assert_eq!(attempts_from_sql(3).unwrap(), 3);
        assert_eq!(attempts_from_sql(-1).unwrap_err(), StoreError::Corrupt);
        assert_eq!(sequence_from_sql(9).unwrap(), 9);
        assert_eq!(count_from_sql(9).unwrap(), 9);
        assert_eq!(limit_to_sql(10), 10);
        assert_eq!(limit_to_sql(usize::MAX), i64::MAX);
        assert_eq!(cursor_to_sql(EventCursor::after(4)), 4);
        assert_eq!(cursor_to_sql(EventCursor(u64::MAX)), i64::MAX);
    }

    #[test]
    fn instants_are_truncated_to_what_timestamptz_can_hold() {
        let with_nanos = DateTime::<Utc>::UNIX_EPOCH
            .with_nanosecond(123_456_789)
            .unwrap();
        assert_eq!(
            truncate_to_micros(with_nanos).timestamp_subsec_nanos(),
            123_456_000
        );
        assert_eq!(now().timestamp_subsec_nanos() % 1_000, 0);
    }

    #[test]
    fn documents_round_trip_through_json() {
        let value = serde_json::json!({ "b": 1, "a": [1, 2] });
        let stored = to_json(&value).unwrap();
        assert_eq!(from_json::<serde_json::Value>(stored).unwrap(), value);
    }
}
