//! All-or-nothing structured parsing (invariant I18, spec §0 rule 6).
//!
//! One rule governs this module: **a structured model response is accepted
//! whole or rejected whole.** If a response proposes three acts and one of them
//! is malformed, the two well-formed ones are not executed, not queued, not
//! reported — the turn is rejected and the runtime asks again or raises a
//! deterministic error. There is no partial value to return, so there is no
//! function here that can return one.
//!
//! [`parse_structured`] runs three gates in order, and stops at the first that
//! fails:
//!
//! 1. **Extraction.** [`ModelResponse::single_json`] finds the one JSON
//!    document — refusing to choose when there are several candidates.
//! 2. **Schema validation.** A [`CompiledSchema`] validates it with
//!    `jsonschema` 0.53. Compiling a schema is expensive relative to a turn, so
//!    a [`SchemaCache`] keyed by the schema's canonical digest holds compiled
//!    validators for the process's lifetime.
//! 3. **Typed deserialization.** Only then does the document become `T`. A
//!    domain type with `#[serde(deny_unknown_fields)]` catches what the schema
//!    let through.
//!
//! # Why validate *and* deserialize
//!
//! The schema is the contract shown to the model, and it is the one the
//! provider enforces natively when it can. `serde` is the contract of the Rust
//! type. They drift: a schema evolves, a field becomes optional in one and not
//! the other. Running both means a drift is a rejected turn, never a silently
//! defaulted field on a command that mutates a case.
//!
//! # What errors may say
//!
//! [`StructuredOutputError`] names pointers, keywords and field names — never
//! the offending value. Model output can carry user text, so a validation
//! message that quoted the instance would be a data leak into logs (spec
//! §25.2). Field names are sanitized and truncated before they appear.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::response::ModelResponse;

/// Longest field name or pointer a [`StructuredOutputError`] will repeat.
pub const MAX_FIELD_LEN: usize = 64;

/// Why a structured response was rejected.
///
/// Every variant means the same thing operationally: nothing from this response
/// is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StructuredOutputError {
    /// The response carried nothing parseable: empty content, or an answer cut
    /// short by a token cap or a content filter.
    #[error("model produced no complete output")]
    NoOutput,
    /// The candidate document is not JSON.
    #[error("model output is not JSON: {detail}")]
    NotJson {
        /// `serde_json`'s positional complaint (`"expected value at line 1
        /// column 1"`). Positions and expectations only — never the input.
        detail: String,
    },
    /// The document is JSON but breaks the schema.
    #[error("schema violation at {pointer}: {keyword}")]
    SchemaViolation {
        /// JSON pointer into the instance, e.g. `/acts/1/operation`.
        pointer: String,
        /// The schema keyword that failed, e.g. `type` or `enum`.
        keyword: String,
    },
    /// The document carries a field the schema or the target type does not
    /// know. Never dropped silently for a critical stage.
    #[error("unknown field {field} at {pointer}")]
    UnknownField {
        /// JSON pointer to the object holding it.
        pointer: String,
        /// The field's name, sanitized.
        field: String,
    },
    /// A required field is missing. Never filled in with a default.
    #[error("missing field {field} at {pointer}")]
    MissingField {
        /// JSON pointer to the object that should hold it.
        pointer: String,
        /// The field's name, sanitized.
        field: String,
    },
    /// The response offered several documents where the stage expects one.
    #[error("model produced {candidates} candidate documents, expected one")]
    MultipleCandidates {
        /// How many were found.
        candidates: usize,
    },
    /// The model declined to answer.
    #[error("model refused to answer")]
    Refusal,
}

impl StructuredOutputError {
    /// Wraps a `serde_json` parse failure, keeping only its positional message.
    #[must_use]
    pub fn not_json(error: serde_json::Error) -> Self {
        Self::NotJson {
            detail: error.to_string(),
        }
    }

    /// Stable snake-case label for metrics and reports.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NoOutput => "no_output",
            Self::NotJson { .. } => "not_json",
            Self::SchemaViolation { .. } => "schema_violation",
            Self::UnknownField { .. } => "unknown_field",
            Self::MissingField { .. } => "missing_field",
            Self::MultipleCandidates { .. } => "multiple_candidates",
            Self::Refusal => "refusal",
        }
    }
}

