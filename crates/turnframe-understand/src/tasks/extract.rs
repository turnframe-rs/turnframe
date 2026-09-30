//! `extract`: each argument's value as the user stated it, or `not_given`.
//!
//! A value is given in its argument's shape and always points at the words that state
//! it. Code turns it into the operation's value: words become text, a date expression a
//! date, an amount money, a handle a record ([`crate::values`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use turnframe_core::ids::OperationKey;
use turnframe_core::operation::{ArgumentSpec, DateExpr, OperationSpec, ValueShape};
use turnframe_core::understanding::RecordValue;
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::{RecordBrief, UnderstandingInput, WorkflowBrief};
use crate::render;
use crate::schema::{any_of, date_expression, index, object, one_of, variant};
use crate::values;
use crate::words::Span;

/// The message name of the user's current message.
pub const CURRENT: &str = "current";

/// The record handle for a record the user names that is not listed.
pub const BY_NAME: &str = "by_name";

const BUILT_IN: &str = include_str!("../../prompts/understand/extract.md");

/// The extraction task, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct Extract<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> Extract<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }

    /// The turn it runs over.
    #[must_use]
    pub const fn turn(&self) -> &'a UnderstandingInput {
        self.turn
    }
}

/// The record an act applies to, as extraction shows it.
#[derive(Debug, Clone, Copy)]
pub enum RecordContext<'a> {
    /// A record in view.
    Existing(&'a RecordBrief),
    /// A record the act creates.
    New,
    /// A record an earlier act of this message creates.
    SameTurn,
    /// No record.
    Nothing,
}

/// A record an argument may name, with the handle it is listed under.
#[derive(Debug, Clone)]
pub struct RecordChoice {
    /// The handle.
    pub handle: String,
    /// What it resolves to.
    pub value: RecordValue,
    /// How it is shown.
    pub label: String,
}

/// One act's arguments to extract, and their context.
#[derive(Debug, Clone)]
pub struct ExtractInput<'a> {
    /// How the unit is shown.
    pub label: &'static str,
    /// Its words.
    pub words: Span,
    /// The operation.
    pub spec: &'a OperationSpec,
    /// Its workflow.
    pub workflow: &'a WorkflowBrief,
    /// The record it applies to.
    pub record: RecordContext<'a>,
    /// The arguments asked for, in declaration order.
    pub arguments: Vec<&'a ArgumentSpec>,
    /// The records each record-valued argument may name.
    pub record_choices: BTreeMap<String, Vec<RecordChoice>>,
    /// Words of this message the unit continues, such as the request a correction changes.
    pub continues: Option<Span>,
    /// Words of this message the other parts hold.
    pub others: Vec<Span>,
    /// Words of the other parts asking for this same operation: a value the segmentation
    /// cut in two runs on into a neighbouring one.
    pub kin: Vec<Span>,
    /// The other operations the same request asks for.
    pub also: Vec<OperationKey>,
    /// How many earlier messages are shown.
    pub transcript: usize,
    /// What the whole-turn check found, when this call reads the act again.
    pub note: Option<String>,
    /// Which of the unit's acts of this operation to read, and how many it asks for.
    pub occurrence: Option<(usize, usize)>,
    /// The dates a correction changes, by argument: one it gives without a year takes theirs.
    pub corrected: BTreeMap<String, chrono::NaiveDate>,
}

impl ExtractInput<'_> {
    /// The names a value's `message` may take.
    #[must_use]
    pub fn messages(&self, turn: &UnderstandingInput) -> Vec<String> {
        let mut names = vec![CURRENT.to_owned()];
        names.extend(render::shown_messages(turn, self.transcript));
        names
    }
}

/// Every argument's value, by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Extraction {
    /// The values.
    pub arguments: BTreeMap<String, Given>,
}

