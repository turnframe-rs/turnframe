//! Rewriting a JSON Schema for a provider's dialect, without weakening it.
//!
//! Every provider that enforces a schema enforces a different subset of JSON Schema.
//! **A rewrite may narrow the accepted set, never widen it.** Narrowing costs a re-roll on a
//! document that would have been fine; widening would leave the profile declaring
//! [`NativeJsonSchema`](crate::capabilities::StructuredOutputCapability::NativeJsonSchema)
//! after the guarantee stopped holding. So a constraint this module cannot express fails,
//! naming the keyword and the JSON pointer: a `Fallback` the router routes around.
//!
//! It does the rewrites the shipped adapters need, proves each with [`prove_disjoint`] and
//! refuses the rest. Why `oneOf` is not simply renamed to `anyOf` is in
//! [`docs/provider-adapters.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/provider-adapters.md).

use serde_json::{Map, Value};

use crate::error::{ErrorCode, ProviderError};

/// Longest JSON pointer a [`DialectError`] repeats, in bytes.
///
/// Pointers are application-authored schema paths rather than model output, but
/// a generated schema can nest deeply enough to make an unbounded pointer a
/// nuisance in a log line.
pub const MAX_POINTER_LEN: usize = 128;

/// How many nested reference expansions [`inline_definitions`] allows.
///
/// This counts *substitutions*, not JSON levels: a definition that references a
/// definition that references a third is three. Real schemas nest named types a
/// handful deep, so the default is generous, and the cycle detector rather than
/// this limit is what catches a definition naming itself.
///
/// Structural depth is a different question and belongs to whoever consumes the
/// inlined schema — a dialect that refuses schemas Gemini cannot decode is
/// measuring what its decoder does, not what inlining did.
pub const DEFAULT_MAX_EXPANSIONS: usize = 32;

/// The JSON nesting depth [`inline_definitions`] walks before giving up.
///
/// Not a policy, a stack guard: the walk is recursive, and a schema built in
/// code rather than parsed can nest arbitrarily. A schema that arrived as JSON
/// is already bounded well below this by the parser.
pub const MAX_STRUCTURAL_DEPTH: usize = 256;

/// Keywords that annotate rather than constrain.
///
/// Dropping one changes no document's validity, so a rewrite may drop them
/// freely. Everything not on this list is a constraint until proven otherwise.
pub const ANNOTATIONS: &[&str] = &[
    "$comment",
    "default",
    "deprecated",
    "description",
    "examples",
    "readOnly",
    "title",
    "writeOnly",
];

/// A schema could not be rewritten without weakening it.
///
/// `Display` names a JSON pointer and a keyword, both of them from a schema the
/// application wrote. Neither model output nor user text can reach these
/// fields.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DialectError {
    /// A union's branches could not be proven mutually exclusive, so rewriting
    /// it as `anyOf` would accept documents the source schema rejects.
    #[error("{pointer}: the branches of {keyword} were not provably disjoint")]
    UnprovenUnion {
        /// JSON pointer to the sub-schema carrying the union.
        pointer: String,
        /// `oneOf`, always — named so the message reads the same as the others.
        keyword: String,
    },
    /// A `$ref` carries a sibling that constrains, so neither dropping it nor
    /// lifting it is safe.
    #[error("{pointer}: $ref carries the constraining sibling {keyword}")]
    ConstrainingRefSibling {
        /// JSON pointer to the sub-schema.
        pointer: String,
        /// The sibling keyword.
        keyword: String,
    },
    /// A `$ref` names something this module cannot resolve: a remote document,
    /// or a pointer into a part of the schema that is not a definition table.
    #[error("{pointer}: $ref is not a local definition reference")]
    UnresolvableRef {
        /// JSON pointer to the sub-schema carrying the reference.
        pointer: String,
    },
    /// A definition refers to itself, directly or through others. Inlining it
    /// does not terminate.
    #[error("{pointer}: the definition is recursive and cannot be inlined")]
    RecursiveRef {
        /// JSON pointer to the reference that closed the cycle.
        pointer: String,
    },
    /// The schema expands or nests deeper than the rewrite allows. `limit`
    /// says which bound was reached.
    #[error("{pointer}: the schema goes deeper than {limit} levels")]
    TooDeep {
        /// JSON pointer to the level that overflowed.
        pointer: String,
        /// The configured limit.
        limit: usize,
    },
    /// A sub-schema is a boolean rather than an object, in a position where the
    /// rewrite has nothing to attach to.
    #[error("{pointer}: expected an object sub-schema")]
    NotAnObject {
        /// JSON pointer to the offending position.
        pointer: String,
    },
}

impl DialectError {
    /// The JSON pointer into the source schema.
    #[must_use]
    pub fn pointer(&self) -> &str {
        match self {
            Self::UnprovenUnion { pointer, .. }
            | Self::ConstrainingRefSibling { pointer, .. }
            | Self::UnresolvableRef { pointer }
            | Self::RecursiveRef { pointer }
            | Self::TooDeep { pointer, .. }
            | Self::NotAnObject { pointer } => pointer,
        }
    }

