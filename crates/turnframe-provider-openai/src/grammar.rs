//! JSON Schema → GBNF, for the `llama.cpp` grammar transport (spec §20.3, §20.4).
//!
//! `llama.cpp` constrains decoding with a GBNF grammar, a genuine
//! [`GrammarConstrained`](turnframe_provider::capabilities::StructuredOutputCapability::GrammarConstrained)
//! transport once a schema becomes a grammar. A grammar constrains shape, not values, so
//! `pattern`, `minimum` and their relatives, and `oneOf`, `$ref` and `allOf`, would leave it
//! weaker than the schema: [`to_gbnf`] refuses them with a [`GrammarError`] naming the keyword
//! and its pointer (ADR-008). What it expresses:
//!
//! | Keyword | Treated as |
//! |---|---|
//! | `type: object` with `properties` and `required` | a closed object, required members first |
//! | `type: array` with `items` | a homogeneous array |
//! | `type: string` / `integer` / `number` / `boolean` / `null` | the JSON primitive |
//! | `enum` / `const` of scalars | an alternation of literals |
//! | `additionalProperties: false` | the closed object the grammar already is |
//! | `title`, `description`, `$comment`, `default`, `examples`, `$schema` | annotations, ignored |
//!
//! Two narrowings are deliberate: properties are accepted in declared order, required first,
//! and an object must require at least one property. Every document the grammar accepts
//! satisfies the schema; a valid one it refuses costs a retry, never a wrong command.

use std::fmt;

use serde_json::{Map, Value};

/// The primitive rules every generated grammar carries.
///
/// Emitted whole rather than on demand: the set is eight lines, and a grammar
/// whose preamble depends on the schema is harder to diff than one that does
/// not. An unreferenced rule is legal GBNF; an undefined one is not.
const PRIMITIVES: &str = concat!(
    "ws ::= [ \\t\\n]*\n",
    "string ::= \"\\\"\" char* \"\\\"\"\n",
    "char ::= [^\"\\\\] | \"\\\\\" escape\n",
    "escape ::= [\"\\\\/bfnrt] | \"u\" hex hex hex hex\n",
    "hex ::= [0-9a-fA-F]\n",
    "integer ::= \"-\"? (\"0\" | [1-9] [0-9]*)\n",
    "number ::= integer (\".\" [0-9]+)? ([eE] [-+]? [0-9]+)?\n",
    "boolean ::= \"true\" | \"false\"\n",
);

/// Rule names [`PRIMITIVES`] defines, plus the root.
///
/// The generated root may reference these and nothing else; a test walks it to
/// prove it, which is why the list exists only for the test build.
#[cfg(test)]
const DEFINED_RULES: [&str; 9] = [
    "root", "ws", "string", "char", "escape", "hex", "integer", "number", "boolean",
];

/// Schema keywords that describe rather than constrain, and are ignored.
const ANNOTATIONS: [&str; 6] = [
    "title",
    "description",
    "$comment",
    "default",
    "examples",
    "$schema",
];

/// A schema this translation cannot express as a grammar.
///
/// It names *what* and *where*, never a value: model output and instance data
/// never reach it, and a failure that quoted one would be a leak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrammarError {
    /// The keyword, or the shape, that could not be expressed.
    pub(crate) keyword: String,
    /// JSON pointer to the sub-schema; empty for the root.
    pub(crate) pointer: String,
    /// Why it could not be expressed, in words.
    pub(crate) reason: &'static str,
}

impl GrammarError {
    /// A failure at `pointer`.
    fn at(keyword: impl Into<String>, pointer: &str, reason: &'static str) -> Self {
        Self {
            keyword: keyword.into(),
            pointer: pointer.to_owned(),
            reason,
        }
    }

    /// A short machine code: `pattern_at_/acts/items/code`.
    pub(crate) fn code(&self) -> String {
        let site = if self.pointer.is_empty() {
            "root"
        } else {
            self.pointer.as_str()
        };
        format!("{}_at_{site}", self.keyword)
    }
}

impl fmt::Display for GrammarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.reason)
    }
}

/// Compiles `schema` into GBNF text for `llama.cpp`'s `grammar` field.
///
/// # Errors
///
/// Returns a [`GrammarError`] naming the first keyword the grammar cannot
/// carry, rather than emitting a grammar weaker than the schema.
pub(crate) fn to_gbnf(schema: &Value) -> Result<String, GrammarError> {
    let mut root = String::new();
    write_expr(schema, "", &mut root)?;
    Ok(format!("root ::= ws {root} ws\n{PRIMITIVES}"))
}