impl From<StructuredOutputError> for crate::error::ProviderError {
    /// A rejected structured response is a
    /// [`Refusal`](crate::error::ProviderErrorKind::Refusal) when the model
    /// declined, and [`Malformed`](crate::error::ProviderErrorKind::Malformed)
    /// otherwise — so a re-roll is the classified remedy.
    fn from(value: StructuredOutputError) -> Self {
        match value {
            StructuredOutputError::Refusal => Self::refusal(),
            other => Self::malformed(other.as_str()),
        }
    }
}

/// Keeps only characters that belong in a field name or a JSON pointer.
fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_FIELD_LEN));
    for ch in raw.chars() {
        if out.len() >= MAX_FIELD_LEN {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '[' | ']') {
            out.push(ch);
        } else {
            out.push('?');
        }
    }
    out
}

/// A JSON pointer that names the document root.
const ROOT_POINTER: &str = "/";

fn pointer_or_root(location: &str) -> String {
    if location.is_empty() {
        ROOT_POINTER.to_owned()
    } else {
        sanitize(location)
    }
}

/// A schema could not be compiled.
///
/// This is an application defect — a schema the runtime supplied is not a valid
/// JSON Schema — not a model failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid JSON Schema at {pointer}: {keyword}")]
pub struct SchemaCompileError {
    /// Where in the schema the problem is.
    pub pointer: String,
    /// Which keyword is wrong.
    pub keyword: String,
}

/// A JSON Schema compiled once and reused.
///
/// Cheap to clone: the validator sits behind an [`Arc`].
#[derive(Clone)]
pub struct CompiledSchema {
    schema: Arc<serde_json::Value>,
    validator: Arc<jsonschema::Validator>,
    fingerprint: turnframe_core::hash::Digest,
}

impl CompiledSchema {
    /// Compiles `schema`.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaCompileError`] when the value is not a valid schema.
    ///
    /// ```
    /// use serde_json::json;
    /// use turnframe_provider::structured::CompiledSchema;
    ///
    /// let schema = CompiledSchema::compile(&json!({
    ///     "type": "object",
    ///     "properties": {"n": {"type": "integer"}},
    ///     "required": ["n"],
    ///     "additionalProperties": false
    /// }))?;
    /// assert!(schema.validate(&json!({"n": 1})).is_ok());
    /// assert!(schema.validate(&json!({"n": "one"})).is_err());
    /// # Ok::<(), turnframe_provider::structured::SchemaCompileError>(())
    /// ```
    pub fn compile(schema: &serde_json::Value) -> Result<Self, SchemaCompileError> {
        let validator = jsonschema::validator_for(schema).map_err(|error| SchemaCompileError {
            pointer: pointer_or_root(&error.schema_path().to_string()),
            keyword: sanitize(keyword_of(error.kind())),
        })?;
        let fingerprint =
            turnframe_core::hash::Digest::of_canonical(schema).map_err(|_| SchemaCompileError {
                pointer: ROOT_POINTER.to_owned(),
                keyword: "not_serializable".to_owned(),
            })?;
        Ok(Self {
            schema: Arc::new(schema.clone()),
            validator: Arc::new(validator),
            fingerprint,
        })
    }

    /// The schema this was compiled from.
    #[must_use]
    pub fn schema(&self) -> &serde_json::Value {
        &self.schema
    }

    /// Canonical digest of the schema; the [`SchemaCache`] key, and a stable
    /// label to record in a replay entry.
    #[must_use]
    pub fn fingerprint(&self) -> &turnframe_core::hash::Digest {
        &self.fingerprint
    }

