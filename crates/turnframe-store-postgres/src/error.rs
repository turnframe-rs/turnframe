//! Translating PostgreSQL failures into the closed error surface of the
//! persistence contract.
//!
//! A store speaks [`StoreError`] and nothing else, so the runtime can classify a
//! persistence failure without knowing which database is underneath. The
//! translation below is the whole of that promise for this adapter, and the
//! distinction that matters most is between *nothing was written* and *the write
//! may have landed*:
//!
//! | PostgreSQL failure | [`StoreError`] | Why |
//! |---|---|---|
//! | unique or exclusion violation | `Conflict` | a uniqueness rule refused the row; nothing was written |
//! | foreign key violation | `Conflict` | a referenced row was missing or in use; nothing was written |
//! | check violation | `Other(INVALID_RECORD)` | the row itself is illegal, e.g. a negative revision |
//! | serialization failure, deadlock | `Conflict` | the transaction lost a race and rolled back whole |
//! | `statement_timeout` cancellation | `Timeout` | the statement was cut off mid-flight |
//! | connection exception, too many connections | `Unavailable` | the statement never ran |
//! | any other SQLSTATE | `Other("turnframe.store.postgres.<sqlstate>")` | a stable, loggable code |
//! | a value that will not decode | `Corrupt` | stored data disagrees with the schema this adapter relies on |
//!
//! Every write this adapter makes runs inside a transaction, so a connection
//! that dies mid-statement rolls the transaction back and nothing survives —
//! which is why a transport error maps to `Unavailable` rather than `Timeout`.
//! The single exception is the `COMMIT` itself: a failure there is genuinely
//! indeterminate, and [`commit_failed`] maps it to `Timeout` so the caller
//! re-reads instead of retrying (spec §16.5).
//!
//! No message from the database is ever carried into a [`StoreError`]: a
//! constraint name or a server message can quote the row that failed, and rows
//! hold user data. Only SQLSTATE codes cross the boundary.

use sqlx::error::ErrorKind;
use turnframe_store::error::{StoreError, invalid_record};

/// SQLSTATE class 08: the connection could not be established or was lost
/// before the statement ran.
const CLASS_CONNECTION_EXCEPTION: &str = "08";
/// SQLSTATE class 53: the server ran out of a resource, typically connections.
const CLASS_INSUFFICIENT_RESOURCES: &str = "53";
/// `query_canceled`, which is what `statement_timeout` raises.
const QUERY_CANCELED: &str = "57014";
/// `admin_shutdown`, raised when the server terminates the backend.
const ADMIN_SHUTDOWN: &str = "57P01";
/// `serialization_failure`.
const SERIALIZATION_FAILURE: &str = "40001";
/// `deadlock_detected`.
const DEADLOCK_DETECTED: &str = "40P01";

/// Prefix of the stable codes this adapter reports for an unclassified
/// PostgreSQL error, completed by the five-character SQLSTATE.
pub const POSTGRES_CODE_PREFIX: &str = "turnframe.store.postgres.";

/// Maps a `sqlx` failure onto the contract's error surface.
///
/// See the module documentation for the table and its reasoning.
#[must_use]
pub fn store_error(error: &sqlx::Error) -> StoreError {
    match error {
        sqlx::Error::RowNotFound => StoreError::NotFound,
        sqlx::Error::Database(database) => database_error(database.as_ref()),
        // The pool never handed out a connection, so no statement ran.
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => StoreError::Unavailable,
        // Transport and driver failures. Every write is inside a transaction, so
        // a connection lost here takes the transaction down with it.
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::WorkerCrashed => StoreError::Unavailable,
        // A value came back that this adapter cannot read: the schema and the
        // code disagree, which is exactly what `Corrupt` is for.
        sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Decode(_)
        | sqlx::Error::ColumnNotFound(_)
        | sqlx::Error::ColumnIndexOutOfBounds { .. }
        | sqlx::Error::TypeNotFound { .. } => StoreError::Corrupt,
        sqlx::Error::Encode(_) => StoreError::Serialization,
        _ => StoreError::Other {
            code: format!("{POSTGRES_CODE_PREFIX}driver"),
        },
    }
}