/// Writes the expression for one sub-schema.
fn write_expr(schema: &Value, pointer: &str, out: &mut String) -> Result<(), GrammarError> {
    let Some(object) = schema.as_object() else {
        return Err(GrammarError::at(
            "schema",
            pointer,
            "a sub-schema must be a JSON object; `true` and `false` schemas are not expressible",
        ));
    };
    if let Some(constant) = object.get("const") {
        reject_unknown(object, &["const", "type"], pointer)?;
        return write_literal(constant, pointer, out);
    }
    if let Some(members) = object.get("enum") {
        reject_unknown(object, &["enum", "type"], pointer)?;
        return write_enum(members, pointer, out);
    }
    let Some(Value::String(kind)) = object.get("type") else {
        return Err(GrammarError::at(
            "type",
            pointer,
            "a grammar needs one named type; an absent or multi-valued `type` is not expressible",
        ));
    };
    match kind.as_str() {
        "object" => {
            reject_unknown(
                object,
                &["type", "properties", "required", "additionalProperties"],
                pointer,
            )?;
            write_object(object, pointer, out)
        }
        "array" => {
            reject_unknown(object, &["type", "items"], pointer)?;
            write_array(object, pointer, out)
        }
        "string" | "integer" | "number" | "boolean" => {
            reject_unknown(object, &["type"], pointer)?;
            out.push_str(kind);
            Ok(())
        }
        "null" => {
            reject_unknown(object, &["type"], pointer)?;
            out.push_str("\"null\"");
            Ok(())
        }
        other => Err(GrammarError::at(
            format!("type_{other}"),
            pointer,
            "this adapter expresses object, array, string, integer, number, boolean and null",
        )),
    }
}

/// Refuses any keyword this translation would otherwise ignore into weakness.
fn reject_unknown(
    object: &Map<String, Value>,
    structural: &[&str],
    pointer: &str,
) -> Result<(), GrammarError> {
    for key in object.keys() {
        if structural.contains(&key.as_str()) || ANNOTATIONS.contains(&key.as_str()) {
            continue;
        }
        return Err(GrammarError::at(
            key.clone(),
            pointer,
            "a grammar constrains shape, not values; this keyword would be dropped, \
             leaving the wire weaker than the declaration",
        ));
    }
    Ok(())
}

/// Writes a closed object: required members in schema order, then optional ones.
fn write_object(
    object: &Map<String, Value>,
    pointer: &str,
    out: &mut String,
) -> Result<(), GrammarError> {
    match object.get("additionalProperties") {
        None | Some(Value::Bool(false)) => {}
        Some(_) => {
            return Err(GrammarError::at(
                "additionalProperties",
                pointer,
                "a generated grammar is a closed object; only `false` (or nothing) matches it",
            ));
        }
    }
    let Some(Value::Object(properties)) = object.get("properties") else {
        return Err(GrammarError::at(
            "properties",
            pointer,
            "an object needs declared properties to become a grammar",
        ));
    };
    let required = required_names(object, properties, pointer)?;
    let (mandatory, optional): (Vec<&str>, Vec<&str>) = properties
        .keys()
        .map(String::as_str)
        .partition(|name| required.contains(name));
    if mandatory.is_empty() {
        return Err(GrammarError::at(
            "required",
            pointer,
            "an object whose members are all optional has nothing to anchor the separators to",
        ));
    }

    out.push_str("\"{\" ws ");
    for (index, name) in mandatory.iter().enumerate() {
        if index > 0 {
            out.push_str(" ws \",\" ws ");
        }
        write_member(properties, name, pointer, out)?;
    }
    for name in optional {
        out.push_str(" ( ws \",\" ws ");
        write_member(properties, name, pointer, out)?;
        out.push_str(" )?");
    }
    out.push_str(" ws \"}\"");
    Ok(())
}

/// Writes `"name" ws ":" ws <type>` for one property.
fn write_member(
    properties: &Map<String, Value>,
    name: &str,
    pointer: &str,
    out: &mut String,
) -> Result<(), GrammarError> {
    let Some(schema) = properties.get(name) else {
        return Err(GrammarError::at(
            "required",
            pointer,
            "names a property the schema does not declare",
        ));
    };
    write_literal(&Value::String(name.to_owned()), pointer, out)?;
    out.push_str(" ws \":\" ws ");
    write_expr(schema, &format!("{pointer}/{name}"), out)
}