/// One argument's value as the model gave it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum Given {
    /// The user did not state it.
    NotGiven,
    /// The user's words are the value. `text` copies them and narrows the pointer to
    /// them; left empty, the pointer stands.
    Words {
        text: String,
        message: String,
        from: usize,
        to: usize,
    },
    /// Text the model wrote from the words pointed at.
    Written {
        message: String,
        from: usize,
        to: usize,
        text: String,
    },
    /// A value of a closed set, a number or a flag.
    Value {
        message: String,
        from: usize,
        to: usize,
        value: Value,
    },
    /// A date as the user said it.
    Date {
        message: String,
        from: usize,
        to: usize,
        date: DateExpr,
    },
    /// An amount of money.
    Money {
        message: String,
        from: usize,
        to: usize,
        amount: String,
        currency: String,
    },
    /// A record, by its handle; one not listed, by its name as written.
    Record {
        message: String,
        from: usize,
        to: usize,
        record: String,
        #[serde(default)]
        name: String,
    },
}

impl Given {
    /// The message and words a given value points at.
    #[must_use]
    pub fn pointer(&self) -> Option<(&str, Span)> {
        match self {
            Self::NotGiven => None,
            Self::Words {
                message, from, to, ..
            }
            | Self::Written {
                message, from, to, ..
            }
            | Self::Value {
                message, from, to, ..
            }
            | Self::Date {
                message, from, to, ..
            }
            | Self::Money {
                message, from, to, ..
            }
            | Self::Record {
                message, from, to, ..
            } => Some((message, Span::from_shown(*from, *to))),
        }
    }
}

/// The kind of value an argument of this shape is given as.
fn given_kind(shape: &ValueShape) -> &'static str {
    match shape {
        ValueShape::Text { written: false } => "words",
        ValueShape::Text { written: true } => "written",
        ValueShape::Date { .. } => "date",
        ValueShape::Money => "money",
        ValueShape::Record { .. } => "record",
        _ => "value",
    }
}

fn argument_schema(
    argument: &ArgumentSpec,
    spec: &OperationSpec,
    messages: &[String],
    records: Option<&Vec<RecordChoice>>,
) -> Value {
    let pointer = || {
        vec![
            ("message", one_of(messages.iter().cloned())),
            ("from", index()),
            ("to", index()),
        ]
    };
    let mut fields = pointer();
    let kind = given_kind(&argument.shape);
    match &argument.shape {
        ValueShape::Text { written: false } => fields.insert(
            0,
            (
                "text",
                json!({
                    "type": "string",
                    "description": "The value's own words, copied exactly as the user wrote them, without the punctuation that ends their sentence."
                }),
            ),
        ),
        ValueShape::Text { written: true } => fields.push(("text", json!({ "type": "string" }))),
        ValueShape::Enum { values } => fields.push(("value", one_of(values.iter().cloned()))),
        ValueShape::Integer => fields.push(("value", json!({ "type": "integer" }))),
        ValueShape::Number => fields.push(("value", json!({ "type": "number" }))),
        ValueShape::Bool => fields.push(("value", json!({ "type": "boolean" }))),
        ValueShape::Date { .. } => fields.push(("date", date_expression())),
        ValueShape::Money => {
            fields.push(("amount", json!({ "type": "string" })));
            fields.push(("currency", json!({ "type": "string" })));
        }
        ValueShape::Record { .. } => {
            let mut handles: Vec<String> = records
                .map(|choices| choices.iter().map(|c| c.handle.clone()).collect())
                .unwrap_or_default();
            handles.push(BY_NAME.to_owned());
            // The name comes first, as a text value's words do: copying it anchors the
            // choice, where a bare handle list reads as «nothing here» when it is short.
            fields.insert(
                0,
                (
                    "name",
                    json!({
                        "type": "string",
                        "description": "The words naming the record, copied exactly as the user wrote them, without the punctuation that ends their sentence."
                    }),
                ),
            );
            fields.push(("record", one_of(handles)));
        }
        _ => fields.push(("value", structured(spec, &argument.name))),
    }
    any_of(vec![variant("not_given", vec![]), variant(kind, fields)])
}