/// Maps the failure of a `COMMIT` statement.
///
/// A commit that does not answer is the one place where this adapter cannot say
/// whether the write landed, so it reports [`StoreError::Timeout`]: the caller
/// must re-read rather than retry (spec §16.5). A commit refused by the server
/// with a SQLSTATE — a deferred constraint, a serialization failure — did roll
/// back and keeps its normal classification.
#[must_use]
pub fn commit_failed(error: &sqlx::Error) -> StoreError {
    match error {
        sqlx::Error::Database(_) => store_error(error),
        _ => StoreError::Timeout,
    }
}

/// Classifies an error the server itself returned.
fn database_error(database: &dyn sqlx::error::DatabaseError) -> StoreError {
    match database.kind() {
        ErrorKind::UniqueViolation | ErrorKind::ForeignKeyViolation => return StoreError::Conflict,
        // A row that violates a CHECK or a NOT NULL is an illegal record, not a
        // race: the caller sent something this schema refuses to hold.
        ErrorKind::CheckViolation | ErrorKind::NotNullViolation => return invalid_record(),
        _ => {}
    }
    let Some(sqlstate) = database.code() else {
        return StoreError::Other {
            code: format!("{POSTGRES_CODE_PREFIX}unknown"),
        };
    };
    by_sqlstate(sqlstate.as_ref())
}

/// Classifies a SQLSTATE the `sqlx` error kinds do not cover.
fn by_sqlstate(sqlstate: &str) -> StoreError {
    match sqlstate {
        SERIALIZATION_FAILURE | DEADLOCK_DETECTED => StoreError::Conflict,
        QUERY_CANCELED => StoreError::Timeout,
        ADMIN_SHUTDOWN => StoreError::Unavailable,
        _ if sqlstate.starts_with(CLASS_CONNECTION_EXCEPTION)
            || sqlstate.starts_with(CLASS_INSUFFICIENT_RESOURCES) =>
        {
            StoreError::Unavailable
        }
        // Stable, loggable, and carries no server message.
        _ => StoreError::Other {
            code: format!("{POSTGRES_CODE_PREFIX}{sqlstate}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_failures_report_that_nothing_was_written() {
        let io = sqlx::Error::Io(std::io::Error::other("boom"));
        assert_eq!(store_error(&io), StoreError::Unavailable);
        assert_eq!(
            store_error(&sqlx::Error::PoolTimedOut),
            StoreError::Unavailable
        );
        assert_eq!(
            store_error(&sqlx::Error::PoolClosed),
            StoreError::Unavailable
        );
    }

    #[test]
    fn a_commit_that_does_not_answer_is_indeterminate() {
        // The write may or may not have landed, so the caller must re-read.
        let io = sqlx::Error::Io(std::io::Error::other("boom"));
        assert_eq!(commit_failed(&io), StoreError::Timeout);
    }

    #[test]
    fn undecodable_values_are_corrupt_not_conflicts() {
        let decode = sqlx::Error::ColumnDecode {
            index: "status".to_owned(),
            source: "not a status".into(),
        };
        assert_eq!(store_error(&decode), StoreError::Corrupt);
        assert_eq!(
            store_error(&sqlx::Error::ColumnNotFound("status".to_owned())),
            StoreError::Corrupt
        );
    }

    #[test]
    fn sqlstates_map_to_the_contract() {
        assert_eq!(by_sqlstate(SERIALIZATION_FAILURE), StoreError::Conflict);
        assert_eq!(by_sqlstate(DEADLOCK_DETECTED), StoreError::Conflict);
        assert_eq!(by_sqlstate(QUERY_CANCELED), StoreError::Timeout);
        assert_eq!(by_sqlstate("08006"), StoreError::Unavailable);
        assert_eq!(by_sqlstate("53300"), StoreError::Unavailable);
        assert_eq!(
            by_sqlstate("22012"),
            StoreError::Other {
                code: "turnframe.store.postgres.22012".to_owned()
            }
        );
    }

    #[test]
    fn no_server_message_reaches_the_error_surface() {
        // Rows hold user data and a server message quotes rows, so only the
        // SQLSTATE may cross.
        let rendered = by_sqlstate("22012").to_string();
        assert_eq!(rendered, "store failure turnframe.store.postgres.22012");
    }
}