    /// Validates `instance`, reporting the single most actionable failure.
    ///
    /// When a document breaks the schema in several ways at once, the reported
    /// one is chosen by a fixed priority — unknown field, then missing field,
    /// then any other violation — so the same document always yields the same
    /// error, whatever order the validator walks in.
    ///
    /// # Errors
    ///
    /// Returns the classified [`StructuredOutputError`].
    pub fn validate(&self, instance: &serde_json::Value) -> Result<(), StructuredOutputError> {
        let mut unknown = None;
        let mut missing = None;
        let mut other = None;
        for error in self.validator.iter_errors(instance) {
            let pointer = pointer_or_root(&error.instance_path().to_string());
            match error.kind() {
                jsonschema::error::ValidationErrorKind::AdditionalProperties { unexpected }
                | jsonschema::error::ValidationErrorKind::UnevaluatedProperties { unexpected } => {
                    if unknown.is_none() {
                        let field = unexpected
                            .first()
                            .map_or_else(String::new, |name| sanitize(name));
                        unknown = Some(StructuredOutputError::UnknownField { pointer, field });
                    }
                }
                jsonschema::error::ValidationErrorKind::Required { property } => {
                    if missing.is_none() {
                        let field = property.as_str().map_or_else(String::new, sanitize);
                        missing = Some(StructuredOutputError::MissingField { pointer, field });
                    }
                }
                kind => {
                    if other.is_none() {
                        other = Some(StructuredOutputError::SchemaViolation {
                            pointer,
                            keyword: sanitize(keyword_of(kind)),
                        });
                    }
                }
            }
            if unknown.is_some() && missing.is_some() && other.is_some() {
                break;
            }
        }
        match unknown.or(missing).or(other) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for CompiledSchema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledSchema")
            .field("fingerprint", &self.fingerprint.as_str())
            .finish_non_exhaustive()
    }
}

impl PartialEq for CompiledSchema {
    fn eq(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint
    }
}

impl Eq for CompiledSchema {}

/// Maps a `jsonschema` error kind onto the schema keyword that raised it.
fn keyword_of(kind: &jsonschema::error::ValidationErrorKind) -> &'static str {
    use jsonschema::error::ValidationErrorKind as K;
    match kind {
        K::AdditionalItems { .. } => "additionalItems",
        K::AdditionalProperties { .. } => "additionalProperties",
        K::AnyOf { .. } => "anyOf",
        K::BacktrackLimitExceeded { .. } | K::RegexEngineFailure { .. } => "pattern",
        K::Constant { .. } => "const",
        K::Contains => "contains",
        K::ContentEncoding { .. } => "contentEncoding",
        K::ContentMediaType { .. } => "contentMediaType",
        K::Custom { .. } => "custom",
        K::Enum { .. } => "enum",
        K::ExclusiveMaximum { .. } => "exclusiveMaximum",
        K::ExclusiveMinimum { .. } => "exclusiveMinimum",
        K::FalseSchema => "false",
        K::Format { .. } => "format",
        K::MaxItems { .. } => "maxItems",
        K::Maximum { .. } => "maximum",
        K::MaxLength { .. } => "maxLength",
        K::MaxProperties { .. } => "maxProperties",
        K::MinItems { .. } => "minItems",
        K::Minimum { .. } => "minimum",
        K::MinLength { .. } => "minLength",
        K::MinProperties { .. } => "minProperties",
        K::MultipleOf { .. } => "multipleOf",
        K::Not { .. } => "not",
        K::OneOfMultipleValid { .. } | K::OneOfNotValid { .. } => "oneOf",
        K::Pattern { .. } => "pattern",
        K::PropertyNames { .. } => "propertyNames",
        K::Required { .. } => "required",
        K::Type { .. } => "type",
        K::UnevaluatedItems { .. } => "unevaluatedItems",
        K::UnevaluatedProperties { .. } => "unevaluatedProperties",
        K::UniqueItems => "uniqueItems",
        _ => "schema",
    }
}

/// Compiled schemas, keyed by the canonical digest of the schema.
///
/// Interpretation runs one schema per workflow generation, so the cache is
/// small, long-lived and worth its lock. Cloning a [`SchemaCache`] shares the
/// same store.
#[derive(Clone, Default)]
pub struct SchemaCache {
    entries: Arc<Mutex<HashMap<String, CompiledSchema>>>,
}