    /// The keyword at fault, or a short label for the structural failures.
    #[must_use]
    pub fn keyword(&self) -> &str {
        match self {
            Self::UnprovenUnion { keyword, .. } | Self::ConstrainingRefSibling { keyword, .. } => {
                keyword
            }
            Self::UnresolvableRef { .. } | Self::RecursiveRef { .. } => "$ref",
            Self::TooDeep { .. } => "depth",
            Self::NotAnObject { .. } => "schema",
        }
    }

    /// Stable snake-case label of the failure family, for metrics and codes.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::UnprovenUnion { .. } => "unproven_union",
            Self::ConstrainingRefSibling { .. } => "constraining_ref_sibling",
            Self::UnresolvableRef { .. } => "unresolvable_ref",
            Self::RecursiveRef { .. } => "recursive_ref",
            Self::TooDeep { .. } => "too_deep",
            Self::NotAnObject { .. } => "not_an_object",
        }
    }

    /// The error code an adapter attaches when it turns this into a
    /// [`ProviderError`].
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        ErrorCode::new(self.as_str())
    }
}

impl From<DialectError> for ProviderError {
    /// A schema the dialect cannot carry is an
    /// [`Unsupported`](crate::error::ProviderErrorKind::Unsupported) feature,
    /// whose retry class is `Fallback`.
    ///
    /// The capability declaration stays true: this provider does enforce
    /// schemas, it just cannot enforce *this* one. So the router may offer the
    /// turn to another profile, and must not re-roll against the same one.
    fn from(value: DialectError) -> Self {
        Self::unsupported("schema_dialect").with_code(value.as_str())
    }
}

/// Appends `segment` to `pointer` as a JSON pointer reference token.
///
/// RFC 6901 escapes `~` as `~0` and `/` as `~1`, in that order. A property
/// named `a/b` would otherwise produce a pointer that reads as two levels and
/// sends whoever is fixing the schema to the wrong place.
fn child(pointer: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{pointer}/{escaped}")
}

/// Truncates a pointer for an error message, keeping the leading path.
fn short(pointer: &str) -> String {
    if pointer.is_empty() {
        return String::from("/");
    }
    if pointer.len() <= MAX_POINTER_LEN {
        return pointer.to_owned();
    }
    let mut end = MAX_POINTER_LEN;
    while end > 0 && !pointer.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &pointer[..end])
}

/// Why a union's branches cannot overlap.
///
/// Returned by [`prove_disjoint`] so a caller can record *which* argument
/// justified the rewrite rather than only that one did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Disjointness {
    /// Every branch pins the whole document to literals, and no literal is
    /// shared.
    ByConstant,
    /// No two branches admit a common JSON type.
    ByType,
    /// Every branch requires a shared property and pins it to literals no other
    /// branch accepts.
    ByDiscriminant {
        /// The property that separates the branches.
        property: String,
    },
    /// No single property separates every branch, but every *pair* of branches
    /// disagrees on some pinned property.
    ///
    /// This is the shape a union takes once one of its variants is specialized:
    /// several branches share a tag and are told apart by a second field. It is
    /// as sound as the single-property case and for the same reason — a pair
    /// that disagrees on any pinned property has no document in common — and it
    /// takes a pass over the pairs rather than over the properties.
    ByDiscriminantPairs,
}

/// The JSON types a document may have.
///
/// A branch that says nothing about type admits all six, which is why a
/// type-based proof needs every branch to narrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TypeSet(u8);

impl TypeSet {
    const NULL: u8 = 1;
    const BOOLEAN: u8 = 1 << 1;
    const INTEGER: u8 = 1 << 2;
    const NUMBER: u8 = 1 << 3;
    const STRING: u8 = 1 << 4;
    const ARRAY: u8 = 1 << 5;
    const OBJECT: u8 = 1 << 6;
    const ALL: u8 = 0b0111_1111;

    const fn all() -> Self {
        Self(Self::ALL)
    }