/// Reads `required`, checking every name is a declared property.
fn required_names<'a>(
    object: &'a Map<String, Value>,
    properties: &Map<String, Value>,
    pointer: &str,
) -> Result<Vec<&'a str>, GrammarError> {
    let Some(value) = object.get("required") else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(GrammarError::at(
            "required",
            pointer,
            "`required` must be an array of property names",
        ));
    };
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else {
            return Err(GrammarError::at(
                "required",
                pointer,
                "`required` must be an array of property names",
            ));
        };
        if !properties.contains_key(name) {
            return Err(GrammarError::at(
                "required",
                pointer,
                "names a property the schema does not declare",
            ));
        }
        names.push(name);
    }
    Ok(names)
}

/// Writes a homogeneous array.
fn write_array(
    object: &Map<String, Value>,
    pointer: &str,
    out: &mut String,
) -> Result<(), GrammarError> {
    let Some(items) = object.get("items") else {
        return Err(GrammarError::at(
            "items",
            pointer,
            "an array of anything is not a constraint a grammar can carry",
        ));
    };
    if items.is_array() {
        return Err(GrammarError::at(
            "items",
            pointer,
            "positional (tuple) items are not expressible",
        ));
    }
    let mut element = String::new();
    write_expr(items, &format!("{pointer}/items"), &mut element)?;
    out.push_str("\"[\" ws ( ");
    out.push_str(&element);
    out.push_str(" ( ws \",\" ws ");
    out.push_str(&element);
    out.push_str(" )* )? ws \"]\"");
    Ok(())
}

/// Writes an alternation of scalar literals.
fn write_enum(members: &Value, pointer: &str, out: &mut String) -> Result<(), GrammarError> {
    let Some(items) = members.as_array().filter(|items| !items.is_empty()) else {
        return Err(GrammarError::at(
            "enum",
            pointer,
            "`enum` must be a non-empty array",
        ));
    };
    out.push_str("( ");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(" | ");
        }
        write_literal(item, pointer, out)?;
    }
    out.push_str(" )");
    Ok(())
}

