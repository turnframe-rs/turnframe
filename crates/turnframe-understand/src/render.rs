//! The labelled sections a task's context is written in, one function per section.
//!
//! Sections are plain lines in a fixed order, static parts first. Earlier messages are
//! named `m<n>` by their position in the turn's transcript, which is also what a
//! pointer into one of them names.

use std::fmt::Write as _;

use turnframe_core::flow::StateField;
use turnframe_core::operation::{ArgumentSource, OperationSpec, ValueShape};
use turnframe_core::understanding::{ArgumentValue, RecordValue, UnderstoodArgument};

use crate::input::{Expectation, RecordBrief, Speaker, UnderstandingInput, WorkflowBrief};
use crate::words::{Span, Words};

/// The name an earlier message is shown and pointed at by.
pub(crate) fn message_name(index: usize) -> String {
    format!("m{}", index + 1)
}

/// Text in guillemets, which no language uses inside a value.
pub(crate) fn quoted(text: &str) -> String {
    format!("«{text}»")
}

/// A state value as a person reads it.
pub(crate) fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "none".to_owned(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `field: value` pairs, all of them or only the identifying ones.
pub(crate) fn fields(fields: &[StateField], identifying_only: bool) -> String {
    fields
        .iter()
        .filter(|field| field.identifying || !identifying_only)
        .map(|field| format!("{}: {}", field.field, value_text(&field.value)))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// One line for a record: label, phase and fields.
pub(crate) fn record_line(record: &RecordBrief, identifying_only: bool) -> String {
    let mut line = format!("{} · {}", record.label, record.phase);
    let shown = fields(&record.fields, identifying_only);
    if !shown.is_empty() {
        let _ = write!(line, " · {shown}");
    }
    line
}

/// `Workflows:` and one line per workflow.
pub(crate) fn workflows(input: &UnderstandingInput) -> String {
    let mut out = String::from("Workflows:");
    for workflow in &input.workflows {
        match &workflow.summary {
            Some(summary) => {
                let _ = write!(out, "\n- {}: {summary}", workflow.key);
            }
            None => {
                let _ = write!(out, "\n- {}", workflow.key);
            }
        }
    }
    out
}

/// The workflow's glossary, or nothing.
pub(crate) fn glossary(workflow: &WorkflowBrief) -> Option<String> {
    if workflow.glossary.is_empty() {
        return None;
    }
    let mut out = String::from("Glossary:");
    for term in &workflow.glossary {
        let _ = write!(out, "\n- {}: {}", term.term, term.meaning);
    }
    Some(out)
}

/// The assistant's last message, or nothing.
pub(crate) fn last_assistant(input: &UnderstandingInput) -> Option<String> {
    let message = input
        .transcript
        .iter()
        .rev()
        .find(|message| message.speaker == Speaker::Assistant)?;
    Some(format!(
        "Last assistant message: {}",
        quoted(message.words.text())
    ))
}

/// The user's message before the assistant's last one, or nothing: what an answer completes.
pub(crate) fn user_before_last_assistant(input: &UnderstandingInput) -> Option<String> {
    let asked = input
        .transcript
        .iter()
        .rposition(|message| message.speaker == Speaker::Assistant)?;
    let message = input.transcript[..asked]
        .iter()
        .rev()
        .find(|message| message.speaker == Speaker::User)?;
    Some(format!(
        "Last user message: {}",
        quoted(message.words.text())
    ))
}

/// The last `count` earlier messages, numbered word by word so values can be pointed at.
pub(crate) fn transcript(input: &UnderstandingInput, count: usize) -> Option<String> {
    let start = input.transcript.len().saturating_sub(count);
    let shown = &input.transcript[start..];
    if shown.is_empty() {
        return None;
    }
    let mut out = String::from("Earlier messages, oldest first:");
    for (offset, message) in shown.iter().enumerate() {
        let speaker = match message.speaker {
            Speaker::User => "user",
            Speaker::Assistant => "assistant",
        };
        let _ = write!(
            out,
            "\n{} ({speaker}):\n{}",
            message_name(start + offset),
            message.words.render()
        );
    }
    Some(out)
}

/// The earlier messages shown by [`transcript`], by name.
pub(crate) fn shown_messages(input: &UnderstandingInput, count: usize) -> Vec<String> {
    let start = input.transcript.len().saturating_sub(count);
    (start..input.transcript.len()).map(message_name).collect()
}

/// The card on screen, or nothing.
pub(crate) fn card(input: &UnderstandingInput) -> Option<String> {
    let card = input.card.as_ref()?;
    let mut out = format!("Card on screen: {}", quoted(&card.question));
    let options: Vec<String> = card
        .options
        .iter()
        .map(|option| format!("{} {}", option.id, quoted(&option.label)))
        .collect();
    if !options.is_empty() {
        let _ = write!(out, "\nOptions: {}", options.join(", "));
    }
    if !card.accepts_typed_answer {
        out.push_str("\nOnly a click answers it; typed text does not.");
    }
    Some(out)
}

/// What the assistant asked for last turn, or nothing.
pub(crate) fn expectation(input: &UnderstandingInput) -> Option<String> {
    match input.expectation.as_ref()? {
        Expectation::Values(pending) => {
            let (_, spec) = input.operation(&pending.operation)?;
            let labels: Vec<String> = pending
                .missing
                .iter()
                .map(|name| argument_label(spec, name, input))
                .collect();
            let record = pending
                .record
                .as_ref()
                .and_then(|token| input.record(token))
                .map(|(_, record)| format!(" of {}", record.label))
                .unwrap_or_default();
            let mut asked = format!(
                "The assistant asked for: {}{record}, to {}.",
                labels.join(", "),
                spec.summary.trim_end_matches('.').to_lowercase()
            );
            // A record given by a name nothing holds yet: what creates it is what is asked.
            for name in &pending.missing {
                let given = pending.given.get(name).map(|argument| &argument.value);
                if let Some(ArgumentValue::Record(RecordValue::Named { workflow, named })) = given {
                    let label = argument_label(spec, name, input);
                    let _ = write!(
                        asked,
                        " The {label} given, «{named}», is not a {workflow} record yet."
                    );
                }
            }
            Some(asked)
        }
        Expectation::Obligation { record, sentence } => {
            let label = input
                .record(record)
                .map_or_else(String::new, |(_, record)| format!(" ({})", record.label));
            Some(format!("The assistant asked about: {sentence}{label}"))
        }
    }
}

/// The receipts of the last turn, or nothing.
pub(crate) fn receipts(input: &UnderstandingInput) -> Option<String> {
    if input.receipts.is_empty() {
        return None;
    }
    let mut out = String::from("Changes the assistant reported last turn:");
    for receipt in &input.receipts {
        let _ = write!(out, "\n- {}: {}", receipt.key, receipt.text);
    }
    Some(out)
}

/// The numbered message.
pub(crate) fn message(words: &Words) -> String {
    titled_message("Message", words)
}

/// The numbered message under its own heading.
pub(crate) fn titled_message(title: &str, words: &Words) -> String {
    format!("{title}:\n{}", words.render())
}

/// Which words of the message a unit is, labelled.
pub(crate) fn unit(label: &str, words: &Words, span: Span) -> String {
    let text = words.slice(span).unwrap_or_default();
    format!(
        "{label}: words {} to {}, {}",
        span.shown().0,
        span.shown().1,
        quoted(text)
    )
}

/// `key: summary` for an operation, the summary in the turn's language when it has one.
pub(crate) fn operation_line(spec: &OperationSpec, input: &UnderstandingInput) -> String {
    format!("{}: {}", spec.key, spec.summary_for(&input.locale))
}

/// The label an argument is shown by in the turn's language, or its name.
pub(crate) fn argument_label(
    spec: &OperationSpec,
    name: &str,
    input: &UnderstandingInput,
) -> String {
    spec.argument_named(name)
        .and_then(|argument| argument.labels_for(&input.locale).next())
        .map_or_else(|| name.to_owned(), str::to_owned)
}

/// One line per argument the model gives: name, labels, description, need and shape.
pub(crate) fn arguments(spec: &OperationSpec, input: &UnderstandingInput) -> Option<String> {
    let mut lines = Vec::new();
    for argument in spec.arguments.iter().filter(|a| model_given(&a.source)) {
        let labels: Vec<&str> = argument.labels_for(&input.locale).collect();
        let mut line = format!("- {}", argument.name);
        if !labels.is_empty() {
            let _ = write!(line, " ({})", labels.join(" / "));
        }
        line.push(':');
        if let Some(description) = &argument.description {
            let _ = write!(line, " {description}");
        }
        line.push_str(if argument.required {
            " Required."
        } else {
            " Optional."
        });
        if let Some(hint) = shape_hint(&argument.shape, &argument.source) {
            let _ = write!(line, " {hint}");
        }
        lines.push(line);
    }
    (!lines.is_empty()).then(|| format!("Arguments:\n{}", lines.join("\n")))
}

/// Whether the model gives this argument, as opposed to a read.
pub(crate) const fn model_given(source: &ArgumentSource) -> bool {
    !matches!(source, ArgumentSource::Server { .. })
}

fn shape_hint(shape: &ValueShape, source: &ArgumentSource) -> Option<&'static str> {
    let hint = match shape {
        ValueShape::Text { written: false } => "Point at the user's own words.",
        ValueShape::Text { written: true } => "Write it, and point at the words it comes from.",
        ValueShape::Date { .. } => "Give the date as the user said it; do not compute it.",
        ValueShape::Money => "Give the amount with a dot for decimals, and the currency code.",
        ValueShape::Record { .. } => {
            "A record the user names: one of those listed, or by_name with its name as the user wrote it."
        }
        _ => {
            return matches!(source, ArgumentSource::Inferred)
                .then_some("May be deduced from what the user said.");
        }
    };
    Some(hint)
}

/// The operation's examples, as the words a pointer would select.
pub(crate) fn examples(spec: &OperationSpec) -> Option<String> {
    if spec.examples.is_empty() {
        return None;
    }
    let mut out = String::from("Examples:");
    for example in &spec.examples {
        let mut parts: Vec<String> = example
            .arguments
            .iter()
            .map(|(name, value)| {
                let shown = match value {
                    serde_json::Value::String(text) => quoted(text),
                    other => other.to_string(),
                };
                format!("{name}: {shown}")
            })
            .collect();
        parts.extend(
            example
                .not_given
                .iter()
                .map(|name| format!("{name}: not given")),
        );
        let _ = write!(
            out,
            "\n- {} → {}",
            quoted(&example.message),
            parts.join("; ")
        );
    }
    Some(out)
}

/// An understood argument as a person reads it, with the words it comes from.
pub(crate) fn understood(
    argument: &UnderstoodArgument,
    input: &UnderstandingInput,
    record_label: impl Fn(&RecordValue) -> String,
) -> String {
    let value = match &argument.value {
        ArgumentValue::Json(value) => match value {
            // A date is shown as a person reads it, so «tomorrow» can be judged against it.
            serde_json::Value::String(text) => {
                match chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
                    Ok(day) => format!("{} ({})", quoted(text), day.format("%A %-d %B %Y")),
                    Err(_) => quoted(text),
                }
            }
            other => other.to_string(),
        },
        ArgumentValue::Record(record) => record_label(record),
    };
    let Some(excerpt) = argument.excerpt else {
        return format!("{value}, given earlier");
    };
    let words = match excerpt.message {
        turnframe_core::understanding::MessageRef::Current => Some(&input.message),
        turnframe_core::understanding::MessageRef::Earlier { index } => {
            input.transcript.get(index).map(|message| &message.words)
        }
    };
    let said = words
        .and_then(|words| {
            words
                .slice(Span::new(excerpt.words.first, excerpt.words.last))
                .ok()
        })
        .unwrap_or_default()
        // The punctuation a sentence puts after a value is no part of it, as extraction reads it.
        .trim_end_matches([',', ';', ':']);
    match excerpt.message {
        turnframe_core::understanding::MessageRef::Earlier { index } => format!(
            "{value} (from earlier message {}: {})",
            message_name(index),
            quoted(said)
        ),
        _ => format!("{value} (from {})", quoted(said)),
    }
}