impl SchemaCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the compiled form of `schema`, compiling it on first sight.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaCompileError`] when the schema is invalid. Invalid
    /// schemas are not cached.
    ///
    /// ```
    /// use serde_json::json;
    /// use turnframe_provider::structured::SchemaCache;
    ///
    /// let cache = SchemaCache::new();
    /// let schema = json!({"type": "object"});
    /// let first = cache.compile(&schema)?;
    /// let second = cache.compile(&schema)?;
    /// assert_eq!(first.fingerprint(), second.fingerprint());
    /// assert_eq!(cache.len(), 1);
    /// # Ok::<(), turnframe_provider::structured::SchemaCompileError>(())
    /// ```
    pub fn compile(
        &self,
        schema: &serde_json::Value,
    ) -> Result<CompiledSchema, SchemaCompileError> {
        let key = turnframe_core::hash::Digest::of_canonical(schema)
            .map_err(|_| SchemaCompileError {
                pointer: ROOT_POINTER.to_owned(),
                keyword: "not_serializable".to_owned(),
            })?
            .into();
        {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(found) = entries.get(&key) {
                return Ok(found.clone());
            }
        }
        let compiled = CompiledSchema::compile(schema)?;
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(entries.entry(key).or_insert(compiled).clone())
    }

    /// How many schemas are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Returns `true` when nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops every compiled schema. Useful when a workflow generation retires.
    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

impl fmt::Debug for SchemaCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchemaCache")
            .field("len", &self.len())
            .finish()
    }
}

/// Turns a model response into `T`, or rejects it whole.
///
/// # Errors
///
/// Returns the first [`StructuredOutputError`] of the extraction, validation
/// and deserialization gates. There is no partial success.
///
/// ```
/// use serde::Deserialize;
/// use serde_json::json;
/// use turnframe_provider::prelude::*;
/// use turnframe_provider::structured::parse_structured;
///
/// #[derive(Debug, Deserialize, PartialEq)]
/// #[serde(deny_unknown_fields)]
/// struct Plan {
///     acts: Vec<String>,
/// }
///
/// let schema = CompiledSchema::compile(&json!({
///     "type": "object",
///     "properties": {"acts": {"type": "array", "items": {"type": "string"}}},
///     "required": ["acts"],
///     "additionalProperties": false
/// }))?;
///
/// let response = ModelResponse::new(RequestId::nil(), "openai", "gpt-4o")
///     .with_text(r#"{"acts": ["set_travel_date"]}"#);
/// let plan: Plan = parse_structured(&response, &schema)?;
/// assert_eq!(plan.acts, vec!["set_travel_date".to_owned()]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn parse_structured<T: DeserializeOwned>(
    response: &ModelResponse,
    schema: &CompiledSchema,
) -> Result<T, StructuredOutputError> {
    let value = response.single_json()?;
    parse_structured_value(&value, schema)
}

/// The validation and deserialization gates, on a document already extracted.
///
/// Useful for adapters and tests that already hold the JSON.
///
/// # Errors
///
/// Returns the classified [`StructuredOutputError`].
pub fn parse_structured_value<T: DeserializeOwned>(
    value: &serde_json::Value,
    schema: &CompiledSchema,
) -> Result<T, StructuredOutputError> {
    schema.validate(value)?;
    serde_json::from_value(value.clone()).map_err(classify_serde_error)
}

/// Classifies a `serde` failure that survived schema validation.
///
/// The two cases worth naming are the ones a schema can miss: a type that
/// declares `deny_unknown_fields` while the schema allows extras, and a field
/// the schema forgot to require.
fn classify_serde_error(error: serde_json::Error) -> StructuredOutputError {
    let message = error.to_string();
    if let Some(field) = quoted_name(&message, "unknown field ") {
        return StructuredOutputError::UnknownField {
            pointer: ROOT_POINTER.to_owned(),
            field,
        };
    }
    if let Some(field) = quoted_name(&message, "missing field ") {
        return StructuredOutputError::MissingField {
            pointer: ROOT_POINTER.to_owned(),
            field,
        };
    }
    StructuredOutputError::SchemaViolation {
        pointer: ROOT_POINTER.to_owned(),
        keyword: "type".to_owned(),
    }
}

