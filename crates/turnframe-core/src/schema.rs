//! JSON Schema helpers.
//!
//! Model-facing structures derive [`schemars::JsonSchema`]; the runtime records
//! a [`SchemaFingerprint`] of the interpreter schema in every replay record so a
//! turn can be reproduced against the exact schema that was in force.

use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::hash::{Digest, HashError, canonical_digest};

/// Generates the root JSON Schema for `T` with the library's default settings.
#[must_use]
pub fn schema_for<T: JsonSchema>() -> Schema {
    SchemaGenerator::default().into_root_schema_for::<T>()
}

/// BLAKE3 digest of the canonical JSON rendering of a schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaFingerprint(pub Digest);

impl SchemaFingerprint {
    /// Fingerprints the schema of `T`.
    pub fn of<T: JsonSchema>() -> Result<Self, HashError> {
        Self::of_schema(&schema_for::<T>())
    }

    /// Fingerprints an already generated schema.
    pub fn of_schema(schema: &Schema) -> Result<Self, HashError> {
        canonical_digest(schema).map(Self)
    }

    /// Borrows the hexadecimal digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for SchemaFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Validates `instance` against `schema`, reporting paths only.
///
/// # Errors
///
/// [`SchemaCheckError`](crate::error::SchemaCheckError) when the schema does not
/// compile or the instance violates it.
pub fn validate_against(
    schema: &schemars::Schema,
    instance: &serde_json::Value,
) -> Result<(), crate::error::SchemaCheckError> {
    let validator = jsonschema::validator_for(schema.as_value())
        .map_err(|_| crate::error::SchemaCheckError::InvalidSchema)?;
    validator.validate(instance).map_err(|error| {
        crate::error::SchemaCheckError::Violation(crate::error::SchemaValidationError {
            instance_path: error.instance_path().to_string(),
            schema_path: error.schema_path().to_string(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct A {
        x: u32,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct B {
        x: String,
    }

    #[test]
    fn fingerprint_distinguishes_schemas_and_is_stable() {
        let a1 = SchemaFingerprint::of::<A>().unwrap();
        let a2 = SchemaFingerprint::of::<A>().unwrap();
        let b = SchemaFingerprint::of::<B>().unwrap();
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
    }
}
