//! JSON Schema → Gemini's schema dialect, or a loud refusal (spec §20.3, §20.4).
//!
//! Both surfaces accept a `responseSchema` beside
//! `responseMimeType: "application/json"` and both *enforce* it during decoding,
//! so this is a genuine
//! [`NativeJsonSchema`](turnframe_provider::capabilities::StructuredOutputCapability::NativeJsonSchema)
//! transport. What it is not is JSON Schema: the wire type is a restricted
//! OpenAPI 3.0 `Schema`, with upper-case types, no `$ref`, no `$defs` and no
//! `oneOf`, and Google's REST layer rejects a field its proto does not declare.
//!
//! **When a schema cannot be expressed, [`translate_schema`] fails.** It never
//! drops the keyword it could not carry and sends the rest — a silently weakened
//! schema is worse than none, because the profile goes on declaring the
//! transport while the guarantee has stopped being true. A refusal is a
//! `Fallback` the router can route around, and it names the keyword and the
//! pointer. It is public so an application can pre-flight its schemas at
//! start-up rather than discovering the problem on the first mutating turn.
//!
//! | Source | Becomes | Why |
//! |---|---|---|
//! | `"type": "string"` | `"type": "STRING"` | Gemini spells types in upper case. |
//! | `"type": ["string", "null"]` | `"type": "STRING", "nullable": true` | The dialect has a `nullable` flag instead of a union. |
//! | `{"anyOf": [T, {"type": "null"}]}` | `T`, `"nullable": true` | The same idiom in union form, which is what `Option<T>` produces. |
//! | `"const": "x"` | `"enum": ["x"]` | Exactly equivalent, and `enum` exists. |
//! | `$ref` into `$defs` | the definition, inlined | The dialect has no references, and dropping one loses the constraint. |
//! | a provably disjoint `oneOf` | `anyOf` | Proven equivalent before the rewrite; see below. |
//! | a `oneOf` of literals | one `enum`, variant docs folded into the description | An enum constrains harder, and the sentences survive. |
//! | `"properties"` | same, plus `propertyOrdering` | Ordering the keys makes the decode deterministic; the set of keys is unchanged. |
//! | `"additionalProperties": false` | *dropped* | Gemini's decoder emits only declared properties, so the constraint is already in force. |
//! | `"$schema"`, `"$id"`, `"examples"`, `"readOnly"`, … | *dropped* | Annotations that constrain nothing. |
//! | an unrecognized `"format"` | *dropped* | Gemini would reject the request and would not have enforced the annotation. |
//! | an unresolvable or recursive `$ref`, an unprovable `oneOf`, `allOf`, `not`, `if`, `patternProperties`, `multipleOf`, `uniqueItems`, tuple `items`, `additionalProperties` as a schema, … | **refused** | Every one of them narrows or widens the accepted set in a way the dialect cannot carry. |
//!
//! References are **inlined** rather than dropped, because deleting `$ref` and
//! `$defs` leaves something that still looks like a schema and constrains
//! nothing it referenced — a field whose type was an enum of four operations
//! becomes free text, the model invents a fifth, and the turn dead-ends on a
//! name nothing can compile. Inlining is exact for an acyclic schema and refused
//! when recursive. A `oneOf` is rewritten only where
//! [`turnframe_provider::dialect::prove_disjoint`] proves the branches cannot
//! both match; why that proof is necessary is in
//! [`docs/provider-adapters.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/provider-adapters.md).
//!
//! ```
//! use serde_json::json;
//! use turnframe_provider_gemini::schema::{SchemaError, translate_schema};
//!
//! let translated = translate_schema(&json!({
//!     "type": "object",
//!     "properties": {"amount": {"type": "integer"}},
//!     "required": ["amount"],
//!     "additionalProperties": false
//! }))
//! .expect("expressible");
//! assert_eq!(translated["type"], "OBJECT");
//! assert_eq!(translated["properties"]["amount"]["type"], "INTEGER");
//! assert_eq!(translated["propertyOrdering"], json!(["amount"]));
//!
//! // A tagged union is proven exclusive by its tag, so it translates.
//! let tagged = translate_schema(&json!({
//!     "oneOf": [
//!         {"type": "object", "required": ["kind"],
//!          "properties": {"kind": {"const": "token"}}},
//!         {"type": "object", "required": ["kind"],
//!          "properties": {"kind": {"const": "new_case"}}}
//!     ]
//! }))
//! .expect("disjoint by its tag");
//! assert_eq!(tagged["anyOf"].as_array().map(Vec::len), Some(2));
//!
//! // A union whose branches can both match is a refusal, not a quiet rename.
//! let refused = translate_schema(&json!({
//!     "type": "object",
//!     "properties": {"choice": {"oneOf": [{"type": "object"}, {"type": "object"}]}}
//! }))
//! .expect_err("the branches overlap");
//! assert_eq!(refused.keyword(), "oneOf");
//! assert_eq!(refused.pointer(), "/properties/choice");
//! ```

use serde_json::{Map, Value};
use turnframe_provider::dialect;
use turnframe_provider::error::{ErrorCode, ProviderError};

/// Longest JSON pointer a [`SchemaError`] repeats, in bytes.
pub const MAX_POINTER_LEN: usize = 128;