/// Writes one JSON scalar as a GBNF literal.
fn write_literal(value: &Value, pointer: &str, out: &mut String) -> Result<(), GrammarError> {
    if value.is_object() || value.is_array() {
        return Err(GrammarError::at(
            "const",
            pointer,
            "only scalar constants and enum members become literals",
        ));
    }
    out.push('"');
    for ch in value.to_string().chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The conformance corpus schema, which a grammar profile must express.
    fn plan_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "acts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "operation": {"type": "string"},
                            "target": {"type": "string"},
                            "turnframe_conformance_marker": {"type": "boolean"}
                        },
                        "required": ["operation", "target"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["acts"],
            "additionalProperties": false
        })
    }

    /// The `root ::=` line, which is the only part this module generates.
    fn root_line(grammar: &str) -> &str {
        grammar
            .lines()
            .find_map(|line| line.strip_prefix("root ::= "))
            .expect("a grammar has a root")
    }

    #[test]
    fn the_corpus_schema_becomes_a_grammar_that_names_every_property() {
        let grammar = to_gbnf(&plan_schema()).expect("expressible");
        assert!(
            root_line(&grammar).starts_with("ws \"{\" ws \"\\\"acts\\\"\""),
            "{grammar}"
        );
        // Every property name travels as a literal, the optional one included:
        // that is what makes the schema recognizable on the wire.
        for name in [
            "acts",
            "operation",
            "target",
            "turnframe_conformance_marker",
        ] {
            assert!(
                grammar.contains(&format!("\\\"{name}\\\"")),
                "{name} missing:\n{grammar}"
            );
        }
        // The optional property is optional; the required ones are not.
        assert!(
            grammar.contains(
                "( ws \",\" ws \"\\\"turnframe_conformance_marker\\\"\" ws \":\" ws boolean )?"
            ),
            "{grammar}"
        );
        assert!(
            grammar.contains("\"[\" ws ("),
            "the array is an array:\n{grammar}"
        );
        for rule in DEFINED_RULES.iter().filter(|rule| **rule != "root") {
            assert!(
                grammar.contains(&format!("{rule} ::=")),
                "{rule} undefined:\n{grammar}"
            );
        }
    }

    #[test]
    fn the_generated_root_references_only_rules_the_preamble_defines() {
        // A grammar referencing an undefined rule is rejected by the server,
        // which would turn a translation bug into a 400 in production. Only
        // the root line is generated, so only it can drift; the preamble is a
        // constant asserted above.
        let grammar = to_gbnf(&plan_schema()).expect("expressible");
        for word in bare_words(root_line(&grammar)) {
            assert!(
                DEFINED_RULES.contains(&word.as_str()),
                "rule {word} is referenced and never defined:\n{grammar}"
            );
        }
    }

    /// Identifiers in `body` that sit outside a quoted literal.
    fn bare_words(body: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut in_literal = false;
        let mut escaped = false;
        for ch in body.chars() {
            if in_literal {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_literal = false;
                }
                continue;
            }
            if ch == '"' {
                in_literal = true;
                continue;
            }
            if ch.is_ascii_alphanumeric() || ch == '_' {
                word.push(ch);
            } else if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
        words
    }

    #[test]
    fn a_value_constraint_a_grammar_cannot_carry_is_refused_by_name() {
        let schema = json!({
            "type": "object",
            "properties": {"code": {"type": "string", "pattern": "^[A-Z]{3}$"}},
            "required": ["code"]
        });
        let error = to_gbnf(&schema).expect_err("a pattern is not expressible");
        assert_eq!(error.keyword, "pattern");
        assert_eq!(error.pointer, "/code");
        assert_eq!(error.code(), "pattern_at_/code");
        assert!(error.to_string().contains("weaker than the declaration"));
    }

    #[test]
    fn the_compositions_this_translation_does_not_attempt_are_named_too() {
        for (schema, keyword) in [
            (json!({"oneOf": [{"type": "string"}]}), "type"),
            (
                json!({"type": "object", "properties": {}, "$ref": "#/x"}),
                "$ref",
            ),
            (json!({"type": "array"}), "items"),
            (
                json!({"type": "array", "items": [{"type": "string"}]}),
                "items",
            ),
            (json!({"type": "string", "minLength": 3}), "minLength"),
            (json!({"enum": ["a"], "pattern": "x"}), "pattern"),
            (json!({"type": "geo"}), "type_geo"),
            (json!(true), "schema"),
        ] {
            let error = to_gbnf(&schema).expect_err("not expressible");
            assert_eq!(error.keyword, keyword, "for {schema}");
        }
    }

    #[test]
    fn an_object_with_nothing_required_has_no_anchor_and_says_so() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": false
        });
        let error = to_gbnf(&schema).expect_err("no anchor");
        assert_eq!(error.keyword, "required");
        assert!(error.reason.contains("anchor"));
    }

    #[test]
    fn open_objects_are_refused_rather_than_quietly_closed() {
        let open = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a"],
            "additionalProperties": true
        });
        let error = to_gbnf(&open).expect_err("an open object is not a closed grammar");
        assert_eq!(error.keyword, "additionalProperties");
    }

    #[test]
    fn scalars_enums_and_constants_all_become_literals() {
        let schema = json!({
            "type": "object",
            "properties": {
                "kind": {"enum": ["a", "b"]},
                "version": {"const": 2},
                "count": {"type": "integer"},
                "ratio": {"type": "number"},
                "flag": {"type": "boolean"},
                "nothing": {"type": "null"}
            },
            "required": ["kind", "version", "count", "ratio", "flag", "nothing"]
        });
        let grammar = to_gbnf(&schema).expect("expressible");
        assert!(
            grammar.contains("( \"\\\"a\\\"\" | \"\\\"b\\\"\" )"),
            "{grammar}"
        );
        assert!(grammar.contains(" ws \"2\""), "{grammar}");
        assert!(grammar.contains(" ws integer"), "{grammar}");
        assert!(grammar.contains(" ws \"null\""), "{grammar}");
        for word in bare_words(root_line(&grammar)) {
            assert!(DEFINED_RULES.contains(&word.as_str()), "{word}\n{grammar}");
        }
    }

    #[test]
    fn a_property_name_that_would_break_the_grammar_is_escaped() {
        let schema = json!({
            "type": "object",
            "properties": {"say \"hi\"": {"type": "string"}},
            "required": ["say \"hi\""]
        });
        let grammar = to_gbnf(&schema).expect("expressible");
        // The JSON encoding is `"say \"hi\""`; every quote and backslash of it
        // is escaped again for GBNF, so no literal ends early.
        assert!(
            grammar.contains("\"\\\"say \\\\\\\"hi\\\\\\\"\\\"\""),
            "{grammar}"
        );
        // `ws "{" ws "name" ws ":" ws string ws "}" ws`: the literals are
        // literals, and the only rules named are ones the preamble defines.
        assert_eq!(
            bare_words(root_line(&grammar)),
            vec!["ws", "ws", "ws", "ws", "string", "ws", "ws"]
        );
    }
}
