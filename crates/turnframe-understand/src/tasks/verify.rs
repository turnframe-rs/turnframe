//! `verify`: whether what was understood is what the user said. It can only take away.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_core::understanding::{ArgumentValue, MessageRef, RecordValue, UnderstoodArgument};
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::{Speaker, UnderstandingInput};
use crate::render;
use crate::schema::{object, one_of, text};
use crate::tasks::not_one_of;
use crate::words::Span;

const BUILT_IN: &str = include_str!("../../prompts/understand/verify.md");

/// The verification task, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct Verify<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> Verify<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }
}

/// One act as understood, shown for a person to check.
#[derive(Debug, Clone)]
pub struct VerifyInput<'a> {
    /// How the unit is shown.
    pub label: &'static str,
    /// Its words.
    pub words: Span,
    /// What the operation does, or what starting the workflow means.
    pub meaning: String,
    /// The record, as shown.
    pub record: String,
    /// The arguments understood, by name.
    pub arguments: &'a BTreeMap<String, UnderstoodArgument>,
    /// How each argument is labelled, by name.
    pub labels: BTreeMap<String, String>,
    /// How a record-valued argument's record is shown.
    pub record_labels: BTreeMap<String, String>,
    /// What a value of a closed set means, by argument, when the operation says.
    pub meanings: BTreeMap<String, String>,
    /// The operation, and which of the times the request asks for it this act is.
    pub occurrence: Option<(String, usize, usize)>,
    /// What the whole-turn check found, when this call reads the act again.
    pub note: Option<String>,
    /// Words of this message the unit continues, such as the request a correction changes.
    pub continues: Option<Span>,
}

/// The verdict on each argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentVerdict {
    /// The user gave that value.
    Stated,
    /// The user gave no value for it.
    NotStated,
    /// The user gave another value.
    Different,
    /// The user gave only part of it.
    Incomplete,
    /// It takes words that are not part of the value, such as the field's name.
    TooMuch,
}

/// The verdict on the act as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    /// The user asked for this operation on this record.
    Confirmed,
    /// The user did not ask for it.
    NotRequested,
    /// The user meant another record.
    WrongRecord,
}

/// What the verifier found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    /// Why, in a sentence; it reaches the repair when the act is not confirmed.
    pub reason: String,
    /// Each argument's verdict, by name.
    pub arguments: BTreeMap<String, ArgumentVerdict>,
    /// The act's verdict.
    pub overall: Overall,
}

impl Verdict {
    /// Whether everything checked out.
    #[must_use]
    pub fn confirmed(&self) -> bool {
        self.overall == Overall::Confirmed
            && self
                .arguments
                .values()
                .all(|verdict| *verdict == ArgumentVerdict::Stated)
    }

    /// The arguments found wanting, by name.
    #[must_use]
    pub fn at_fault(&self) -> Vec<String> {
        self.arguments
            .iter()
            .filter(|(_, verdict)| **verdict != ArgumentVerdict::Stated)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// The feedback a repair of the extraction is given.
    #[must_use]
    pub fn feedback(&self) -> String {
        self.feedback_given(|_| false)
    }

    /// The feedback, where `refers_back` names the arguments copied from this message in a
    /// conversation with earlier words: words there may only point at a value said before.
    #[must_use]
    pub fn feedback_given(&self, refers_back: impl Fn(&str) -> bool) -> String {
        let mut out = format!("A check of your answer found: {}", self.reason);
        for (name, verdict) in &self.arguments {
            let said = match verdict {
                ArgumentVerdict::Stated => continue,
                ArgumentVerdict::NotStated | ArgumentVerdict::Different if refers_back(name) => {
                    "you copied it from this message, which gives no value of its own there; \
                     words that only refer back to a value said before give that value: point \
                     at the value alone in the earlier message that says it, or give not_given \
                     when none does"
                }
                ArgumentVerdict::NotStated => "the user gave no value for it",
                ArgumentVerdict::Different => "the user gave another value",
                ArgumentVerdict::Incomplete => "the user gave more of it than you took",
                ArgumentVerdict::TooMuch => {
                    "it takes words that are not part of the value; point at the value alone"
                }
            };
            let _ = write!(out, "\n- {name}: {said}");
        }
        out
    }
}

/// Whether `argument` is words of this message copied as they stand, not a value computed
/// from them: a date or a choice read from words is never the words themselves.
fn copied_from_this_message(turn: &UnderstandingInput, argument: &UnderstoodArgument) -> bool {
    let (Some(excerpt), ArgumentValue::Json(Value::String(text))) =
        (argument.excerpt, &argument.value)
    else {
        return false;
    };
    let bare = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    excerpt.message == MessageRef::Current
        && turn
            .message
            .slice(crate::words::Span::new(
                excerpt.words.first,
                excerpt.words.last,
            ))
            .is_ok_and(|said| bare(said) == bare(text))
}

impl<'a> ModelTask for Verify<'a> {
    type Input = VerifyInput<'a>;
    type Output = Verdict;