/// Nesting depth [`SchemaDialect::gemini`] allows before refusing.
///
/// Google documents no single number and enforces a lower one on some models;
/// this is a guard against a cyclic or pathological schema turning into an
/// unbounded body, not a claim about the service's own limit.
pub const DEFAULT_MAX_DEPTH: usize = 24;

/// Keywords carried through unchanged.
const PASSTHROUGH: &[&str] = &[
    "title",
    "description",
    "nullable",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minProperties",
    "maxProperties",
    "pattern",
    "minimum",
    "maximum",
    "default",
    "example",
];

/// Keywords dropped because they annotate rather than constrain.
const IGNORED: &[&str] = &[
    "$schema",
    "$id",
    "$anchor",
    "$comment",
    "examples",
    "readOnly",
    "writeOnly",
    "deprecated",
];

/// The `format` values Gemini documents. Anything else is dropped.
const KNOWN_FORMATS: &[&str] = &["float", "double", "int32", "int64", "enum", "date-time"];

/// JSON Schema types with a Gemini equivalent.
const TYPES: &[(&str, &str)] = &[
    ("string", "STRING"),
    ("number", "NUMBER"),
    ("integer", "INTEGER"),
    ("boolean", "BOOLEAN"),
    ("array", "ARRAY"),
    ("object", "OBJECT"),
];

/// A schema could not be expressed in Gemini's dialect.
///
/// `Display` names the JSON pointer and the keyword — both of them
/// application-authored configuration, never model output or user text — and
/// nothing else. The offending *value* is never repeated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SchemaError {
    /// The keyword has no equivalent in the dialect.
    #[error("{pointer}: Gemini's schema dialect has no equivalent for {keyword}")]
    UnsupportedKeyword {
        /// JSON pointer to the sub-schema that carries it.
        pointer: String,
        /// The keyword's name.
        keyword: String,
    },
    /// The keyword exists but this value of it cannot be carried.
    #[error("{pointer}: Gemini's schema dialect cannot express this value of {keyword}")]
    UnsupportedValue {
        /// JSON pointer to the sub-schema that carries it.
        pointer: String,
        /// The keyword's name.
        keyword: String,
    },
    /// A sub-schema is not a JSON object. The dialect has no boolean schemas.
    #[error("{pointer}: a Gemini schema must be a JSON object")]
    NotAnObject {
        /// JSON pointer to the offending position.
        pointer: String,
    },
    /// The named type is not one of Gemini's six.
    #[error("{pointer}: {reported} is not one of Gemini's types")]
    UnknownType {
        /// JSON pointer to the sub-schema.
        pointer: String,
        /// The type as written, sanitized.
        reported: String,
    },
    /// The schema nests deeper than the dialect allows.
    #[error("{pointer}: the schema nests deeper than {limit} levels")]
    TooDeep {
        /// JSON pointer to the level that overflowed.
        pointer: String,
        /// The configured limit.
        limit: usize,
    },
    /// The translated schema carries no type, so Gemini cannot decode against
    /// it. Only checked at the root: a `responseSchema` must say what it is.
    #[error("{pointer}: the root of a response schema must declare a type")]
    RootWithoutType {
        /// Always the root pointer.
        pointer: String,
    },
}

impl SchemaError {
    /// The JSON pointer into the source schema.
    #[must_use]
    pub fn pointer(&self) -> &str {
        match self {
            Self::UnsupportedKeyword { pointer, .. }
            | Self::UnsupportedValue { pointer, .. }
            | Self::NotAnObject { pointer }
            | Self::UnknownType { pointer, .. }
            | Self::TooDeep { pointer, .. }
            | Self::RootWithoutType { pointer } => pointer,
        }
    }

    /// The keyword at fault, or a short label for the structural failures.
    #[must_use]
    pub fn keyword(&self) -> &str {
        match self {
            Self::UnsupportedKeyword { keyword, .. } | Self::UnsupportedValue { keyword, .. } => {
                keyword
            }
            Self::NotAnObject { .. } => "schema",
            Self::UnknownType { .. } => "type",
            Self::TooDeep { .. } => "depth",
            Self::RootWithoutType { .. } => "root_type",
        }
    }

    /// Stable snake-case label of the failure family.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::UnsupportedKeyword { .. } => "unsupported_keyword",
            Self::UnsupportedValue { .. } => "unsupported_value",
            Self::NotAnObject { .. } => "not_an_object",
            Self::UnknownType { .. } => "unknown_type",
            Self::TooDeep { .. } => "too_deep",
            Self::RootWithoutType { .. } => "root_without_type",
        }
    }
}

impl From<SchemaError> for ProviderError {
    /// A schema this adapter cannot carry is an
    /// [`Unsupported`](turnframe_provider::error::ProviderErrorKind::Unsupported)
    /// feature, whose retry class is `Fallback`.
    ///
    /// That is the honest classification. The declaration
    /// `NativeJsonSchema` is still true — this provider-model pair does enforce
    /// schemas — it simply cannot enforce *this* one, so the router may offer
    /// another candidate that satisfies the same requirements. Calling it an
    /// invalid request would be `Fatal` and would strand a turn a sibling
    /// provider could have served.
    fn from(value: SchemaError) -> Self {
        Self::unsupported(format!("response_schema:{}", value.keyword())).with_code(format!(
            "{}:{}",
            value.as_str(),
            value.pointer()
        ))
    }
}