    const fn empty() -> Self {
        Self(0)
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn named(name: &str) -> Self {
        Self(match name {
            "null" => Self::NULL,
            "boolean" => Self::BOOLEAN,
            // An integer is a number, so a branch typed `number` and one typed
            // `integer` overlap. Modelling integer as its own bit *plus* the
            // number bit on the `number` side keeps the intersection honest.
            "integer" => Self::INTEGER,
            "number" => Self::NUMBER | Self::INTEGER,
            "string" => Self::STRING,
            "array" => Self::ARRAY,
            "object" => Self::OBJECT,
            _ => 0,
        })
    }

    fn of_value(value: &Value) -> Self {
        Self(match value {
            Value::Null => Self::NULL,
            Value::Bool(_) => Self::BOOLEAN,
            Value::Number(number) if number.is_f64() => Self::NUMBER,
            Value::Number(_) => Self::INTEGER,
            Value::String(_) => Self::STRING,
            Value::Array(_) => Self::ARRAY,
            Value::Object(_) => Self::OBJECT,
        })
    }

    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// The set of JSON types a sub-schema admits, over-approximated.
///
/// Over-approximation is the safe direction: a set that is too *large* can only
/// make a disjointness proof fail, never make a false one succeed.
fn type_set(schema: &Value) -> TypeSet {
    let Some(object) = schema.as_object() else {
        return TypeSet::all();
    };
    if let Some(constant) = object.get("const") {
        return TypeSet::of_value(constant);
    }
    if let Some(Value::Array(values)) = object.get("enum") {
        return values.iter().fold(TypeSet::empty(), |acc, value| {
            acc.union(TypeSet::of_value(value))
        });
    }
    match object.get("type") {
        Some(Value::String(name)) => TypeSet::named(name),
        Some(Value::Array(names)) => names.iter().fold(TypeSet::empty(), |acc, name| {
            name.as_str()
                .map_or(TypeSet::all(), |name| acc.union(TypeSet::named(name)))
        }),
        _ => TypeSet::all(),
    }
}

/// The finite set of literals a sub-schema pins its instance to, if it pins one.
///
/// `None` means the sub-schema admits infinitely many documents, or admits a
/// finite set this function is not clever enough to see. Either way a proof
/// that depends on it does not go through.
fn literals(schema: &Value) -> Option<Vec<&Value>> {
    let object = schema.as_object()?;
    if let Some(constant) = object.get("const") {
        return Some(vec![constant]);
    }
    match object.get("enum") {
        Some(Value::Array(values)) if !values.is_empty() => Some(values.iter().collect()),
        _ => None,
    }
}

/// True when two literal sets share no member.
fn literals_disjoint(left: &[&Value], right: &[&Value]) -> bool {
    !left.iter().any(|one| right.contains(one))
}

/// The properties a branch requires, as a set of names.
fn required_names(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Proves that no document can satisfy two of `branches` at once.
///
/// Returns the argument that succeeded, or `None` when none of them does.
/// `None` is not a claim that the branches overlap — only that this function
/// could not show they do not, which is the direction that keeps a rewrite
/// sound.
///
/// A union of fewer than two branches is trivially disjoint.
///
/// ```
/// use serde_json::json;
/// use turnframe_provider::dialect::{Disjointness, prove_disjoint};
///
/// // A tagged enum, the shape `#[serde(tag = "kind")]` produces.
/// let tagged = [
///     json!({"type": "object", "required": ["kind"],
///            "properties": {"kind": {"const": "a"}}}),
///     json!({"type": "object", "required": ["kind"],
///            "properties": {"kind": {"const": "b"}}}),
/// ];
/// assert_eq!(
///     prove_disjoint(&tagged),
///     Some(Disjointness::ByDiscriminant { property: "kind".to_owned() })
/// );
///
/// // Two open object branches could both match, and are not rewritten.
/// let open = [json!({"type": "object"}), json!({"type": "object"})];
/// assert_eq!(prove_disjoint(&open), None);
/// ```
#[must_use]
pub fn prove_disjoint(branches: &[Value]) -> Option<Disjointness> {
    if branches.len() < 2 {
        return Some(Disjointness::ByConstant);
    }

    // By constant: every branch is a finite literal set, pairwise disjoint.
    let pinned: Option<Vec<Vec<&Value>>> = branches.iter().map(literals).collect();
    if let Some(sets) = pinned
        && pairwise(&sets, |left, right| literals_disjoint(left, right))
    {
        return Some(Disjointness::ByConstant);
    }

    // By type: no two branches admit a common JSON type.
    let types: Vec<TypeSet> = branches.iter().map(type_set).collect();
    if !types.iter().any(|set| set.is_empty())
        && pairwise(&types, |left, right| !left.intersects(*right))
    {
        return Some(Disjointness::ByType);
    }

    // By discriminant: a shared required property pinned to disjoint literals.
    // Candidates come from the first branch, since the property must be
    // required by every one of them.
    for candidate in required_names(&branches[0]) {
        let pinned: Option<Vec<Vec<&Value>>> = branches
            .iter()
            .map(|branch| pinned_property(branch, candidate))
            .collect();
        if let Some(sets) = pinned
            && pairwise(&sets, |left, right| literals_disjoint(left, right))
        {
            return Some(Disjointness::ByDiscriminant {
                property: candidate.to_owned(),
            });
        }
    }

    // No single property separates all of them. It is enough that every *pair*
    // disagrees somewhere: three branches sharing a tag and differing on a
    // second field are still pairwise exclusive, which is what a union looks
    // like once one of its variants has been specialized.
    let candidates: Vec<&str> = required_names(&branches[0]);
    if !candidates.is_empty()
        && pairwise(branches, |left, right| {
            candidates.iter().any(|candidate| {
                match (
                    pinned_property(left, candidate),
                    pinned_property(right, candidate),
                ) {
                    (Some(one), Some(other)) => literals_disjoint(&one, &other),
                    _ => false,
                }
            })
        })
    {
        return Some(Disjointness::ByDiscriminantPairs);
    }

    None
}

/// The literals a branch pins `property` to, when it requires it and pins it.
fn pinned_property<'a>(branch: &'a Value, property: &str) -> Option<Vec<&'a Value>> {
    if !required_names(branch).contains(&property) {
        return None;
    }
    branch.get("properties")?.get(property).and_then(literals)
}

/// True when `holds` is true for every unordered pair of distinct items.
fn pairwise<T>(items: &[T], holds: impl Fn(&T, &T) -> bool) -> bool {
    items
        .iter()
        .enumerate()
        .all(|(index, left)| items[index + 1..].iter().all(|right| holds(left, right)))
}

/// Rewrites every provably disjoint `oneOf` as `anyOf`, refusing the rest.
///
/// The rewrite is exact where it applies: when no document can match two
/// branches, *exactly one* and *at least one* accept the same set. Where it
/// does not apply the schema is refused, because renaming the keyword there
/// would accept documents the source rejects.
///
/// # Errors
///
/// Returns [`DialectError::UnprovenUnion`] naming the pointer of the first
/// union it could not prove.
///
/// ```
/// use serde_json::json;
/// use turnframe_provider::dialect::narrow_unions;
///
/// let rewritten = narrow_unions(&json!({
///     "oneOf": [{"const": "a"}, {"const": "b"}]
/// }))?;
/// assert_eq!(rewritten["anyOf"], json!([{"const": "a"}, {"const": "b"}]));
/// assert!(rewritten.get("oneOf").is_none());
/// # Ok::<(), turnframe_provider::dialect::DialectError>(())
/// ```
pub fn narrow_unions(schema: &Value) -> Result<Value, DialectError> {
    fn walk(node: &Value, pointer: &str) -> Result<Value, DialectError> {
        match node {
            Value::Object(map) => {
                let mut out = Map::with_capacity(map.len());
                for (key, value) in map {
                    let child = child(pointer, key);
                    if key == "oneOf" {
                        let branches =
                            value.as_array().ok_or_else(|| DialectError::NotAnObject {
                                pointer: short(&child),
                            })?;
                        if prove_disjoint(branches).is_none() {
                            return Err(DialectError::UnprovenUnion {
                                pointer: short(pointer),
                                keyword: String::from("oneOf"),
                            });
                        }
                        out.insert(String::from("anyOf"), walk(value, &child)?);
                    } else {
                        out.insert(key.clone(), walk(value, &child)?);
                    }
                }
                Ok(Value::Object(out))
            }
            Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| walk(item, &child(pointer, &index.to_string())))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            other => Ok(other.clone()),
        }
    }
    walk(schema, "")
}

