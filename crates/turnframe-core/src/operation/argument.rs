//! One argument of an operation: what people call it, where its value comes from, and
//! the shape a model gives it.

use serde::{Deserialize, Serialize};

use crate::ids::{ReadToolKey, WorkflowKey};
use crate::locale::Locale;
use crate::operation::value::DateDirection;

/// Where an argument's value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ArgumentSource {
    /// The user states it; a verifier checks they did.
    #[default]
    User,
    /// The model may deduce it from what the user said; a verifier checks it is consistent.
    Inferred,
    /// Code fills it from a declared read after the plan exists. The model never sees it.
    Server {
        /// The read that supplies it.
        read: ReadToolKey,
    },
}

/// The shape a model gives an argument's value, derived from its JSON Schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ValueShape {
    /// Text. A user-sourced text is pointed at in the user's words unless `written`.
    Text {
        /// The model may phrase it; otherwise it selects the user's own words.
        written: bool,
    },
    /// One of a closed set of values.
    Enum {
        /// The values.
        values: Vec<String>,
    },
    /// A whole number.
    Integer,
    /// A number.
    Number,
    /// True or false.
    Bool,
    /// A calendar date, given as a date expression code evaluates.
    Date {
        /// Which way a date without a year points.
        direction: DateDirection,
    },
    /// An amount of money, given as a decimal and a currency code evaluates.
    Money,
    /// A record of a workflow, chosen among the records in view.
    Record {
        /// The workflow the record belongs to.
        workflow: WorkflowKey,
    },
    /// Anything else, written against its own schema.
    Structured,
}

/// A word people use for an argument, optionally only in one language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentLabel {
    /// The language, or `None` for every language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<Locale>,
    /// The word.
    pub text: String,
}

/// One argument of an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ArgumentSpec {
    /// Its name among the arguments' top-level fields.
    pub name: String,
    /// What it is, from the argument type's documentation unless replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The words people use for it. Hints for the model, never matched by code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<ArgumentLabel>,
    /// Whether an act cannot run without it.
    pub required: bool,
    /// Where its value comes from.
    pub source: ArgumentSource,
    /// The shape a model gives its value.
    pub shape: ValueShape,
    /// Whether its value names the record the operation creates, for the acts of the
    /// same turn that point at that record before it has a label.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub names_the_record: bool,
}

impl ArgumentSpec {
    /// An argument named `name` of `shape`, user-sourced and optional.
    #[must_use]
    pub fn new(name: impl Into<String>, shape: ValueShape) -> Self {
        Self {
            name: name.into(),
            description: None,
            labels: Vec::new(),
            required: false,
            source: ArgumentSource::User,
            shape,
            names_the_record: false,
        }
    }

    /// Adds a word people use for it, in every language.
    #[must_use]
    pub fn label(mut self, text: impl Into<String>) -> Self {
        self.labels.push(ArgumentLabel {
            locale: None,
            text: text.into(),
        });
        self
    }

    /// Adds a word people use for it in `locale`.
    #[must_use]
    pub fn label_in(mut self, locale: impl Into<Locale>, text: impl Into<String>) -> Self {
        self.labels.push(ArgumentLabel {
            locale: Some(locale.into()),
            text: text.into(),
        });
        self
    }

    /// Replaces its description.
    #[must_use]
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Makes it required.
    #[must_use]
    pub const fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Says its value names the record the operation creates.
    #[must_use]
    pub const fn names_the_record(mut self) -> Self {
        self.names_the_record = true;
        self
    }

    /// Makes it optional.
    #[must_use]
    pub const fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    /// Lets the model deduce it from what the user said.
    #[must_use]
    pub fn inferred(mut self) -> Self {
        self.source = ArgumentSource::Inferred;
        self
    }

    /// Fills it from `read` after the plan exists.
    #[must_use]
    pub fn from_read(mut self, read: impl Into<ReadToolKey>) -> Self {
        self.source = ArgumentSource::Server { read: read.into() };
        self
    }

    /// Lets the model phrase a text value instead of selecting the user's words.
    #[must_use]
    pub fn written(mut self) -> Self {
        if let ValueShape::Text { written } = &mut self.shape {
            *written = true;
        }
        self
    }

    /// Points a date without a year in `direction`.
    #[must_use]
    pub fn date_direction(mut self, direction: DateDirection) -> Self {
        if let ValueShape::Date { direction: current } = &mut self.shape {
            *current = direction;
        }
        self
    }

    /// Gives it as an amount of money.
    #[must_use]
    pub fn money(mut self) -> Self {
        self.shape = ValueShape::Money;
        self
    }

    /// Gives it as a record of `workflow`. The operation receives the record's
    /// [`CaseRef`](crate::case::CaseRef) fields, with `label` when the record was in
    /// view, or, for one the same turn opens, the value of the opening operation's
    /// argument that [names it](Self::names_the_record). A record named but not in view is looked
    /// up in the case directory, and asked for again when none or several match.
    #[must_use]
    pub fn record(mut self, workflow: impl Into<WorkflowKey>) -> Self {
        self.shape = ValueShape::Record {
            workflow: workflow.into(),
        };
        self
    }