    fn kind(&self) -> TaskKind {
        TaskKind::Verify
    }

    fn prompt_name(&self) -> &str {
        "understand.verify"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, input: &VerifyInput<'a>) -> Value {
        let verdicts = one_of([
            "stated",
            "not_stated",
            "different",
            "incomplete",
            "too_much",
        ]);
        let arguments = input
            .arguments
            .keys()
            .map(|name| (name.as_str(), verdicts.clone()))
            .collect();
        object(vec![
            ("reason", text("Why, in one sentence.")),
            ("arguments", object(arguments)),
            (
                "overall",
                one_of(["confirmed", "not_requested", "wrong_record"]),
            ),
        ])
    }

    fn render(&self, input: &VerifyInput<'a>) -> Vec<Message> {
        let turn = self.turn;
        let mut understood = String::from("Understood:");
        if input.arguments.is_empty() {
            understood.push_str(" no arguments.");
        }
        let mut cited = Vec::new();
        for (name, argument) in input.arguments {
            let label = input.labels.get(name).map_or(name.as_str(), String::as_str);
            let shown = render::understood(argument, turn, |record: &RecordValue| {
                input
                    .record_labels
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| format!("{record:?}"))
            });
            let _ = write!(understood, "\n- {name} ({label}): {shown}");
            if let Some(meaning) = input.meanings.get(name) {
                let _ = write!(understood, "\n  {meaning}");
            }
            // Only a copy from this message can be words pointing at an earlier value; a value
            // taken from the earlier message is what they point at.
            let copied_here = copied_from_this_message(turn, argument);
            if copied_here && turn.transcript.iter().any(|m| m.speaker == Speaker::User) {
                understood.push_str(
                    "\n  Copied from this message: words that only refer back to a value said \
                     before («what I told you before») are not that value.",
                );
            }
            if let Some(excerpt) = argument.excerpt
                && let MessageRef::Earlier { index } = excerpt.message
                && !cited.contains(&index)
            {
                cited.push(index);
            }
        }
        let earlier = cited
            .iter()
            .filter_map(|index| {
                let message = turn.transcript.get(*index)?;
                Some(format!(
                    "Earlier message {}: {}",
                    render::message_name(*index),
                    render::quoted(message.words.text())
                ))
            })
            .collect::<Vec<_>>();
        let words = &turn.message;
        // A correction answers for what the last turn did, never for the question asked next.
        let correction = input.label == "Correction";
        let said = |span: Span| render::quoted(words.slice(span).unwrap_or_default());
        // A correction of this message follows the request it corrects, as the user said them.
        let (unit, then) = match (correction, input.continues) {
            (true, Some(request)) => (
                format!("Request: {}", said(request)),
                Some(format!(
                    "Corrected by: {}, the user's last word on what it changes.",
                    said(input.words)
                )),
            ),
            // Named as the words the act reads, so they are never taken for its value.
            (_, continues) => (
                format!(
                    "The part of the message this act reads, {}: {}",
                    with_article(input.label),
                    said(input.words)
                ),
                continues.map(|span| format!("It continues: {}", said(span))),
            ),
        };
        vec![Message::user(render::sections([
            Some(format!("Operation: {}", input.meaning)),
            Some(format!("Record: {}", input.record)),
            Some(understood),
            Some(format!("Today: {}", turn.today.format("%A %-d %B %Y"))),
            (!earlier.is_empty()).then(|| earlier.join("\n")),
            correction.then(|| render::receipts(turn)).flatten(),
            render::last_assistant(turn),
            (!correction).then(|| render::expectation(turn)).flatten(),
            Some(format!(
                "The user's message: {}",
                render::quoted(words.text())
            )),
            Some(unit),
            then,
            input.occurrence.as_ref().map(|(operation, number, of)| {
                format!(
                    "This request asks for {operation} {of} times: this act is the {}, and other \
                     acts read the rest of the request. Judge only whether the user gave these \
                     values for it.",
                    ordinal(*number)
                )
            }),
            input
                .note
                .as_ref()
                .map(|note| format!("A check of the whole message found: {note}")),
        ]))]
    }

