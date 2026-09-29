//! JSON ↔ Smithy [`Document`].
//!
//! The Converse API carries every model-facing JSON payload — a tool's input
//! schema, a tool call's arguments, a tool result rendered as JSON — as an
//! [`aws_smithy_types::Document`] rather than as text. The normalized layer
//! speaks [`serde_json::Value`] for the same payloads (spec §20.1: model-facing
//! arguments are the one place a `Value` is the right type), so every crossing
//! of the boundary goes through this module.
//!
//! # The one lossy edge, and why it fails closed
//!
//! `Document::Number` is a Smithy number: an unsigned integer, a negative
//! integer, or a 64-bit float. JSON's number grammar is wider — a literal may
//! carry more precision than an `f64` holds, and a `f64` may be `NaN` or an
//! infinity, neither of which JSON can spell. Conversion therefore has exactly
//! two documented outcomes:
//!
//! * a JSON number that fits none of the three Smithy shapes becomes
//!   [`Document::Null`], never a rounded guess;
//! * a `NaN` or an infinity coming back becomes [`Value::Null`], for the same
//!   reason.
//!
//! Both are unreachable for the payloads this adapter actually carries — a JSON
//! Schema and a model's arguments — and both are a null rather than a wrong
//! number, which a schema check then rejects instead of accepting silently.

use std::collections::HashMap;

use aws_smithy_types::{Document, Number};
use serde_json::{Map, Value};

/// Converts a JSON value into the Smithy document the SDK sends.
pub(crate) fn to_document(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(flag) => Document::Bool(*flag),
        Value::Number(number) => number_to_document(number),
        Value::String(text) => Document::String(text.clone()),
        Value::Array(items) => Document::Array(items.iter().map(to_document).collect()),
        Value::Object(fields) => Document::Object(
            fields
                .iter()
                .map(|(key, field)| (key.clone(), to_document(field)))
                .collect::<HashMap<String, Document>>(),
        ),
    }
}

/// Converts a Smithy document from the wire into a JSON value.
pub(crate) fn from_document(document: &Document) -> Value {
    match document {
        Document::Null => Value::Null,
        Document::Bool(flag) => Value::Bool(*flag),
        Document::Number(number) => number_to_value(*number),
        Document::String(text) => Value::String(text.clone()),
        Document::Array(items) => Value::Array(items.iter().map(from_document).collect()),
        Document::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, field)| (key.clone(), from_document(field)))
                .collect::<Map<String, Value>>(),
        ),
    }
}

/// Maps a JSON number onto the three shapes Smithy has.
fn number_to_document(number: &serde_json::Number) -> Document {
    if let Some(value) = number.as_u64() {
        return Document::Number(Number::PosInt(value));
    }
    if let Some(value) = number.as_i64() {
        return Document::Number(Number::NegInt(value));
    }
    if let Some(value) = number.as_f64() {
        return Document::Number(Number::Float(value));
    }
    // A literal that fits no machine number: a null the schema rejects beats a
    // silently rounded one it accepts.
    Document::Null
}

/// Maps a Smithy number back onto a JSON number.
fn number_to_value(number: Number) -> Value {
    match number {
        Number::PosInt(value) => Value::Number(value.into()),
        Number::NegInt(value) => Value::Number(value.into()),
        Number::Float(value) => {
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::from)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_schema_round_trips_through_the_smithy_document() {
        let schema = json!({
            "type": "object",
            "properties": {
                "acts": {"type": "array", "items": {"type": "object"}},
                "count": {"type": "integer"}
            },
            "required": ["acts"],
            "additionalProperties": false
        });
        let round_tripped = from_document(&to_document(&schema));
        assert_eq!(round_tripped, schema);
    }

    #[test]
    fn every_scalar_shape_survives_both_directions() {
        let value = json!({
            "null": null,
            "true": true,
            "false": false,
            "positive": 42,
            "negative": -7,
            "float": 1.5,
            "text": "ciao",
            "nested": [1, [2, {"deep": "value"}]]
        });
        assert_eq!(from_document(&to_document(&value)), value);
    }

    #[test]
    fn a_number_no_machine_type_holds_becomes_a_null_rather_than_a_guess() {
        // Serde parses this as an f64 with arbitrary_precision off, so the
        // interesting case is the reverse direction: an infinity has no JSON
        // spelling and must not become a rounded finite number.
        assert_eq!(number_to_value(Number::Float(f64::INFINITY)), Value::Null);
        assert_eq!(number_to_value(Number::Float(f64::NAN)), Value::Null);
        assert_eq!(number_to_value(Number::NegInt(-3)), json!(-3));
        assert_eq!(number_to_value(Number::PosInt(3)), json!(3));
    }

    #[test]
    fn object_key_order_does_not_change_the_value() {
        // The Smithy document is a hash map, so key order is not preserved on
        // the way through; the value must still compare equal.
        let value = json!({"b": 1, "a": 2, "c": {"z": 0, "y": 1}});
        assert_eq!(from_document(&to_document(&value)), value);
    }
}
