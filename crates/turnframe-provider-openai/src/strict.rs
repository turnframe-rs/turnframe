//! JSON Schema → OpenAI's strict structured-output dialect (spec §20.3, §20.4).
//!
//! Strict `json_schema` output constrains decoding, so a violation is impossible, but its
//! dialect is a proper subset of JSON Schema and a schema outside it is an HTTP 400 on every
//! request. Three of its rules collide with what `schemars` derives, and [`to_strict_schema`]
//! performs exactly those rewrites, each one narrowing only ([`turnframe_provider::dialect`]):
//!
//! | The dialect requires | `schemars` emits | Why |
//! |---|---|---|
//! | no `oneOf` anywhere | `oneOf` per enum | one branch per variant |
//! | no keyword beside a `$ref` | `{"$ref": …, "description": …}` | the field's doc comment |
//! | every object closed, every property required | `required` minus the optional ones | `Option<T>` and `#[serde(default)]` |
//!
//! An unproven union is refused, not renamed, and a constraining sibling of a `$ref` too.
//! Closing objects makes an optional property required, so a field meant to be absent must be
//! nullable; [`StrictSchema::forced_required`] lists what was forced. The function is public
//! so an application can check its schemas at start-up:
//!
//! ```
//! use serde_json::json;
//! use turnframe_provider_openai::strict::to_strict_schema;
//!
//! let strict = to_strict_schema(&json!({
//!     "type": "object",
//!     "properties": {
//!         "act": {"oneOf": [
//!             {"type": "object", "required": ["kind"],
//!              "properties": {"kind": {"const": "set"}, "value": {"type": "string"}}},
//!             {"type": "object", "required": ["kind"],
//!              "properties": {"kind": {"const": "clear"}}}
//!         ]}
//!     },
//!     "required": ["act"]
//! }))?;
//!
//! // The union is proven disjoint by its tag, so it is expressible.
//! assert!(strict.schema["properties"]["act"].get("oneOf").is_none());
//! assert_eq!(strict.schema["additionalProperties"], json!(false));
//! // `value` was optional and is now required, which the result says out loud.
//! assert_eq!(
//!     strict.forced_required,
//!     vec!["/properties/act/anyOf/0/properties/value".to_owned()]
//! );
//! # Ok::<(), turnframe_provider::dialect::DialectError>(())
//! ```

use serde_json::Value;
use turnframe_provider::dialect::{DialectError, close_objects, lift_ref_siblings, narrow_unions};

/// A schema rewritten for strict mode, with what the rewrite had to force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictSchema {
    /// The schema to put on the wire.
    pub schema: Value,
    /// JSON pointers, into the rewritten schema, to properties that were
    /// optional in the source and are required here.
    ///
    /// Empty is the healthy case. A non-empty list is not an error — the
    /// rewrite is still sound — but each entry is a place where the model must
    /// now emit something it used to be free to omit, so each should be
    /// nullable in the source schema.
    pub forced_required: Vec<String>,
}

/// Rewrites `schema` into OpenAI's strict dialect.
///
/// # Errors
///
/// Returns the [`DialectError`] naming the JSON pointer and keyword of the
/// first constraint that cannot be carried. An adapter turns that into an
/// `Unsupported` provider failure, whose retry class is `Fallback`: the router
/// may offer the turn to a different profile and must not re-roll against this
/// one, since the schema will fail identically every time.
pub fn to_strict_schema(schema: &Value) -> Result<StrictSchema, DialectError> {
    // Order matters. Unions are narrowed first so the branches that closing
    // will walk into are already `anyOf`; references are lifted before closing
    // so that closing does not see a `$ref` wearing a `description`.
    let narrowed = narrow_unions(schema)?;
    let lifted = lift_ref_siblings(&narrowed)?;
    let closed = close_objects(&lifted);
    Ok(StrictSchema {
        schema: closed.schema,
        forced_required: closed.forced_required,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tagged_union_becomes_an_any_of() {
        let strict = to_strict_schema(&json!({
            "oneOf": [
                {"type": "object", "required": ["kind"], "properties": {"kind": {"const": "a"}}},
                {"type": "object", "required": ["kind"], "properties": {"kind": {"const": "b"}}}
            ]
        }))
        .expect("proven disjoint by its tag");
        assert!(strict.schema.get("oneOf").is_none());
        assert_eq!(strict.schema["anyOf"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn an_unproven_union_is_refused_rather_than_renamed() {
        // Two open objects overlap, so `anyOf` would accept what `oneOf`
        // rejects. Refusing is the whole point.
        let error = to_strict_schema(&json!({
            "oneOf": [{"type": "object"}, {"type": "object"}]
        }))
        .expect_err("not provably disjoint");
        assert_eq!(error.as_str(), "unproven_union");
        assert_eq!(error.keyword(), "oneOf");
    }

    #[test]
    fn a_field_description_survives_beside_its_reference() {
        let strict = to_strict_schema(&json!({
            "type": "object",
            "required": ["a"],
            "properties": {"a": {"$ref": "#/$defs/X", "description": "which trip"}},
            "$defs": {"X": {"type": "string"}}
        }))
        .expect("an annotation lifts");
        let field = &strict.schema["properties"]["a"];
        assert_eq!(field["description"], json!("which trip"));
        assert_eq!(field["anyOf"], json!([{"$ref": "#/$defs/X"}]));
        assert!(field.get("$ref").is_none(), "no keyword sits beside a $ref");
    }

    #[test]
    fn a_constraint_beside_a_reference_is_refused() {
        let error = to_strict_schema(&json!({
            "type": "object",
            "properties": {"a": {"$ref": "#/$defs/X", "minLength": 2}}
        }))
        .expect_err("a constraint cannot move onto a one-branch union");
        assert_eq!(error.as_str(), "constraining_ref_sibling");
    }

    #[test]
    fn every_object_closes_and_names_what_it_forced() {
        let strict = to_strict_schema(&json!({
            "type": "object",
            "required": ["a"],
            "properties": {"a": {"type": "string"}, "b": {"type": ["string", "null"]}}
        }))
        .expect("expressible");
        assert_eq!(strict.schema["additionalProperties"], json!(false));
        assert_eq!(strict.schema["required"], json!(["a", "b"]));
        assert_eq!(strict.forced_required, vec!["/properties/b".to_owned()]);
    }

    #[test]
    fn a_schema_already_in_the_dialect_is_left_alone() {
        let source = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a"],
            "additionalProperties": false
        });
        let strict = to_strict_schema(&source).expect("expressible");
        assert_eq!(strict.schema, source);
        assert!(strict.forced_required.is_empty());
    }
}
