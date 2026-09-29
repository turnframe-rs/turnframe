//! The error surface of the persistence contract.
//!
//! Every store speaks [`StoreError`] from `turnframe-core` and nothing else, so
//! the runtime classifies persistence failures uniformly (see
//! `turnframe_core::error::ErrorClassification`). The variants and the meaning
//! this crate attaches to them:
//!
//! | Variant | Meaning in this contract |
//! |---|---|
//! | `NotFound` | The addressed record does not exist **for this account**. An id that belongs to another tenant produces exactly the same error (spec §25.4). |
//! | `Conflict` | A uniqueness rule or a compare-and-swap precondition failed: a second blocking interaction on a case, a status transition from the wrong state, a duplicate identifier, an outbox key already enqueued. |
//! | `Unavailable` | The backend could not be reached; nothing was written. Retryable. |
//! | `Timeout` | The backend did not answer in time; the write **may** have landed. Callers must re-read, never blindly retry (spec §16.5). |
//! | `Serialization` | A payload could not be encoded or decoded at the storage boundary. |
//! | `Corrupt` | Stored data violates an invariant the adapter relies on (including a poisoned lock in the in-memory store). |
//! | `Other { code }` | A refusal identified by a stable code from [`codes`]. |
//!
//! `Display` output of every variant is safe to log: identifiers and codes only.

pub use turnframe_core::error::StoreError;

/// Shorthand for the result of a store operation.
pub type StoreResult<T> = Result<T, StoreError>;

/// Stable codes carried by [`StoreError::Other`] when this contract refuses a
/// request before touching storage.
pub mod codes {
    /// A record disagrees with the record it addresses: for example an assistant
    /// turn whose `conversation_id` differs from the persisted user turn's, or a
    /// user turn whose actor account differs from the addressed account.
    pub const IDENTITY_MISMATCH: &str = "turnframe.store.identity_mismatch";
    /// A record violates a structural precondition of the operation: an
    /// interaction inserted in a status other than `Active`, an outbox entry
    /// enqueued in a status other than `Pending`, an empty event batch.
    pub const INVALID_RECORD: &str = "turnframe.store.invalid_record";
    /// A commit bundle carries an item that belongs to another account than the
    /// one the bundle is committed for.
    pub const BUNDLE_ACCOUNT_MISMATCH: &str = "turnframe.store.bundle_account_mismatch";
}

/// Builds the [`codes::IDENTITY_MISMATCH`] refusal.
#[must_use]
pub fn identity_mismatch() -> StoreError {
    StoreError::Other {
        code: codes::IDENTITY_MISMATCH.to_owned(),
    }
}

/// Builds the [`codes::INVALID_RECORD`] refusal.
#[must_use]
pub fn invalid_record() -> StoreError {
    StoreError::Other {
        code: codes::INVALID_RECORD.to_owned(),
    }
}

/// Builds the [`codes::BUNDLE_ACCOUNT_MISMATCH`] refusal.
#[must_use]
pub fn bundle_account_mismatch() -> StoreError {
    StoreError::Other {
        code: codes::BUNDLE_ACCOUNT_MISMATCH.to_owned(),
    }
}

/// Returns `true` when `error` is [`StoreError::Other`] with exactly `code`.
#[must_use]
pub fn has_code(error: &StoreError, code: &str) -> bool {
    matches!(error, StoreError::Other { code: actual } if actual == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_helpers_round_trip() {
        assert!(has_code(&identity_mismatch(), codes::IDENTITY_MISMATCH));
        assert!(has_code(&invalid_record(), codes::INVALID_RECORD));
        assert!(has_code(
            &bundle_account_mismatch(),
            codes::BUNDLE_ACCOUNT_MISMATCH
        ));
        assert!(!has_code(&StoreError::NotFound, codes::INVALID_RECORD));
        assert!(!has_code(&invalid_record(), codes::IDENTITY_MISMATCH));
    }

    #[test]
    fn display_carries_codes_only() {
        assert_eq!(
            invalid_record().to_string(),
            "store failure turnframe.store.invalid_record"
        );
    }
}