/// Moves keywords that sit beside a `$ref` onto a single-branch `anyOf`.
///
/// JSON Schema 2020-12 lets a `$ref` carry siblings; OpenAI's strict mode does
/// not. `schemars` emits the shape constantly, because a field's doc comment
/// becomes a `description` next to the reference to the field's type.
///
/// Wrapping the reference in `{"anyOf": [{"$ref": …}], "description": …}` is
/// exact — a union of one branch accepts precisely that branch — and it keeps
/// the description, which is the sentence telling the model what the field
/// means. Dropping it would be sound and would make the model worse.
///
/// Only [`ANNOTATIONS`] may be lifted. A sibling that constrains is refused,
/// because in 2020-12 it applies *alongside* the reference and a single-branch
/// `anyOf` would drop that conjunction.
///
/// # Errors
///
/// Returns [`DialectError::ConstrainingRefSibling`] naming the keyword.
pub fn lift_ref_siblings(schema: &Value) -> Result<Value, DialectError> {
    fn walk(node: &Value, pointer: &str) -> Result<Value, DialectError> {
        match node {
            Value::Object(map) if map.contains_key("$ref") && map.len() > 1 => {
                let mut out = Map::with_capacity(map.len());
                for key in map.keys().filter(|key| key.as_str() != "$ref") {
                    if !ANNOTATIONS.contains(&key.as_str()) {
                        return Err(DialectError::ConstrainingRefSibling {
                            pointer: short(pointer),
                            keyword: key.clone(),
                        });
                    }
                }
                for (key, value) in map {
                    if key == "$ref" {
                        continue;
                    }
                    out.insert(key.clone(), value.clone());
                }
                out.insert(
                    String::from("anyOf"),
                    Value::Array(vec![serde_json::json!({"$ref": map["$ref"].clone()})]),
                );
                Ok(Value::Object(out))
            }
            Value::Object(map) => map
                .iter()
                .map(|(key, value)| {
                    walk(value, &child(pointer, key)).map(|value| (key.clone(), value))
                })
                .collect::<Result<Map<_, _>, _>>()
                .map(Value::Object),
            Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| walk(item, &child(pointer, &index.to_string())))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            other => Ok(other.clone()),
        }
    }
    walk(schema, "")
}

/// What [`close_objects`] had to force, so a caller can say so rather than
/// discover it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Closure {
    /// The rewritten schema.
    pub schema: Value,
    /// JSON pointers to properties that were optional in the source and are
    /// required in the result, most useful when a model starts emitting a
    /// field it used to omit.
    pub forced_required: Vec<String>,
}

