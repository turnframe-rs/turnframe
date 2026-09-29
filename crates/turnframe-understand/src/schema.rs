//! Builders for task answer schemas: closed objects, every property required, and every
//! choice an enum of what is valid for this call.

use serde_json::{Map, Value, json};

/// A closed object whose properties are all required, in the order given.
pub(crate) fn object(properties: Vec<(&str, Value)>) -> Value {
    let required: Vec<Value> = properties
        .iter()
        .map(|(name, _)| Value::from(*name))
        .collect();
    let properties: Map<String, Value> = properties
        .into_iter()
        .map(|(name, schema)| (name.to_owned(), schema))
        .collect();
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

/// One of `values`.
pub(crate) fn one_of<I, S>(values: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let values: Vec<Value> = values.into_iter().map(|v| Value::from(v.into())).collect();
    json!({ "type": "string", "enum": values })
}

/// The constant `value`, which tags a union branch.
pub(crate) fn constant(value: &str) -> Value {
    json!({ "type": "string", "const": value })
}

/// A branch of a union tagged by its `kind`.
pub(crate) fn variant(kind: &str, mut properties: Vec<(&str, Value)>) -> Value {
    properties.insert(0, ("kind", constant(kind)));
    object(properties)
}

/// Any one of `branches`, which must be tagged apart.
pub(crate) fn any_of(branches: Vec<Value>) -> Value {
    json!({ "anyOf": branches })
}

/// `schema` or null.
pub(crate) fn nullable(schema: Value) -> Value {
    json!({ "anyOf": [schema, { "type": "null" }] })
}

/// A list of `items`.
pub(crate) fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

/// A word number, counted from 1.
pub(crate) fn index() -> Value {
    json!({ "type": "integer", "minimum": 1 })
}

/// A range of words, first to last inclusive.
pub(crate) fn span() -> Value {
    object(vec![("from", index()), ("to", index())])
}

/// Free text with a description.
pub(crate) fn text(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

/// A date as the user said it: the shape of `DateExpr`, with no references.
pub(crate) fn date_expression() -> Value {
    let weekday = one_of([
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ]);
    let period = one_of(["week", "month", "quarter", "year"]);
    let which = one_of(["this", "next", "last"]);
    any_of(vec![
        variant(
            "absolute",
            vec![
                ("year", nullable(json!({ "type": "integer" }))),
                (
                    "month",
                    json!({ "type": "integer", "minimum": 1, "maximum": 12 }),
                ),
                (
                    "day",
                    json!({ "type": "integer", "minimum": 1, "maximum": 31 }),
                ),
            ],
        ),
        variant(
            "relative",
            vec![
                ("unit", one_of(["day", "week", "month", "year"])),
                ("amount", json!({ "type": "integer" })),
            ],
        ),
        variant(
            "weekday",
            vec![
                ("day", weekday),
                (
                    "which",
                    one_of(["coming", "previous", "this_week", "next_week", "last_week"]),
                ),
            ],
        ),
        variant(
            "period_end",
            vec![("period", period.clone()), ("which", which.clone())],
        ),
        variant("period_start", vec![("period", period), ("which", which)]),
    ])
}

#[cfg(test)]
mod tests {
    use turnframe_core::operation::DateExpr;

    use super::*;

    #[test]
    fn a_closed_object_requires_every_property_in_order() {
        let schema = object(vec![("b", index()), ("a", index())]);
        assert_eq!(schema["required"], json!(["b", "a"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        let order: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
        assert_eq!(order, ["b", "a"]);
    }

    #[test]
    fn the_date_schema_accepts_what_date_expressions_deserialize_from() {
        let validator = jsonschema::validator_for(&date_expression()).unwrap();
        for value in [
            json!({"kind": "absolute", "year": null, "month": 3, "day": 1}),
            json!({"kind": "relative", "unit": "day", "amount": 1}),
            json!({"kind": "weekday", "day": "friday", "which": "coming"}),
            json!({"kind": "period_end", "period": "month", "which": "this"}),
        ] {
            assert!(validator.is_valid(&value), "{value}");
            serde_json::from_value::<DateExpr>(value).unwrap();
        }
    }
}