/// Sections joined by blank lines, skipping the absent ones.
pub(crate) fn sections<I>(parts: I) -> String
where
    I: IntoIterator<Item = Option<String>>,
{
    parts.into_iter().flatten().collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use turnframe_core::understanding::{ArgumentValue, Excerpt, MessageRef, WordRange};

    use super::*;

    #[test]
    fn the_evidence_of_a_value_leaves_out_the_punctuation_its_sentence_puts_after_it() {
        let input = UnderstandingInput::new(
            "the email is x@y.example, and more",
            "en-GB",
            chrono::NaiveDate::MIN,
        );
        let argument = UnderstoodArgument {
            value: ArgumentValue::Json("x@y.example".into()),
            excerpt: Some(Excerpt {
                message: MessageRef::Current,
                words: WordRange {
                    first: 3,
                    last: 3,
                    start: 13,
                    end: 25,
                },
            }),
        };
        let shown = understood(&argument, &input, |_| String::new());
        assert_eq!(shown, "«x@y.example» (from «x@y.example»)");
    }

    #[test]
    fn a_value_taken_from_an_earlier_message_names_that_message() {
        let input = UnderstandingInput::new("the one I said", "en-GB", chrono::NaiveDate::MIN)
            .with_earlier(crate::Speaker::User, "call it Porto");
        let argument = UnderstoodArgument {
            value: ArgumentValue::Json("Porto".into()),
            excerpt: Some(Excerpt {
                message: MessageRef::Earlier { index: 0 },
                words: WordRange {
                    first: 2,
                    last: 2,
                    start: 8,
                    end: 13,
                },
            }),
        };
        let shown = understood(&argument, &input, |_| String::new());
        assert_eq!(shown, "«Porto» (from earlier message m1: «Porto»)");
    }
}