/// Closes every object and requires every property it declares.
///
/// OpenAI's strict mode demands both. Both are **narrowings**: a document that
/// satisfies the result satisfies the source, since the result forbids extra
/// properties the source allowed and forbids omitting properties the source let
/// you omit.
///
/// The narrowing has a consequence worth stating, because it is behavioural
/// rather than formal. A property that was optional must now be present, so the
/// model has to emit *something* for it. When the source schema admits `null`
/// for that property — which is what `Option<T>` produces — the model emits
/// `null` and nothing is lost. When it does not, the model must invent a value
/// of the declared type. Mark genuinely optional fields nullable and the
/// question does not arise; [`Closure::forced_required`] lists the ones this
/// call had to force, so a schema that gets the treatment wrong is visible
/// rather than mysterious.
///
/// ```
/// use serde_json::json;
/// use turnframe_provider::dialect::close_objects;
///
/// let closed = close_objects(&json!({
///     "type": "object",
///     "properties": {"a": {"type": "string"}, "b": {"type": ["string", "null"]}},
///     "required": ["a"]
/// }));
/// assert_eq!(closed.schema["required"], json!(["a", "b"]));
/// assert_eq!(closed.schema["additionalProperties"], json!(false));
/// assert_eq!(closed.forced_required, vec!["/properties/b".to_owned()]);
/// ```
#[must_use]
pub fn close_objects(schema: &Value) -> Closure {
    fn walk(node: &Value, pointer: &str, forced: &mut Vec<String>) -> Value {
        match node {
            Value::Object(map) => {
                let mut out = Map::with_capacity(map.len() + 2);
                for (key, value) in map {
                    out.insert(key.clone(), walk(value, &child(pointer, key), forced));
                }
                if let Some(Value::Object(properties)) = map.get("properties") {
                    let already: Vec<&str> = required_names(node);
                    for name in properties.keys() {
                        if !already.contains(&name.as_str()) {
                            forced.push(short(&child(&child(pointer, "properties"), name)));
                        }
                    }
                    out.insert(
                        String::from("required"),
                        Value::Array(
                            properties
                                .keys()
                                .map(|name| Value::String(name.clone()))
                                .collect(),
                        ),
                    );
                    out.insert(String::from("additionalProperties"), Value::Bool(false));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| walk(item, &child(pointer, &index.to_string()), forced))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    let mut forced_required = Vec::new();
    let schema = walk(schema, "", &mut forced_required);
    // The walk finishes a child before its parent, so the raw order is
    // inside-out. Sorting makes the list read like the schema and makes two
    // runs over the same schema report the same thing.
    forced_required.sort();
    Closure {
        schema,
        forced_required,
    }
}

/// Replaces every local `$ref` with the definition it names.
///
/// For a dialect with no `$ref` — Gemini's is one — this is the only way to
/// keep a referenced constraint at all. The alternative that suggests itself,
/// deleting `$defs` and the references into it, is the failure this whole
/// module exists to prevent: a typed enum behind a reference becomes an
/// unconstrained value, the schema still looks like a schema, and the model
/// starts inventing members.
///
/// Inlining is exact for an acyclic schema. Where the shape does not permit it,
/// this refuses:
///
/// * a reference to anything but a local definition, since there is nothing to
///   substitute;
/// * a definition that reaches itself, since substitution does not terminate;
/// * a schema that expands past `max_depth`.
///
/// Annotation siblings on the reference **win** over the definition's own, which
/// is what makes the result readable: `schemars` puts the field's documentation
/// beside the reference and the type's documentation inside it, and the field's
/// is the more specific of the two. A constraining sibling is refused, for the
/// reason [`lift_ref_siblings`] gives.
///
/// # Errors
///
/// Returns the [`DialectError`] naming the pointer that could not be inlined.
pub fn inline_definitions(schema: &Value, max_expansions: usize) -> Result<Value, DialectError> {
    /// Where `schemars` and its predecessors put definitions.
    const TABLES: [&str; 2] = ["$defs", "definitions"];

    struct Inliner<'a> {
        tables: Vec<&'a Map<String, Value>>,
        max_expansions: usize,
    }

    impl Inliner<'_> {
        /// Resolves `#/$defs/Name`, returning the definition body.
        fn resolve(&self, reference: &str) -> Option<&Value> {
            let rest = reference.strip_prefix("#/")?;
            let (table, name) = rest.split_once('/')?;
            if !TABLES.contains(&table) || name.contains('/') {
                return None;
            }
            self.tables.iter().find_map(|found| found.get(name))
        }

        fn walk(
            &self,
            node: &Value,
            pointer: &str,
            depth: usize,
            open: &mut Vec<String>,
        ) -> Result<Value, DialectError> {
            // Two separate bounds, because they answer two questions. `open`
            // counts reference substitutions, which is what "how far did
            // inlining expand" means and what the caller configures. `depth`
            // counts JSON levels and exists only so the recursion cannot run
            // off the stack.
            if open.len() > self.max_expansions {
                return Err(DialectError::TooDeep {
                    pointer: short(pointer),
                    limit: self.max_expansions,
                });
            }
            if depth > MAX_STRUCTURAL_DEPTH {
                return Err(DialectError::TooDeep {
                    pointer: short(pointer),
                    limit: MAX_STRUCTURAL_DEPTH,
                });
            }
            match node {
                Value::Object(map) if map.contains_key("$ref") => {
                    let reference =
                        map["$ref"]
                            .as_str()
                            .ok_or_else(|| DialectError::UnresolvableRef {
                                pointer: short(pointer),
                            })?;
                    if open.iter().any(|seen| seen == reference) {
                        return Err(DialectError::RecursiveRef {
                            pointer: short(pointer),
                        });
                    }
                    let target =
                        self.resolve(reference)
                            .ok_or_else(|| DialectError::UnresolvableRef {
                                pointer: short(pointer),
                            })?;
                    for key in map.keys() {
                        if key == "$ref" || TABLES.contains(&key.as_str()) {
                            // The reference itself, and the definition tables it
                            // resolves against, which a root schema carries
                            // beside its own `$ref`.
                            continue;
                        }
                        if !ANNOTATIONS.contains(&key.as_str()) {
                            return Err(DialectError::ConstrainingRefSibling {
                                pointer: short(pointer),
                                keyword: key.clone(),
                            });
                        }
                    }
                    open.push(reference.to_owned());
                    let expanded = self.walk(target, pointer, depth + 1, open);
                    open.pop();
                    let mut expanded = match expanded? {
                        Value::Object(body) => body,
                        other => return Ok(other),
                    };
                    // The reference's own annotations describe this use of the
                    // type; the definition's describe the type. Prefer the use.
                    for (key, value) in map {
                        if key != "$ref" && !TABLES.contains(&key.as_str()) {
                            expanded.insert(key.clone(), value.clone());
                        }
                    }
                    Ok(Value::Object(expanded))
                }
                Value::Object(map) => map
                    .iter()
                    .filter(|(key, _)| !TABLES.contains(&key.as_str()))
                    .map(|(key, value)| {
                        self.walk(value, &child(pointer, key), depth + 1, open)
                            .map(|value| (key.clone(), value))
                    })
                    .collect::<Result<Map<_, _>, _>>()
                    .map(Value::Object),
                Value::Array(items) => items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        self.walk(item, &child(pointer, &index.to_string()), depth + 1, open)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::Array),
                other => Ok(other.clone()),
            }
        }
    }

    let tables = TABLES
        .iter()
        .filter_map(|name| schema.get(*name).and_then(Value::as_object))
        .collect();
    let inliner = Inliner {
        tables,
        max_expansions,
    };
    inliner.walk(schema, "", 0, &mut Vec::new())
}