    /// The labels that apply in `locale`: its own ones first, then the universal ones.
    pub fn labels_for<'a>(&'a self, locale: &'a Locale) -> impl Iterator<Item = &'a str> + 'a {
        let own = self
            .labels
            .iter()
            .filter(move |label| label.locale.as_ref() == Some(locale));
        let universal = self.labels.iter().filter(|label| label.locale.is_none());
        own.chain(universal).map(|label| label.text.as_str())
    }
}

/// Derives the shape of one property of an arguments schema.
///
/// `defs` is the schema's `$defs`, for properties that reference an enum there.
#[must_use]
pub fn shape_of(property: &serde_json::Value, defs: Option<&serde_json::Value>) -> ValueShape {
    if let Some(value) = without_null(property) {
        return shape_of(&value, defs);
    }
    let resolved = resolve_ref(property, defs).unwrap_or(property);
    if let Some(values) = enum_values(resolved) {
        return ValueShape::Enum { values };
    }
    let kind = resolved.get("type").and_then(serde_json::Value::as_str);
    match kind {
        Some("string")
            if resolved.get("format").and_then(serde_json::Value::as_str) == Some("date") =>
        {
            ValueShape::Date {
                direction: DateDirection::Any,
            }
        }
        Some("string") => ValueShape::Text { written: false },
        Some("integer") => ValueShape::Integer,
        Some("number") => ValueShape::Number,
        Some("boolean") => ValueShape::Bool,
        _ => ValueShape::Structured,
    }
}

/// An optional property's schema without the `null` that makes it optional, or `None`.
fn without_null(property: &serde_json::Value) -> Option<serde_json::Value> {
    let is_null = |kind: &serde_json::Value| kind.as_str() == Some("null");
    if let Some(kinds) = property.get("type").and_then(serde_json::Value::as_array) {
        let kept: Vec<&serde_json::Value> = kinds.iter().filter(|kind| !is_null(kind)).collect();
        let [only] = kept.as_slice() else {
            return None;
        };
        let mut value = property.clone();
        value["type"] = (*only).clone();
        return Some(value);
    }
    let branches = property
        .get("anyOf")
        .or_else(|| property.get("oneOf"))?
        .as_array()?;
    let kept: Vec<serde_json::Value> = branches
        .iter()
        .filter(|branch| !branch.get("type").is_some_and(is_null))
        .cloned()
        .collect();
    match kept.as_slice() {
        _ if kept.len() == branches.len() => None,
        [only] => Some(only.clone()),
        _ => Some(serde_json::json!({ "anyOf": kept })),
    }
}

fn resolve_ref<'a>(
    property: &'a serde_json::Value,
    defs: Option<&'a serde_json::Value>,
) -> Option<&'a serde_json::Value> {
    let reference = property.get("$ref")?.as_str()?;
    let name = reference.rsplit('/').next()?;
    defs?.get(name)
}

fn enum_values(schema: &serde_json::Value) -> Option<Vec<String>> {
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
        return values
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect();
    }
    let branches = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))?
        .as_array()?;
    branches
        .iter()
        .map(|branch| branch.get("const")?.as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_optional_argument_has_the_shape_of_its_value() {
        let defs = json!({"Kind": {"enum": ["a", "b"]}});
        let text = ValueShape::Text { written: false };
        assert_eq!(shape_of(&json!({"type": ["string", "null"]}), None), text);
        assert_eq!(
            shape_of(
                &json!({"anyOf": [{"type": "string"}, {"type": "null"}]}),
                None
            ),
            text
        );
        assert_eq!(
            shape_of(
                &json!({"anyOf": [{"$ref": "#/$defs/Kind"}, {"type": "null"}]}),
                Some(&defs)
            ),
            ValueShape::Enum {
                values: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(
            shape_of(&json!({"type": ["integer", "null"]}), None),
            ValueShape::Integer
        );
    }

    #[test]
    fn shapes_follow_the_argument_types_schema() {
        let defs = json!({"Kind": {"oneOf": [{"type": "string", "const": "a"}, {"type": "string", "const": "b"}]}});
        assert_eq!(
            shape_of(&json!({"type": "string"}), None),
            ValueShape::Text { written: false }
        );
        assert_eq!(
            shape_of(&json!({"type": "string", "format": "date"}), None),
            ValueShape::Date {
                direction: DateDirection::Any
            }
        );
        assert_eq!(
            shape_of(&json!({"$ref": "#/$defs/Kind"}), Some(&defs)),
            ValueShape::Enum {
                values: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(
            shape_of(&json!({"type": "integer"}), None),
            ValueShape::Integer
        );
        assert_eq!(
            shape_of(&json!({"type": "object"}), None),
            ValueShape::Structured
        );
    }

    #[test]
    fn labels_in_the_turns_language_come_first() {
        let spec = ArgumentSpec::new("value", ValueShape::Text { written: false })
            .label("subject")
            .label_in("it-IT", "oggetto");
        let (it, en) = (Locale::from("it-IT"), Locale::from("en-GB"));
        let italian: Vec<&str> = spec.labels_for(&it).collect();
        assert_eq!(italian, vec!["oggetto", "subject"]);
        let english: Vec<&str> = spec.labels_for(&en).collect();
        assert_eq!(english, vec!["subject"]);
    }
}
