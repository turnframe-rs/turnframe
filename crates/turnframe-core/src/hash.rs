//! Canonical JSON hashing used for payload hashes, idempotency keys, plan hashes
//! and schema fingerprints.
//!
//! "Canonical" here means: the value is serialized with `serde`, every object
//! has its keys sorted lexicographically (recursively) and the result is
//! rendered with `serde_json`'s compact writer. It is **not** full RFC 8785; in
//! particular floating point formatting follows `serde_json`. Domain types that
//! participate in hashes should avoid floats or accept that rule.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Error raised when a value cannot be rendered as canonical JSON.
///
/// The `Display` output never contains the offending value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HashError {
    /// `serde` failed while serializing the value.
    #[error("value could not be serialized to canonical JSON")]
    Serialization(#[source] serde_json::Error),
}

/// A lowercase hexadecimal BLAKE3 digest (64 characters).
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Digest(pub String);

impl Digest {
    /// Borrows the hexadecimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Computes the digest of raw bytes.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(digest_hex(bytes))
    }

    /// Computes the digest of the canonical JSON rendering of `value`.
    pub fn of_canonical<T: Serialize + ?Sized>(value: &T) -> Result<Self, HashError> {
        canonical_digest(value)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Digest> for String {
    fn from(value: Digest) -> Self {
        value.0
    }
}

/// Renders `value` as canonical JSON (sorted keys, compact).
pub fn canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<String, HashError> {
    let mut json = serde_json::to_value(value).map_err(HashError::Serialization)?;
    json.sort_all_objects();
    serde_json::to_string(&json).map_err(HashError::Serialization)
}

/// Renders `value` as a canonical [`serde_json::Value`] (sorted keys).
pub fn canonical_value<T: Serialize + ?Sized>(value: &T) -> Result<serde_json::Value, HashError> {
    let mut json = serde_json::to_value(value).map_err(HashError::Serialization)?;
    json.sort_all_objects();
    Ok(json)
}

/// Returns the lowercase hexadecimal BLAKE3 digest of `bytes`.
#[must_use]
pub fn digest_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Returns the BLAKE3 digest of the canonical JSON rendering of `value`.
pub fn canonical_digest<T: Serialize + ?Sized>(value: &T) -> Result<Digest, HashError> {
    let json = canonical_json(value)?;
    Ok(Digest(digest_hex(json.as_bytes())))
}

/// Derives a stable, opaque UUID from domain-separated `parts`.
///
/// The material is `domain`, then every part, joined by a NUL byte, hashed with
/// BLAKE3; the first 16 bytes become a version 8 (custom) UUID. Two processes
/// running the same library version derive the same identifier from the same
/// parts, which is what makes a replayed turn reproduce its plan (I20).
///
/// Parts must be identifiers, digests or enumerations — never free user text —
/// and must not contain a NUL byte, or the separation is not injective.
#[must_use]
pub fn derive_uuid(domain: &str, parts: &[&str]) -> Uuid {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update(b"\0");
        hasher.update(part.as_bytes());
    }
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        let value = json!({"b": {"z": 1, "a": [{"y": 1, "x": 2}]}, "a": 1});
        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"a":1,"b":{"a":[{"x":2,"y":1}],"z":1}}"#
        );
    }

    #[test]
    fn derive_uuid_is_stable_domain_separated_and_injective() {
        let a = derive_uuid("turnframe.test.v1", &["x", "y"]);
        assert_eq!(a, derive_uuid("turnframe.test.v1", &["x", "y"]));
        assert_ne!(a, derive_uuid("turnframe.other.v1", &["x", "y"]));
        assert_ne!(a, derive_uuid("turnframe.test.v1", &["xy"]));
        assert_ne!(a, derive_uuid("turnframe.test.v1", &["x", "y", "z"]));
        assert_eq!(a.get_version_num(), 8, "custom, not a time-ordered v7");
    }

    #[test]
    fn digest_is_stable_and_order_independent() {
        let a = canonical_digest(&json!({"x": 1, "y": 2})).unwrap();
        let b = canonical_digest(&json!({"y": 2, "x": 1})).unwrap();
        let c = canonical_digest(&json!({"y": 2, "x": 3})).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.as_str().len(), 64);
    }
}