/// Which flavour of the dialect to emit.
///
/// The two surfaces differ in a single field, so this is deliberately small: a
/// difference that changed *meaning* would be a capability, not a dialect flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SchemaDialect {
    /// Emit `propertyOrdering` derived from the source key order.
    ///
    /// Google recommends it for deterministic decoding. Older Vertex API
    /// versions do not declare the field and reject a body carrying it.
    pub property_ordering: bool,
    /// Refuse a schema nesting deeper than this.
    pub max_depth: usize,
}

impl SchemaDialect {
    /// What the Gemini developer API and current Vertex versions accept.
    #[must_use]
    pub const fn gemini() -> Self {
        Self {
            property_ordering: true,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// The subset every version accepts: no `propertyOrdering`.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            property_ordering: false,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// Sets the nesting limit.
    #[must_use]
    pub const fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }
}

impl Default for SchemaDialect {
    fn default() -> Self {
        Self::gemini()
    }
}

/// Translates `schema` into Gemini's dialect with the default
/// [`SchemaDialect`].
///
/// # Errors
///
/// Returns the [`SchemaError`] naming the first pointer and keyword that cannot
/// be expressed. Nothing is dropped in order to succeed.
pub fn translate_schema(schema: &Value) -> Result<Value, SchemaError> {
    translate_schema_with(schema, SchemaDialect::gemini())
}

/// Translates `schema` into `dialect`.
///
/// # Errors
///
/// As [`translate_schema`].
pub fn translate_schema_with(schema: &Value, dialect: SchemaDialect) -> Result<Value, SchemaError> {
    translate_node(&normalize(schema, dialect.max_depth)?, "", 0, dialect)
}

/// Puts a schema into the shape the dialect can carry, before translating it.
///
/// Three rewrites, each of them proven in
/// [`turnframe_provider::dialect`] and each of them a narrowing at worst:
///
/// 1. **Definitions are inlined.** The dialect has no `$ref`, so a reference
///    left standing has to be either inlined or dropped, and dropping one turns
///    a typed enum into free text while leaving something that still looks like
///    a schema. That failure is silent, which is why this happens first rather
///    than being left to the caller.
/// 2. **Provably disjoint unions become `anyOf`.** `oneOf` does not exist here.
///    Renaming it unconditionally would widen the accepted set, so the branches
///    are proven mutually exclusive and refused when they cannot be.
/// 3. **Unions of literals collapse into one `enum`.** A union whose branches
///    each pin a single value *is* an enum of those values, and Gemini
///    constrains an enum harder than it constrains a union. The per-branch
///    documentation — for a Rust enum, the doc comment on each variant, often
///    the only place the model is told what a variant means — is folded into
///    the description rather than discarded with the branches.
fn normalize(schema: &Value, max_depth: usize) -> Result<Value, SchemaError> {
    // The caller's limit governs the expansion too, so a refusal never names a
    // bound nobody configured. It counts reference substitutions here and
    // schema levels in the translation below — a schema deep enough to trouble
    // Gemini's decoder is caught there, where the decoder's behaviour is what
    // is being modelled.
    let inlined = dialect::inline_definitions(schema, max_depth)?;
    let narrowed = dialect::narrow_unions(&inlined)?;
    Ok(collapse_literal_unions(&narrowed))
}

/// Rewrites every `anyOf` whose branches all pin one literal into an `enum`.
fn collapse_literal_unions(node: &Value) -> Value {
    match node {
        Value::Object(map) => {
            let mut out: Map<String, Value> = map
                .iter()
                .filter(|(key, _)| key.as_str() != "anyOf")
                .map(|(key, value)| (key.clone(), collapse_literal_unions(value)))
                .collect();
            match map.get("anyOf").and_then(Value::as_array) {
                Some(branches) => match dialect::collapse_literal_union(branches) {
                    Some((values, folded)) => {
                        out.insert("enum".to_owned(), Value::Array(values));
                        if !folded.is_empty() {
                            let described = match out.get("description").and_then(Value::as_str) {
                                Some(existing) => format!("{existing}\n{folded}"),
                                None => folded,
                            };
                            out.insert("description".to_owned(), Value::String(described));
                        }
                    }
                    None => {
                        out.insert(
                            "anyOf".to_owned(),
                            Value::Array(branches.iter().map(collapse_literal_unions).collect()),
                        );
                    }
                },
                None => {
                    if let Some(value) = map.get("anyOf") {
                        out.insert("anyOf".to_owned(), collapse_literal_unions(value));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(collapse_literal_unions).collect()),
        other => other.clone(),
    }
}

impl From<dialect::DialectError> for SchemaError {
    /// Maps a normalization refusal onto this module's own vocabulary.
    ///
    /// The families line up exactly, so nothing is invented: a union whose
    /// branches overlap is a value of `oneOf` the dialect cannot carry, an
    /// unresolvable or recursive reference is a value of `$ref` it cannot
    /// carry, and the two structural failures already exist here.
    fn from(value: dialect::DialectError) -> Self {
        use dialect::DialectError as D;
        let pointer = value.pointer().to_owned();
        // The wildcard is not laziness: `DialectError` is `#[non_exhaustive]`,
        // and a refusal family added there later must still arrive here as a
        // refusal rather than as a compile error in a downstream crate.
        let fallback = value.keyword().to_owned();
        match value {
            D::UnprovenUnion { keyword, .. } | D::ConstrainingRefSibling { keyword, .. } => {
                Self::UnsupportedValue { pointer, keyword }
            }
            D::UnresolvableRef { .. } | D::RecursiveRef { .. } => Self::UnsupportedValue {
                pointer,
                keyword: "$ref".to_owned(),
            },
            D::TooDeep { limit, .. } => Self::TooDeep { pointer, limit },
            D::NotAnObject { .. } => Self::NotAnObject { pointer },
            _ => Self::UnsupportedValue {
                pointer,
                keyword: fallback,
            },
        }
    }
}

/// Translates a schema destined for `responseSchema`, which must have a type.
///
/// A `responseSchema` with no type tells the decoder nothing, and Gemini
/// rejects it. Catching that here turns a 400 at the first task
/// into a `Fallback`-class refusal the router can act on.
///
/// # Errors
///
/// As [`translate_schema`], plus [`SchemaError::RootWithoutType`].
pub fn translate_response_schema(
    schema: &Value,
    dialect: SchemaDialect,
) -> Result<Value, SchemaError> {
    let translated = translate_schema_with(schema, dialect)?;
    let typed = translated.get("type").is_some() || translated.get("anyOf").is_some();
    if typed {
        Ok(translated)
    } else {
        Err(SchemaError::RootWithoutType {
            pointer: root_pointer(),
        })
    }
}

/// The pointer rendered for the root of a schema.
fn root_pointer() -> String {
    "/".to_owned()
}

/// The pointer for a child, truncated so an error line stays bounded.
fn child_pointer(parent: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    let mut pointer = format!("{parent}/{escaped}");
    if pointer.len() > MAX_POINTER_LEN {
        pointer.truncate(MAX_POINTER_LEN);
        pointer.push('…');
    }
    pointer
}

/// The pointer rendered for a node, which is `/` at the root.
fn shown(pointer: &str) -> String {
    if pointer.is_empty() {
        root_pointer()
    } else {
        pointer.to_owned()
    }
}

/// Translates one sub-schema.
fn translate_node(
    node: &Value,
    pointer: &str,
    depth: usize,
    dialect: SchemaDialect,
) -> Result<Value, SchemaError> {
    if depth > dialect.max_depth {
        return Err(SchemaError::TooDeep {
            pointer: shown(pointer),
            limit: dialect.max_depth,
        });
    }
    let Some(source) = node.as_object() else {
        return Err(SchemaError::NotAnObject {
            pointer: shown(pointer),
        });
    };

    let mut out = Map::new();
    for (keyword, value) in source {
        translate_keyword(keyword, value, pointer, depth, dialect, &mut out)?;
    }
    // An `enum` with no type is a string enum; Gemini needs the type spelled.
    if out.contains_key("enum") && !out.contains_key("type") {
        out.insert("type".to_owned(), Value::String("STRING".to_owned()));
    }
    Ok(Value::Object(out))
}

/// Translates one keyword of one sub-schema into `out`.
fn translate_keyword(
    keyword: &str,
    value: &Value,
    pointer: &str,
    depth: usize,
    dialect: SchemaDialect,
    out: &mut Map<String, Value>,
) -> Result<(), SchemaError> {
    match keyword {
        _ if IGNORED.contains(&keyword) => {}
        _ if PASSTHROUGH.contains(&keyword) => {
            out.insert(keyword.to_owned(), value.clone());
        }
        "format" => {
            // An unrecognized format is dropped: Gemini would reject the body
            // and would not have enforced the annotation either way.
            if value.as_str().is_some_and(|f| KNOWN_FORMATS.contains(&f)) {
                out.insert("format".to_owned(), value.clone());
            }
        }
        "type" => translate_type(value, pointer, out)?,
        "enum" => translate_enum(value, pointer, out)?,
        "const" => translate_const(value, pointer, out)?,
        "required" => translate_required(value, pointer, out)?,
        "items" => {
            if value.is_array() {
                // Tuple validation: each position has its own schema, which the
                // dialect's single `items` cannot carry.
                return Err(unsupported_value(pointer, "items"));
            }
            let child = child_pointer(pointer, "items");
            out.insert(
                "items".to_owned(),
                translate_node(value, &child, depth + 1, dialect)?,
            );
        }
        "properties" => translate_properties(value, pointer, depth, dialect, out)?,
        "anyOf" => translate_any_of(value, pointer, depth, dialect, out)?,
        "additionalProperties" => {
            // `false` is Gemini's own behaviour: its decoder emits declared
            // properties and nothing else, so the constraint already holds and
            // dropping the keyword weakens nothing. Anything else asks for a
            // permissiveness the dialect cannot express.
            if value != &Value::Bool(false) {
                return Err(unsupported_value(pointer, "additionalProperties"));
            }
        }
        "propertyOrdering" => {
            if dialect.property_ordering {
                out.insert(keyword.to_owned(), value.clone());
            }
        }
        other => {
            return Err(SchemaError::UnsupportedKeyword {
                pointer: shown(pointer),
                keyword: other.to_owned(),
            });
        }
    }
    Ok(())
}

/// An [`SchemaError::UnsupportedValue`] for `keyword` at `pointer`.
fn unsupported_value(pointer: &str, keyword: &str) -> SchemaError {
    SchemaError::UnsupportedValue {
        pointer: shown(pointer),
        keyword: keyword.to_owned(),
    }
}

/// Maps `type`, including the `["thing", "null"]` union JSON Schema uses for an
/// optional value.
fn translate_type(
    value: &Value,
    pointer: &str,
    out: &mut Map<String, Value>,
) -> Result<(), SchemaError> {
    match value {
        Value::String(name) => {
            out.insert(
                "type".to_owned(),
                Value::String(gemini_type(name, pointer)?),
            );
            Ok(())
        }
        Value::Array(names) => {
            let mut concrete: Vec<&str> = Vec::new();
            let mut nullable = false;
            for name in names {
                let Some(name) = name.as_str() else {
                    return Err(unsupported_value(pointer, "type"));
                };
                if name == "null" {
                    nullable = true;
                } else {
                    concrete.push(name);
                }
            }
            // The dialect has one type plus a `nullable` flag, so exactly one
            // concrete member can survive. Two would need a union it lacks.
            match concrete.as_slice() {
                [only] => {
                    out.insert(
                        "type".to_owned(),
                        Value::String(gemini_type(only, pointer)?),
                    );
                    if nullable {
                        out.insert("nullable".to_owned(), Value::Bool(true));
                    }
                    Ok(())
                }
                _ => Err(unsupported_value(pointer, "type")),
            }
        }
        _ => Err(unsupported_value(pointer, "type")),
    }
}

/// The dialect's upper-case name for a JSON Schema type.
fn gemini_type(name: &str, pointer: &str) -> Result<String, SchemaError> {
    TYPES
        .iter()
        .find(|(json, _)| *json == name)
        .map(|(_, gemini)| (*gemini).to_owned())
        .ok_or_else(|| SchemaError::UnknownType {
            pointer: shown(pointer),
            reported: ErrorCode::new(name).as_str().to_owned(),
        })
}

/// `enum` is a list of strings in the dialect, whatever JSON Schema allows.
fn translate_enum(
    value: &Value,
    pointer: &str,
    out: &mut Map<String, Value>,
) -> Result<(), SchemaError> {
    let Some(values) = value.as_array() else {
        return Err(unsupported_value(pointer, "enum"));
    };
    if values.is_empty() || !values.iter().all(Value::is_string) {
        return Err(unsupported_value(pointer, "enum"));
    }
    out.insert("enum".to_owned(), value.clone());
    Ok(())
}

/// `const: "x"` is exactly `enum: ["x"]`, which the dialect has.
fn translate_const(
    value: &Value,
    pointer: &str,
    out: &mut Map<String, Value>,
) -> Result<(), SchemaError> {
    if !value.is_string() {
        // A non-string constant would have to travel as an enum of one, and the
        // dialect's enum holds strings only.
        return Err(unsupported_value(pointer, "const"));
    }
    out.insert("enum".to_owned(), Value::Array(vec![value.clone()]));
    Ok(())
}

/// `required` is a list of property names.
fn translate_required(
    value: &Value,
    pointer: &str,
    out: &mut Map<String, Value>,
) -> Result<(), SchemaError> {
    let Some(names) = value.as_array() else {
        return Err(unsupported_value(pointer, "required"));
    };
    if !names.iter().all(Value::is_string) {
        return Err(unsupported_value(pointer, "required"));
    }
    out.insert("required".to_owned(), value.clone());
    Ok(())
}

/// Translates every property and records the source key order.
fn translate_properties(
    value: &Value,
    pointer: &str,
    depth: usize,
    dialect: SchemaDialect,
    out: &mut Map<String, Value>,
) -> Result<(), SchemaError> {
    let properties_pointer = child_pointer(pointer, "properties");
    let Some(properties) = value.as_object() else {
        return Err(SchemaError::NotAnObject {
            pointer: properties_pointer,
        });
    };
    let mut translated = Map::new();
    let mut ordering = Vec::with_capacity(properties.len());
    for (name, schema) in properties {
        let child = child_pointer(&properties_pointer, name);
        translated.insert(
            name.clone(),
            translate_node(schema, &child, depth + 1, dialect)?,
        );
        ordering.push(Value::String(name.clone()));
    }
    out.insert("properties".to_owned(), Value::Object(translated));
    // Deterministic key order is free here and Google asks for it. It adds no
    // constraint on the *set* of keys, so it cannot weaken the schema.
    if dialect.property_ordering && !ordering.is_empty() && !out.contains_key("propertyOrdering") {
        out.insert("propertyOrdering".to_owned(), Value::Array(ordering));
    }
    Ok(())
}

/// Translates every branch of an `anyOf`, absorbing a `null` branch into
/// `nullable`.
///
/// The dialect has no `null` type and no way to spell "this or nothing" as a
/// union; it has a `nullable` flag instead. `schemars` writes an optional field
/// as exactly that union — `{"anyOf": [T, {"type": "null"}]}` is what
/// `Option<T>` produces — so without this an optional field is untranslatable
/// and the whole schema is refused for a shape the dialect can express
/// perfectly well.
///
/// Absorbing it is exact: *T or null* is what both forms mean. When one branch
/// is left after the null is taken out, it merges into the parent rather than
/// staying a union of one, since `{"anyOf": [X]}` and `X` accept the same
/// documents and the flatter of the two is the one Gemini's decoder handles.
///
/// A union of nothing but `null` is refused. The dialect cannot say "only
/// null", and pretending the field is an unconstrained string would be the
/// silent weakening this module exists to prevent.
fn translate_any_of(
    value: &Value,
    pointer: &str,
    depth: usize,
    dialect: SchemaDialect,
    out: &mut Map<String, Value>,
) -> Result<(), SchemaError> {
    let Some(branches) = value.as_array() else {
        return Err(unsupported_value(pointer, "anyOf"));
    };
    if branches.is_empty() {
        return Err(unsupported_value(pointer, "anyOf"));
    }

    let is_null = |branch: &Value| branch.get("type") == Some(&Value::String("null".to_owned()));
    let nullable = branches.iter().any(is_null);
    let concrete: Vec<(usize, &Value)> = branches
        .iter()
        .enumerate()
        .filter(|(_, branch)| !is_null(branch))
        .collect();
    if concrete.is_empty() {
        return Err(unsupported_value(pointer, "anyOf"));
    }

    let mut translated = Vec::with_capacity(concrete.len());
    for (index, branch) in concrete {
        let child = child_pointer(&child_pointer(pointer, "anyOf"), &index.to_string());
        translated.push(translate_node(branch, &child, depth + 1, dialect)?);
    }

    match translated.as_slice() {
        [only] => {
            // The sole branch becomes this schema. Keywords already gathered
            // from the parent — the field's own description, say — are more
            // specific than the branch's and are left in place.
            if let Some(body) = only.as_object() {
                for (key, value) in body {
                    out.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }
        _ => {
            out.insert("anyOf".to_owned(), Value::Array(translated));
        }
    }
    if nullable {
        out.insert("nullable".to_owned(), Value::Bool(true));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turnframe_provider::error::{ProviderErrorKind, RetryClass};

    fn translate(schema: Value) -> Result<Value, SchemaError> {
        translate_schema(&schema)
    }

    #[test]
    fn the_conformance_suites_own_schema_survives_translation_intact() {
        let source = turnframe_provider::conformance::payloads::plan_schema();
        let translated = translate(source).expect("the suite's schema is expressible");
        assert_eq!(translated["type"], "OBJECT");
        assert_eq!(translated["required"], json!(["acts"]));
        assert_eq!(translated["properties"]["acts"]["type"], "ARRAY");
        let item = &translated["properties"]["acts"]["items"];
        assert_eq!(item["type"], "OBJECT");
        assert_eq!(item["properties"]["operation"]["type"], "STRING");
        assert_eq!(
            item["properties"][turnframe_provider::conformance::payloads::SCHEMA_MARKER]["type"],
            "BOOLEAN"
        );
        assert_eq!(item["required"], json!(["operation", "target"]));
        // `additionalProperties: false` is Gemini's own behaviour, so it goes
        // away without weakening anything.
        assert!(translated.get("additionalProperties").is_none());
        assert!(item.get("additionalProperties").is_none());
        // And the marker still travels, which is what proves the schema was
        // sent rather than merely named.
        assert!(
            translated
                .to_string()
                .contains(turnframe_provider::conformance::payloads::SCHEMA_MARKER)
        );
    }

    #[test]
    fn types_are_upper_cased_and_optional_unions_become_nullable() {
        for (json_type, gemini) in TYPES {
            let translated = translate(json!({"type": json_type})).expect("a known type");
            assert_eq!(translated["type"], *gemini);
        }
        let nullable = translate(json!({"type": ["string", "null"]})).expect("optional string");
        assert_eq!(nullable["type"], "STRING");
        assert_eq!(nullable["nullable"], true);
        // A single-member union is just that member.
        let single = translate(json!({"type": ["integer"]})).expect("one member");
        assert_eq!(single["type"], "INTEGER");
        assert!(single.get("nullable").is_none());
    }

    #[test]
    fn a_type_the_dialect_does_not_have_is_named_in_the_refusal() {
        let error = translate(json!({"type": "null"})).expect_err("no NULL type");
        assert!(matches!(error, SchemaError::UnknownType { .. }));
        assert_eq!(error.keyword(), "type");
        assert_eq!(error.as_str(), "unknown_type");
        assert!(error.to_string().contains("null"), "{error}");

        // Two concrete members need a union the dialect lacks.
        let union = translate(json!({"type": ["string", "integer"]})).expect_err("real union");
        assert!(matches!(union, SchemaError::UnsupportedValue { .. }));
    }

    #[test]
    fn a_constant_becomes_an_enum_of_one_and_a_non_string_one_is_refused() {
        let translated = translate(json!({"const": "set_travel_date"})).expect("a string constant");
        assert_eq!(translated["enum"], json!(["set_travel_date"]));
        // An enum with no type is a string enum, spelled out for the decoder.
        assert_eq!(translated["type"], "STRING");

        let refused = translate(json!({"const": 7})).expect_err("a numeric constant");
        assert_eq!(refused.keyword(), "const");
    }

    #[test]
    fn every_keyword_the_dialect_cannot_carry_is_refused_by_name() {
        let cases = [
            ("allOf", json!({"allOf": [{"type": "object"}]})),
            ("not", json!({"not": {"type": "string"}})),
            ("if", json!({"if": {"type": "string"}})),
            ("multipleOf", json!({"type": "number", "multipleOf": 2})),
            ("uniqueItems", json!({"type": "array", "uniqueItems": true})),
            (
                "patternProperties",
                json!({"patternProperties": {"^a": {"type": "string"}}}),
            ),
            (
                "exclusiveMinimum",
                json!({"type": "number", "exclusiveMinimum": 0}),
            ),
            ("contains", json!({"type": "array", "contains": {}})),
            (
                "dependentRequired",
                json!({"dependentRequired": {"a": ["b"]}}),
            ),
        ];
        for (keyword, schema) in cases {
            let error = translate(schema).expect_err(keyword);
            assert_eq!(error.keyword(), keyword, "{error}");
            assert_eq!(error.as_str(), "unsupported_keyword", "{error}");
        }
    }

    #[test]
    fn tuple_items_and_a_permissive_additional_properties_are_refused() {
        let tuple = translate(json!({"type": "array", "items": [{"type": "string"}]}))
            .expect_err("tuple validation");
        assert_eq!(tuple.keyword(), "items");

        let open = translate(json!({"type": "object", "additionalProperties": true}))
            .expect_err("an open object");
        assert_eq!(open.keyword(), "additionalProperties");

        let typed =
            translate(json!({"type": "object", "additionalProperties": {"type": "string"}}))
                .expect_err("a typed extension");
        assert_eq!(typed.keyword(), "additionalProperties");
    }

    #[test]
    fn a_boolean_schema_is_refused_because_the_dialect_has_none() {
        let error = translate(json!(true)).expect_err("a boolean schema");
        assert!(matches!(error, SchemaError::NotAnObject { .. }));
        assert_eq!(error.pointer(), "/");

        let nested = translate(json!({"type": "object", "properties": {"a": false}}))
            .expect_err("a nested boolean schema");
        assert_eq!(nested.pointer(), "/properties/a");
    }

    #[test]
    fn the_pointer_leads_to_the_offending_sub_schema() {
        let error = translate(json!({
            "type": "object",
            "properties": {
                "acts": {
                    "type": "array",
                    "items": {"type": "object", "properties": {"when": {"$ref": "#/x"}}}
                }
            }
        }))
        .expect_err("a nested $ref");
        assert_eq!(error.pointer(), "/properties/acts/items/properties/when");
        assert_eq!(error.keyword(), "$ref");
    }

    #[test]
    fn an_optional_field_becomes_nullable_rather_than_untranslatable() {
        // `Option<T>` is written as a union with a null branch. The dialect has
        // no null type, so without absorbing it into `nullable` every schema
        // with an optional field would be refused for a shape Gemini expresses
        // perfectly well.
        let translated = translate(json!({
            "description": "which operation, if any",
            "anyOf": [{"type": "string", "enum": ["a", "b"]}, {"type": "null"}]
        }))
        .expect("the nullable idiom");
        assert_eq!(translated["type"], "STRING");
        assert_eq!(translated["enum"], json!(["a", "b"]));
        assert_eq!(translated["nullable"], true);
        assert!(
            translated.get("anyOf").is_none(),
            "a union of one is flattened"
        );
        assert_eq!(
            translated["description"], "which operation, if any",
            "the field's own sentence is not replaced by the branch's"
        );

        // Several concrete branches keep the union and carry the flag.
        let several = translate(json!({
            "anyOf": [{"type": "string"}, {"type": "object"}, {"type": "null"}]
        }))
        .expect("a nullable union");
        assert_eq!(several["anyOf"].as_array().map(Vec::len), Some(2));
        assert_eq!(several["nullable"], true);

        // Nothing but null is not expressible, and is refused rather than
        // quietly widened into an unconstrained field.
        let refused = translate(json!({"anyOf": [{"type": "null"}]}))
            .expect_err("the dialect cannot say only-null");
        assert_eq!(refused.keyword(), "anyOf");
    }

    #[test]
    fn a_referenced_enum_is_inlined_rather_than_silently_unconstrained() {
        // The failure this guards against is not a rejected request. It is a
        // request that succeeds having quietly lost the constraint: drop the
        // reference and the field becomes free text, so the model invents an
        // operation name and the turn dead-ends.
        let translated = translate(json!({
            "type": "object",
            "required": ["operation"],
            "properties": {"operation": {"$ref": "#/$defs/Op", "description": "which one"}},
            "$defs": {"Op": {"type": "string", "enum": ["set_travel_date", "rebook"]}}
        }))
        .expect("a resolvable reference");
        let field = &translated["properties"]["operation"];
        assert_eq!(field["enum"], json!(["set_travel_date", "rebook"]));
        assert_eq!(field["type"], "STRING");
        assert_eq!(
            field["description"], "which one",
            "the field's own sentence survives the inlining"
        );
    }

    #[test]
    fn a_tagged_union_translates_and_an_overlapping_one_does_not() {
        let translated = translate(json!({
            "oneOf": [
                {"type": "object", "required": ["kind"],
                 "properties": {"kind": {"const": "token"}}},
                {"type": "object", "required": ["kind"],
                 "properties": {"kind": {"const": "new_case"}}}
            ]
        }))
        .expect("disjoint by its tag");
        assert_eq!(translated["anyOf"].as_array().map(Vec::len), Some(2));

        let refused = translate(json!({
            "oneOf": [{"type": "object"}, {"type": "object"}]
        }))
        .expect_err("two open objects overlap");
        assert_eq!(refused.keyword(), "oneOf");
        assert_eq!(refused.as_str(), "unsupported_value");
    }

    #[test]
    fn a_union_of_literals_becomes_one_enum_keeping_each_variant_sentence() {
        // Gemini constrains an enum harder than a union, and the per-variant
        // doc comment is often the only place the model learns what a variant
        // means. Losing it with the branches would be a silent quality loss.
        let translated = translate(json!({
            "description": "what the turn produced",
            "oneOf": [
                {"const": "plan", "description": "a finished plan"},
                {"const": "read_requests", "description": "reads to perform first"}
            ]
        }))
        .expect("a literal union");
        assert_eq!(translated["enum"], json!(["plan", "read_requests"]));
        assert_eq!(translated["type"], "STRING");
        assert!(translated.get("anyOf").is_none());
        let doc = translated["description"].as_str().expect("a description");
        assert!(doc.starts_with("what the turn produced"), "{doc}");
        assert!(doc.contains("«plan»: a finished plan"), "{doc}");
        assert!(
            doc.contains("«read_requests»: reads to perform first"),
            "{doc}"
        );
    }

    #[test]
    fn an_unresolvable_reference_is_refused_rather_than_dropped() {
        for reference in ["#/$defs/Missing", "https://example.test/s#/$defs/X"] {
            let error = translate(json!({"$ref": reference})).expect_err(reference);
            assert_eq!(error.keyword(), "$ref", "{reference}");
            assert_eq!(error.as_str(), "unsupported_value", "{reference}");
        }
    }

    #[test]
    fn a_pathological_nesting_depth_is_refused_rather_than_expanded() {
        let mut schema = json!({"type": "string"});
        for _ in 0..40 {
            schema = json!({"type": "array", "items": schema});
        }
        let error = translate_schema_with(&schema, SchemaDialect::gemini().with_max_depth(8))
            .expect_err("too deep");
        assert!(matches!(error, SchemaError::TooDeep { limit: 8, .. }));
        assert_eq!(error.keyword(), "depth");
    }

    #[test]
    fn annotations_are_dropped_and_known_formats_are_kept() {
        let translated = translate(json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "urn:turnframe:plan",
            "title": "Plan",
            "description": "the proposal",
            "readOnly": true,
            "examples": [{"acts": []}],
            "type": "object"
        }))
        .expect("annotations are harmless");
        assert_eq!(translated["title"], "Plan");
        assert_eq!(translated["description"], "the proposal");
        for dropped in ["$schema", "$id", "readOnly", "examples"] {
            assert!(translated.get(dropped).is_none(), "{dropped} survived");
        }

        let kept = translate(json!({"type": "number", "format": "double"})).expect("known format");
        assert_eq!(kept["format"], "double");
        // An unknown format is dropped: Gemini rejects the body over it and
        // would not have enforced it anyway.
        let dropped = translate(json!({"type": "string", "format": "uuid"})).expect("unknown");
        assert!(dropped.get("format").is_none());
    }

    #[test]
    fn any_of_branches_are_translated_and_ordering_follows_the_source() {
        let translated = translate(json!({
            "type": "object",
            "properties": {
                "zebra": {"type": "string"},
                "alpha": {"anyOf": [{"type": "string"}, {"type": "integer"}]}
            }
        }))
        .expect("anyOf is expressible");
        assert_eq!(translated["propertyOrdering"], json!(["zebra", "alpha"]));
        let branches = &translated["properties"]["alpha"]["anyOf"];
        assert_eq!(branches[0]["type"], "STRING");
        assert_eq!(branches[1]["type"], "INTEGER");
    }

    #[test]
    fn the_conservative_dialect_omits_property_ordering() {
        let schema = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        let modern = translate_schema_with(&schema, SchemaDialect::gemini()).expect("modern");
        assert!(modern.get("propertyOrdering").is_some());
        let old = translate_schema_with(&schema, SchemaDialect::conservative()).expect("old");
        assert!(old.get("propertyOrdering").is_none());
        assert_eq!(SchemaDialect::default(), SchemaDialect::gemini());
    }

    #[test]
    fn a_response_schema_must_say_what_it_is() {
        let dialect = SchemaDialect::gemini();
        let error = translate_response_schema(&json!({"description": "anything"}), dialect)
            .expect_err("no type");
        assert!(matches!(error, SchemaError::RootWithoutType { .. }));
        assert_eq!(error.as_str(), "root_without_type");
        assert!(translate_response_schema(&json!({"type": "object"}), dialect).is_ok());
        assert!(
            translate_response_schema(&json!({"anyOf": [{"type": "object"}]}), dialect).is_ok()
        );
    }

    #[test]
    fn a_refusal_becomes_a_fallback_class_failure_naming_the_keyword() {
        // Two open objects can both match one document, so no proof of
        // exclusivity exists and the union cannot become an `anyOf`.
        let error = translate(json!({"oneOf": [{"type": "object"}, {"type": "object"}]}))
            .expect_err("an unprovable union");
        let provider_error = ProviderError::from(error);
        assert!(matches!(
            provider_error.kind(),
            ProviderErrorKind::Unsupported { .. }
        ));
        assert_eq!(provider_error.retry_class(), RetryClass::Fallback);
        let rendered = provider_error.to_string();
        assert!(rendered.contains("response_schema:oneOf"), "{rendered}");
        assert!(rendered.contains("unsupported_value"), "{rendered}");
    }

    #[test]
    fn a_slash_in_a_property_name_does_not_break_the_pointer() {
        let error = translate(json!({
            "type": "object",
            "properties": {"a/b~c": {"$ref": "#/x"}}
        }))
        .expect_err("a $ref under an awkward name");
        assert_eq!(error.pointer(), "/properties/a~1b~0c");
    }
}