    fn check(&self, input: &VerifyInput<'a>, output: &Verdict) -> Result<(), StructuralError> {
        let expected: Vec<String> = input.arguments.keys().cloned().collect();
        if let Some(unknown) = output.arguments.keys().find(|n| !expected.contains(n)) {
            return Err(not_one_of("argument", unknown, &expected));
        }
        if let Some(missing) = expected.iter().find(|n| !output.arguments.contains_key(*n)) {
            return Err(StructuralError::new(
                "missing_argument",
                format!("`arguments.{missing}` is missing"),
            ));
        }
        Ok(())
    }

    fn agree(&self, left: &Verdict, right: &Verdict) -> bool {
        left.overall == right.overall && left.arguments == right.arguments
    }
}

/// «Answer» as «an answer», «Request» as «a request».
fn with_article(label: &str) -> String {
    let lower = label.to_lowercase();
    let article = if lower.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    format!("{article} {lower}")
}

/// `1` as «first», up to «tenth»; past it, «number 11».
fn ordinal(number: usize) -> String {
    const WORDS: [&str; 10] = [
        "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth",
        "tenth",
    ];
    number
        .checked_sub(1)
        .and_then(|index| WORDS.get(index))
        .map_or_else(|| format!("number {number}"), |word| (*word).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_from_the_whole_turn_check_is_shown() {
        let turn = UnderstandingInput::new("name Lisbon", "en-GB", chrono::NaiveDate::MIN);
        let arguments = BTreeMap::new();
        let input = VerifyInput {
            label: "Request",
            words: Span::new(0, 1),
            meaning: "Name the trip.".to_owned(),
            record: "Trip 1".to_owned(),
            arguments: &arguments,
            labels: BTreeMap::new(),
            record_labels: BTreeMap::new(),
            meanings: BTreeMap::new(),
            occurrence: None,
            note: Some("the message may not ask for this act".to_owned()),
            continues: None,
        };
        let rendered = format!("{:?}", Verify::new(&turn).render(&input));
        assert!(
            rendered.contains(
                "A check of the whole message found: the message may not ask for this act"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn an_answer_is_checked_against_the_question_it_answers() {
        let turn = UnderstandingInput::new("X", "en-GB", chrono::NaiveDate::MIN)
            .with_earlier(crate::Speaker::Assistant, "What is A?")
            .with_expectation(crate::Expectation::Obligation {
                record: turnframe_core::ids::TargetToken::new("t1"),
                sentence: "What is A?".to_owned(),
            });
        let arguments = BTreeMap::new();
        let input = VerifyInput {
            label: "Answer",
            words: Span::new(1, 1),
            meaning: "Set A.".to_owned(),
            record: "Record 1".to_owned(),
            arguments: &arguments,
            labels: BTreeMap::new(),
            record_labels: BTreeMap::new(),
            meanings: BTreeMap::new(),
            occurrence: None,
            note: None,
            continues: None,
        };
        let rendered = format!("{:?}", Verify::new(&turn).render(&input));
        assert!(
            rendered.contains("Last assistant message: «What is A?»"),
            "{rendered}"
        );
        assert!(
            rendered.contains("The assistant asked about: What is A?"),
            "{rendered}"
        );
    }

    #[test]
    fn a_correction_is_checked_with_the_words_it_continues() {
        let turn =
            UnderstandingInput::new("set A to X 2026, no, Y", "en-GB", chrono::NaiveDate::MIN);
        let arguments = BTreeMap::new();
        let input = VerifyInput {
            label: "Correction",
            words: Span::new(5, 6),
            meaning: "Set A.".to_owned(),
            record: "Record 1".to_owned(),
            arguments: &arguments,
            labels: BTreeMap::new(),
            record_labels: BTreeMap::new(),
            meanings: BTreeMap::new(),
            occurrence: None,
            note: None,
            continues: Some(Span::new(0, 4)),
        };
        let rendered = format!("{:?}", Verify::new(&turn).render(&input));
        let request = rendered
            .find("Request: «set A to X 2026,»")
            .expect(&rendered);
        let corrected = rendered
            .find("Corrected by: «no, Y», the user's last word on what it changes.")
            .expect(&rendered);
        assert!(request < corrected, "{rendered}");
    }

    #[test]
    fn a_correction_is_checked_against_what_the_last_turn_did() {
        let turn = UnderstandingInput::new("no, the other one", "en-GB", chrono::NaiveDate::MIN)
            .with_receipt(crate::PreviousReceipt::new("r1", "Field A set to X."))
            .with_expectation(crate::Expectation::Obligation {
                record: turnframe_core::ids::TargetToken::new("t1"),
                sentence: "What is B?".to_owned(),
            });
        let arguments = BTreeMap::new();
        let input = VerifyInput {
            label: "Correction",
            words: Span::new(1, 4),
            meaning: "Set A.".to_owned(),
            record: "Record 1".to_owned(),
            arguments: &arguments,
            labels: BTreeMap::new(),
            record_labels: BTreeMap::new(),
            meanings: BTreeMap::new(),
            occurrence: None,
            note: None,
            continues: None,
        };
        let rendered = format!("{:?}", Verify::new(&turn).render(&input));
        assert!(rendered.contains("r1: Field A set to X."), "{rendered}");
        assert!(!rendered.contains("What is B?"), "{rendered}");
    }

    #[test]
    fn an_occurrence_is_judged_as_the_one_it_is() {
        let turn = UnderstandingInput::new("add a bag and a meal", "en-GB", chrono::NaiveDate::MIN);
        let arguments = BTreeMap::new();
        let input = VerifyInput {
            label: "Request",
            words: Span::new(0, 4),
            meaning: "trip.add_extra: Add an extra.".to_owned(),
            record: "Trip 1".to_owned(),
            arguments: &arguments,
            labels: BTreeMap::new(),
            record_labels: BTreeMap::new(),
            meanings: BTreeMap::new(),
            occurrence: Some(("trip.add_extra".to_owned(), 2, 2)),
            note: None,
            continues: None,
        };
        let rendered = format!("{:?}", Verify::new(&turn).render(&input));
        assert!(
            rendered.contains(
                "This request asks for trip.add_extra 2 times: this act is the second, and \
                 other acts read the rest of the request. Judge only whether the user gave these \
                 values for it."
            ),
            "{rendered}"
        );
    }

    #[test]
    fn a_value_of_a_closed_set_is_shown_with_what_it_means() {
        let turn =
            UnderstandingInput::new("none, she never joined", "en-GB", chrono::NaiveDate::MIN);
        let arguments = BTreeMap::from([(
            "reason".to_owned(),
            UnderstoodArgument {
                value: turnframe_core::understanding::ArgumentValue::Json("not_applicable".into()),
                excerpt: None,
            },
        )]);
        let input = VerifyInput {
            label: "Request",
            words: Span::new(0, 3),
            meaning: "Decline the number.".to_owned(),
            record: "Traveler 1".to_owned(),
            arguments: &arguments,
            labels: BTreeMap::new(),
            record_labels: BTreeMap::new(),
            meanings: BTreeMap::from([(
                "reason".to_owned(),
                "not_applicable when there is none to give".to_owned(),
            )]),
            occurrence: None,
            note: None,
            continues: None,
        };
        let rendered = format!("{:?}", Verify::new(&turn).render(&input));
        assert!(
            rendered.contains("- reason (reason): «not_applicable», given earlier\\n  not_applicable when there is none to give"),
            "{rendered}"
        );
    }
}