/// Extracts the ``name`` from a `serde` message of the form
/// ``<prefix>`name`, expected …``.
fn quoted_name(message: &str, prefix: &str) -> Option<String> {
    let rest = message.strip_prefix(prefix)?;
    let inner = rest.strip_prefix('`')?;
    let end = inner.find('`')?;
    Some(sanitize(&inner[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::RequestId;
    use crate::request::ToolCall;
    use crate::response::{FinishReason, ModelResponse};
    use serde_json::json;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Act {
        operation: String,
        target: String,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Plan {
        acts: Vec<Act>,
    }

    fn act_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string"},
                "target": {"type": "string"}
            },
            "required": ["operation", "target"],
            "additionalProperties": false
        })
    }

    fn plan_schema() -> CompiledSchema {
        CompiledSchema::compile(&json!({
            "type": "object",
            "properties": {"acts": {"type": "array", "items": act_schema()}},
            "required": ["acts"],
            "additionalProperties": false
        }))
        .unwrap()
    }

    fn text_response(text: &str) -> ModelResponse {
        ModelResponse::new(RequestId::nil(), "p", "m").with_text(text)
    }

    #[test]
    fn a_valid_two_act_plan_parses() {
        let response = text_response(
            r#"{"acts": [
                {"operation": "set_travel_date", "target": "tok_1"},
                {"operation": "set_amount", "target": "tok_2"}
            ]}"#,
        );
        let plan: Plan = parse_structured(&response, &plan_schema()).unwrap();
        assert_eq!(plan.acts.len(), 2);
        assert_eq!(plan.acts[1].operation, "set_amount");
    }

    #[test]
    fn one_malformed_act_rejects_the_whole_response() {
        // The first act is perfect; the second lacks `target`. Nothing is used.
        let response = text_response(
            r#"{"acts": [
                {"operation": "set_travel_date", "target": "tok_1"},
                {"operation": "set_amount"}
            ]}"#,
        );
        let error = parse_structured::<Plan>(&response, &plan_schema()).unwrap_err();
        assert!(
            matches!(
                &error,
                StructuredOutputError::MissingField { pointer, field }
                    if pointer == "/acts/1" && field == "target"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn one_act_with_an_unknown_field_rejects_the_whole_response() {
        let response = text_response(
            r#"{"acts": [
                {"operation": "set_travel_date", "target": "tok_1"},
                {"operation": "set_amount", "target": "tok_2", "force": true}
            ]}"#,
        );
        let error = parse_structured::<Plan>(&response, &plan_schema()).unwrap_err();
        assert!(
            matches!(
                &error,
                StructuredOutputError::UnknownField { pointer, field }
                    if pointer == "/acts/1" && field == "force"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn unknown_field_wins_over_missing_field_deterministically() {
        // Both faults at once: the reported one must not depend on walk order.
        let response = text_response(r#"{"acts": [{"operation": "x", "force": true}]}"#);
        let schema = plan_schema();
        let first = parse_structured::<Plan>(&response, &schema).unwrap_err();
        let second = parse_structured::<Plan>(&response, &schema).unwrap_err();
        assert_eq!(first, second);
        assert!(matches!(first, StructuredOutputError::UnknownField { .. }));
    }

    #[test]
    fn not_json_is_reported_without_the_payload() {
        let response = text_response("Certo! Ecco il piano: primo, secondo.");
        let error = parse_structured::<Plan>(&response, &plan_schema()).unwrap_err();
        let StructuredOutputError::NotJson { detail } = &error else {
            panic!("{error:?}");
        };
        assert!(detail.contains("expected"), "{detail}");
        assert!(!detail.contains("Certo"), "the payload leaked: {detail}");
        assert_eq!(error.as_str(), "not_json");
    }

    #[test]
    fn a_schema_violation_names_the_pointer_and_keyword_only() {
        let response = text_response(r#"{"acts": [{"operation": 7, "target": "t"}]}"#);
        let error = parse_structured::<Plan>(&response, &plan_schema()).unwrap_err();
        assert!(
            matches!(
                &error,
                StructuredOutputError::SchemaViolation { pointer, keyword }
                    if pointer == "/acts/0/operation" && keyword == "type"
            ),
            "{error:?}"
        );
        assert!(!error.to_string().contains('7'));
    }

    #[test]
    fn no_output_empty_and_refusal_are_distinct() {
        let empty = ModelResponse::new(RequestId::nil(), "p", "m");
        assert_eq!(
            parse_structured::<Plan>(&empty, &plan_schema()).unwrap_err(),
            StructuredOutputError::NoOutput
        );

        let refused = text_response("no").with_finish(FinishReason::Refusal);
        assert_eq!(
            parse_structured::<Plan>(&refused, &plan_schema()).unwrap_err(),
            StructuredOutputError::Refusal
        );
    }

    #[test]
    fn two_tool_calls_are_multiple_candidates() {
        let response = ModelResponse::new(RequestId::nil(), "p", "m")
            .with_tool_call(ToolCall::new("a", "plan", json!({"acts": []})))
            .with_tool_call(ToolCall::new("b", "plan", json!({"acts": []})))
            .with_finish(FinishReason::ToolCalls);
        assert_eq!(
            parse_structured::<Plan>(&response, &plan_schema()).unwrap_err(),
            StructuredOutputError::MultipleCandidates { candidates: 2 }
        );
    }

    #[test]
    fn serde_catches_what_a_loose_schema_lets_through() {
        // The schema allows extra properties; the Rust type does not.
        let loose = CompiledSchema::compile(&json!({"type": "object"})).unwrap();
        let response = text_response(r#"{"acts": [], "extra": 1}"#);
        let error = parse_structured::<Plan>(&response, &loose).unwrap_err();
        assert!(
            matches!(&error, StructuredOutputError::UnknownField { field, .. } if field == "extra"),
            "{error:?}"
        );

        let missing = text_response(r#"{}"#);
        let error = parse_structured::<Plan>(&missing, &loose).unwrap_err();
        assert!(
            matches!(&error, StructuredOutputError::MissingField { field, .. } if field == "acts"),
            "{error:?}"
        );

        let wrong_type = text_response(r#"{"acts": "no"}"#);
        let error = parse_structured::<Plan>(&wrong_type, &loose).unwrap_err();
        assert!(
            matches!(error, StructuredOutputError::SchemaViolation { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn field_names_from_the_model_are_sanitized() {
        let loose = CompiledSchema::compile(&json!({"type": "object"})).unwrap();
        let response = text_response(r#"{"acts": [], "a field with spaces": 1}"#);
        let error = parse_structured::<Plan>(&response, &loose).unwrap_err();
        let StructuredOutputError::UnknownField { field, .. } = &error else {
            panic!("{error:?}");
        };
        assert!(!field.contains(' '), "{field}");
        assert_eq!(field, "a?field?with?spaces");
    }

    #[test]
    fn the_cache_compiles_once_and_rejects_bad_schemas() {
        let cache = SchemaCache::new();
        assert!(cache.is_empty());
        let schema = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        let first = cache.compile(&schema).unwrap();
        let second = cache.compile(&schema).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first, second);
        assert_eq!(cache.len(), 1);

        // Key order does not create a second entry: the digest is canonical.
        let reordered = json!({"properties": {"a": {"type": "string"}}, "type": "object"});
        let third = cache.compile(&reordered).unwrap();
        assert_eq!(third.fingerprint(), first.fingerprint());
        assert_eq!(cache.len(), 1);

        let invalid = cache.compile(&json!({"type": "not-a-type"}));
        assert!(invalid.is_err());
        assert_eq!(cache.len(), 1, "invalid schemas are not cached");

        cache.clear();
        assert!(cache.is_empty());
        assert!(format!("{cache:?}").contains("SchemaCache"));
    }

    #[test]
    fn structured_errors_map_onto_provider_errors() {
        use crate::error::{ProviderErrorKind, RetryClass};
        let malformed = crate::error::ProviderError::from(StructuredOutputError::NoOutput);
        assert!(matches!(malformed.kind(), ProviderErrorKind::Malformed));
        assert_eq!(malformed.retry_class(), RetryClass::Retry);
        assert_eq!(
            malformed.code().map(|c| c.as_str().to_owned()),
            Some("no_output".to_owned())
        );

        let refusal = crate::error::ProviderError::from(StructuredOutputError::Refusal);
        assert!(matches!(refusal.kind(), ProviderErrorKind::Refusal));
        assert_eq!(refusal.retry_class(), RetryClass::Fatal);
    }
}