/// A union of literal branches, collapsed into one `enum` with its
/// documentation kept.
///
/// A `oneOf` whose every branch pins a single string is *exactly* an `enum` of
/// those strings, so a dialect with no union keyword can still carry the
/// constraint. What it cannot carry is the per-branch `description`, which for
/// a Rust enum is the doc comment on each variant and is often the only place
/// the model is told what a variant means. Folding those lines into the parent
/// description keeps them in front of the model.
///
/// Returns `None` when the branches are not all literal-pinned, leaving the
/// caller to translate or refuse the union some other way.
#[must_use]
pub fn collapse_literal_union(branches: &[Value]) -> Option<(Vec<Value>, String)> {
    let mut values = Vec::with_capacity(branches.len());
    let mut lines = Vec::new();
    for branch in branches {
        let pinned = literals(branch)?;
        if pinned.len() != 1 {
            return None;
        }
        let value = pinned[0];
        if let (Some(name), Some(doc)) = (
            value.as_str(),
            branch.get("description").and_then(Value::as_str),
        ) {
            lines.push(format!("«{name}»: {doc}"));
        }
        values.push(value.clone());
    }
    Some((values, lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tagged_enum_is_disjoint_by_its_tag() {
        let branches = [
            json!({"type": "object", "required": ["kind", "value"],
                   "properties": {"kind": {"const": "token"}, "value": {"type": "string"}}}),
            json!({"type": "object", "required": ["kind"],
                   "properties": {"kind": {"const": "new_case"}}}),
        ];
        assert_eq!(
            prove_disjoint(&branches),
            Some(Disjointness::ByDiscriminant {
                property: "kind".to_owned()
            })
        );
    }

    /// Three branches sharing a tag, told apart by a second pinned field.
    ///
    /// The shape a union takes once one of its variants is specialized: an act
    /// kind split per operation, so `kind` no longer separates everything and
    /// `operation` finishes the job. Without this the adapters would refuse a
    /// schema they can express perfectly well.
    #[test]
    fn branches_sharing_a_tag_are_separated_by_a_second_field() {
        let branches = [
            json!({"type": "object", "required": ["kind", "operation"],
                   "properties": {"kind": {"const": "apply"},
                                  "operation": {"enum": ["a.set", "a.clear"]}}}),
            json!({"type": "object", "required": ["kind", "operation"],
                   "properties": {"kind": {"const": "apply"},
                                  "operation": {"enum": ["a.create"]}}}),
            json!({"type": "object", "required": ["kind"],
                   "properties": {"kind": {"const": "start"}}}),
        ];
        assert_eq!(
            prove_disjoint(&branches),
            Some(Disjointness::ByDiscriminantPairs)
        );
    }

    /// And the generalization does not become permissive: two branches sharing
    /// a tag whose second field also overlaps still cannot be proved.
    #[test]
    fn a_shared_tag_with_an_overlapping_second_field_is_not_disjoint() {
        let branches = [
            json!({"type": "object", "required": ["kind", "operation"],
                   "properties": {"kind": {"const": "apply"},
                                  "operation": {"enum": ["a.set", "a.clear"]}}}),
            json!({"type": "object", "required": ["kind", "operation"],
                   "properties": {"kind": {"const": "apply"},
                                  "operation": {"enum": ["a.clear"]}}}),
        ];
        assert_eq!(prove_disjoint(&branches), None);
    }

    #[test]
    fn a_repeated_tag_is_not_disjoint() {
        // The proof must fail: a document with kind "token" matches both.
        let branches = [
            json!({"type": "object", "required": ["kind"],
                   "properties": {"kind": {"const": "token"}}}),
            json!({"type": "object", "required": ["kind"],
                   "properties": {"kind": {"const": "token"}}}),
        ];
        assert_eq!(prove_disjoint(&branches), None);
    }

    #[test]
    fn a_tag_that_one_branch_leaves_optional_is_not_disjoint() {
        // The second branch accepts a document carrying kind "a", so the two
        // overlap even though their pinned values differ.
        let branches = [
            json!({"type": "object", "required": ["kind"],
                   "properties": {"kind": {"const": "a"}}}),
            json!({"type": "object", "properties": {"kind": {"const": "b"}}}),
        ];
        assert_eq!(prove_disjoint(&branches), None);
    }

    #[test]
    fn distinct_constants_are_disjoint_and_shared_ones_are_not() {
        assert_eq!(
            prove_disjoint(&[json!({"const": "a"}), json!({"const": "b"})]),
            Some(Disjointness::ByConstant)
        );
        assert_eq!(
            prove_disjoint(&[json!({"enum": ["a", "b"]}), json!({"enum": ["b", "c"]})]),
            None
        );
    }

    #[test]
    fn distinct_types_are_disjoint_but_number_contains_integer() {
        assert_eq!(
            prove_disjoint(&[json!({"type": "string"}), json!({"type": "object"})]),
            Some(Disjointness::ByType)
        );
        // 7 satisfies both, so the rewrite must not be allowed.
        assert_eq!(
            prove_disjoint(&[json!({"type": "number"}), json!({"type": "integer"})]),
            None
        );
    }

    #[test]
    fn an_unconstrained_branch_defeats_every_proof() {
        assert_eq!(
            prove_disjoint(&[json!({"type": "string"}), json!({})]),
            None
        );
    }

    #[test]
    fn narrowing_rewrites_what_it_proves_and_refuses_what_it_cannot() {
        let proven = narrow_unions(&json!({
            "properties": {"t": {"oneOf": [{"const": "a"}, {"const": "b"}]}}
        }))
        .expect("provably disjoint");
        assert_eq!(
            proven["properties"]["t"]["anyOf"],
            json!([{"const": "a"}, {"const": "b"}])
        );

        let error = narrow_unions(&json!({
            "properties": {"t": {"oneOf": [{"type": "object"}, {"type": "object"}]}}
        }))
        .expect_err("two open objects overlap");
        assert_eq!(error.keyword(), "oneOf");
        assert_eq!(error.pointer(), "/properties/t");
        assert_eq!(error.as_str(), "unproven_union");
    }

    #[test]
    fn a_ref_keeps_its_description_and_refuses_a_constraint() {
        let lifted = lift_ref_siblings(&json!({
            "properties": {"a": {"$ref": "#/$defs/X", "description": "the a"}}
        }))
        .expect("an annotation lifts");
        assert_eq!(
            lifted["properties"]["a"],
            json!({"description": "the a", "anyOf": [{"$ref": "#/$defs/X"}]})
        );

        let error = lift_ref_siblings(&json!({
            "properties": {"a": {"$ref": "#/$defs/X", "minLength": 3}}
        }))
        .expect_err("a constraint cannot be lifted");
        assert_eq!(error.keyword(), "minLength");
        assert_eq!(error.as_str(), "constraining_ref_sibling");
    }

    #[test]
    fn closing_forces_every_property_and_names_what_it_forced() {
        let closed = close_objects(&json!({
            "type": "object",
            "required": ["a"],
            "properties": {
                "a": {"type": "string"},
                "b": {"type": ["string", "null"]},
                "c": {"type": "object", "properties": {"d": {"type": "string"}}}
            }
        }));
        assert_eq!(closed.schema["required"], json!(["a", "b", "c"]));
        assert_eq!(closed.schema["additionalProperties"], json!(false));
        assert_eq!(
            closed.schema["properties"]["c"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            closed.forced_required,
            vec![
                "/properties/b".to_owned(),
                "/properties/c".to_owned(),
                "/properties/c/properties/d".to_owned()
            ]
        );
    }

    #[test]
    fn inlining_substitutes_a_definition_and_prefers_the_field_documentation() {
        let inlined = inline_definitions(
            &json!({
                "type": "object",
                "properties": {"a": {"$ref": "#/$defs/Name", "description": "this field"}},
                "$defs": {"Name": {"type": "string", "description": "the type", "minLength": 1}}
            }),
            DEFAULT_MAX_EXPANSIONS,
        )
        .expect("acyclic");
        assert_eq!(
            inlined["properties"]["a"],
            json!({"type": "string", "description": "this field", "minLength": 1}),
            "the constraint survives and the field's own sentence wins"
        );
        assert!(inlined.get("$defs").is_none(), "the table is consumed");
    }

    #[test]
    fn inlining_refuses_a_recursive_definition_rather_than_looping() {
        let error = inline_definitions(
            &json!({
                "$ref": "#/$defs/Node",
                "$defs": {"Node": {"type": "object", "properties": {"next": {"$ref": "#/$defs/Node"}}}}
            }),
            DEFAULT_MAX_EXPANSIONS,
        )
        .expect_err("recursion has no finite expansion");
        assert_eq!(error.as_str(), "recursive_ref");
    }

    #[test]
    fn inlining_refuses_a_reference_it_cannot_resolve() {
        for reference in [
            "https://example.test/schema#/$defs/X",
            "#/components/schemas/X",
            "#/$defs/Missing",
        ] {
            let error = inline_definitions(&json!({"$ref": reference}), DEFAULT_MAX_EXPANSIONS)
                .expect_err("unresolvable");
            assert_eq!(error.as_str(), "unresolvable_ref", "{reference}");
        }
    }

    #[test]
    fn a_literal_union_collapses_and_keeps_each_variant_sentence() {
        let (values, doc) = collapse_literal_union(&[
            json!({"const": "plan", "description": "a finished plan"}),
            json!({"const": "read_requests", "description": "reads to perform"}),
        ])
        .expect("all branches pin one literal");
        assert_eq!(values, vec![json!("plan"), json!("read_requests")]);
        assert_eq!(
            doc,
            "«plan»: a finished plan\n«read_requests»: reads to perform"
        );

        assert!(collapse_literal_union(&[json!({"type": "string"})]).is_none());
    }

    #[test]
    fn a_dialect_failure_is_a_fallback_not_a_retry() {
        use crate::error::{ProviderErrorKind, RetryClass};
        let error = ProviderError::from(DialectError::UnprovenUnion {
            pointer: String::from("/x"),
            keyword: String::from("oneOf"),
        });
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::Unsupported { .. }
        ));
        assert_eq!(error.retry_class(), RetryClass::Fallback);
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("unproven_union".to_owned())
        );
    }

    #[test]
    fn an_awkward_property_name_is_escaped_into_the_pointer() {
        // Unescaped, `a/b` would read as two levels and send whoever is fixing
        // the schema to a path that does not exist.
        let error = narrow_unions(&json!({
            "properties": {"a/b~c": {"oneOf": [{"type": "object"}, {"type": "object"}]}}
        }))
        .expect_err("an unprovable union under an awkward name");
        assert_eq!(error.pointer(), "/properties/a~1b~0c");
    }

    #[test]
    fn the_limit_counts_expansions_rather_than_json_levels() {
        // Structural nesting alone is not expansion. A deeply nested schema
        // with no references inlines to itself, and refusing it here would
        // report a limit that has nothing to do with what inlining did.
        let mut deep = json!({"type": "string"});
        for _ in 0..40 {
            deep = json!({"type": "array", "items": deep});
        }
        assert!(
            inline_definitions(&deep, 2).is_ok(),
            "no references to expand"
        );

        // A chain of three definitions is three expansions, and a limit of two
        // stops it.
        let chained = json!({
            "$ref": "#/$defs/A",
            "$defs": {
                "A": {"type": "object", "properties": {"b": {"$ref": "#/$defs/B"}}},
                "B": {"type": "object", "properties": {"c": {"$ref": "#/$defs/C"}}},
                "C": {"type": "string"}
            }
        });
        assert!(inline_definitions(&chained, 8).is_ok());
        let error = inline_definitions(&chained, 2).expect_err("three deep");
        assert!(
            matches!(error, DialectError::TooDeep { limit: 2, .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_long_pointer_is_truncated_on_a_character_boundary() {
        let deep = format!("/{}", "à".repeat(200));
        let error = DialectError::TooDeep {
            pointer: short(&deep),
            limit: 4,
        };
        assert!(error.pointer().len() <= MAX_POINTER_LEN + 4);
        assert!(error.pointer().ends_with('…'));
    }
}