/// The argument's own schema from the arguments type, with no references left.
fn structured(spec: &OperationSpec, name: &str) -> Value {
    let root = spec.arguments_schema.as_value();
    let property = root
        .pointer(&format!("/properties/{name}"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let defs = root.get("$defs").cloned();
    inline(property, defs.as_ref(), 0)
}

fn inline(value: Value, defs: Option<&Value>, depth: usize) -> Value {
    match value {
        Value::Object(map) if depth < 16 => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                let name = reference.rsplit('/').next().unwrap_or_default();
                let target = defs.and_then(|d| d.get(name)).cloned().unwrap_or(json!({}));
                return inline(target, defs, depth + 1);
            }
            Value::Object(
                map.into_iter()
                    .map(|(key, child)| (key, inline(child, defs, depth + 1)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| inline(item, defs, depth + 1))
                .collect(),
        ),
        other => other,
    }
}

impl<'a> ModelTask for Extract<'a> {
    type Input = ExtractInput<'a>;
    type Output = Extraction;

    fn kind(&self) -> TaskKind {
        TaskKind::Extract
    }

    fn prompt_name(&self) -> &str {
        "understand.extract"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, input: &ExtractInput<'a>) -> Value {
        let messages = input.messages(self.turn);
        let properties = input
            .arguments
            .iter()
            .map(|argument| {
                (
                    argument.name.as_str(),
                    argument_schema(
                        argument,
                        input.spec,
                        &messages,
                        input.record_choices.get(&argument.name),
                    ),
                )
            })
            .collect();
        object(vec![("arguments", object(properties))])
    }

    fn render(&self, input: &ExtractInput<'a>) -> Vec<Message> {
        let turn = self.turn;
        let words = &turn.message;
        let record = match input.record {
            RecordContext::Existing(record) => {
                Some(format!("Record: {}", render::record_line(record, false)))
            }
            RecordContext::New => Some(format!("Record: a new {} record", input.workflow.key)),
            RecordContext::SameTurn => Some(format!(
                "Record: the {} record this message creates",
                input.workflow.key
            )),
            RecordContext::Nothing => None,
        };
        let mut choices = String::new();
        for (name, listed) in &input.record_choices {
            if !choices.is_empty() {
                choices.push_str("\n\n");
            }
            if listed.is_empty() {
                let _ = write!(
                    choices,
                    "No record is listed for {name}: one the user names is {BY_NAME}, with its name."
                );
                continue;
            }
            let _ = write!(choices, "Records {name} may name:");
            for choice in listed {
                let _ = write!(choices, "\n- {}: {}", choice.handle, choice.label);
            }
            let _ = write!(
                choices,
                "\n- {BY_NAME}: a record the user names that is not listed"
            );
        }
        let asked: Vec<&str> = input.arguments.iter().map(|a| a.name.as_str()).collect();
        let offered = input
            .spec
            .arguments
            .iter()
            .filter(|argument| render::model_given(&argument.source))
            .count();
        let only = (asked.len() < offered).then(|| format!("Give only: {}.", asked.join(", ")));
        vec![Message::user(render::sections([
            Some(format!(
                "Operation: {}",
                render::operation_line(input.spec, self.turn)
            )),
            input
                .spec
                .guidance
                .as_ref()
                .map(|g| format!("Guidance: {g}")),
            render::arguments(input.spec, turn),
            only,
            render::examples(input.spec),
            render::glossary(input.workflow),
            record,
            (!choices.is_empty()).then_some(choices),
            Some(format!("Today: {}", turn.today.format("%A %-d %B %Y"))),
            render::transcript(turn, input.transcript),
            Some(render::titled_message(
                &format!("Message ({CURRENT})"),
                words,
            )),
            Some(render::unit(input.label, words, input.words)),
            input
                .continues
                .map(|span| render::unit("It continues", words, span)),
            (!input.also.is_empty()).then(|| {
                let keys: Vec<&str> = input.also.iter().map(OperationKey::as_str).collect();
                format!(
                    "This request also asks for {}: the words of its values are that \
                     operation's.",
                    keys.join(", ")
                )
            }),
            (!input.others.is_empty()).then(|| {
                let spans: Vec<String> = input
                    .others
                    .iter()
                    .map(|span| {
                        let (from, to) = span.shown();
                        format!("words {from} to {to}")
                    })
                    .collect();
                format!(
                    "Other parts of the message, each read on its own, give none of this \
                     part's values: {}.",
                    spans.join(", ")
                )
            }),
            input.occurrence.map(|(number, of)| {
                format!(
                    "This request asks for {} {of} times: give only the values of occurrence \
                     {number} of {of}, counting in the order the message says them.",
                    input.spec.key
                )
            }),
            input
                .note
                .as_ref()
                .map(|note| format!("A check of the whole message found: {note}")),
        ]))]
    }

    fn check(&self, input: &ExtractInput<'a>, output: &Extraction) -> Result<(), StructuralError> {
        values::convert(self.turn, input, output).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_from_the_whole_turn_check_is_shown() {
        let turn = UnderstandingInput::new("name Lisbon", "en-GB", chrono::NaiveDate::MIN);
        let spec = OperationSpec::new("trip.set_name").summary("Name the trip.");
        let workflow = WorkflowBrief::new("trip");
        let input = ExtractInput {
            label: "Request",
            words: Span::new(0, 1),
            spec: &spec,
            workflow: &workflow,
            record: RecordContext::Nothing,
            arguments: Vec::new(),
            record_choices: BTreeMap::new(),
            continues: None,
            others: Vec::new(),
            kin: Vec::new(),
            also: Vec::new(),
            transcript: 0,
            note: Some("the value of value is in «rent»".to_owned()),
            occurrence: None,
            corrected: std::collections::BTreeMap::new(),
        };
        let rendered = format!("{:?}", Extract::new(&turn).render(&input));
        assert!(
            rendered
                .contains("A check of the whole message found: the value of value is in «rent»"),
            "{rendered}"
        );
    }

    #[test]
    fn the_words_other_parts_hold_are_named() {
        let turn =
            UnderstandingInput::new("name Lisbon, fly tomorrow", "en-GB", chrono::NaiveDate::MIN);
        let spec = OperationSpec::new("trip.set_name").summary("Name the trip.");
        let workflow = WorkflowBrief::new("trip");
        let input = ExtractInput {
            label: "Request",
            words: Span::new(0, 1),
            spec: &spec,
            workflow: &workflow,
            record: RecordContext::Nothing,
            arguments: Vec::new(),
            record_choices: BTreeMap::new(),
            continues: None,
            others: vec![Span::new(2, 3)],
            kin: Vec::new(),
            also: Vec::new(),
            transcript: 0,
            note: None,
            occurrence: None,
            corrected: std::collections::BTreeMap::new(),
        };
        let rendered = format!("{:?}", Extract::new(&turn).render(&input));
        assert!(
            rendered.contains(
                "Other parts of the message, each read on its own, give none of this part's \
                 values: words 3 to 4."
            ),
            "{rendered}"
        );
    }

    #[test]
    fn the_other_operations_of_the_request_are_named() {
        let turn = UnderstandingInput::new(
            "name Lisbon and fly tomorrow",
            "en-GB",
            chrono::NaiveDate::MIN,
        );
        let spec = OperationSpec::new("trip.set_name").summary("Name the trip.");
        let workflow = WorkflowBrief::new("trip");
        let input = ExtractInput {
            label: "Request",
            words: Span::new(0, 4),
            spec: &spec,
            workflow: &workflow,
            record: RecordContext::Nothing,
            arguments: Vec::new(),
            record_choices: BTreeMap::new(),
            continues: None,
            others: Vec::new(),
            kin: Vec::new(),
            also: vec!["trip.set_travel_date".into()],
            transcript: 0,
            note: None,
            occurrence: None,
            corrected: std::collections::BTreeMap::new(),
        };
        let rendered = format!("{:?}", Extract::new(&turn).render(&input));
        assert!(
            rendered.contains(
                "This request also asks for trip.set_travel_date: the words of its values are \
                 that operation's."
            ),
            "{rendered}"
        );
    }

    #[test]
    fn a_given_value_deserializes_by_its_kind() {
        let given: Given = serde_json::from_value(json!({
            "kind": "date", "message": "current", "from": 4, "to": 4,
            "date": {"kind": "relative", "unit": "day", "amount": 1}
        }))
        .unwrap();
        assert_eq!(
            given.pointer(),
            Some(("current", Span::new(3, 3))),
            "a model counts words from 1"
        );
        let absent: Given = serde_json::from_value(json!({"kind": "not_given"})).unwrap();
        assert_eq!(absent, Given::NotGiven);
    }
}
