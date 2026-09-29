//! `locate`: which record a request is about, when more than one could be.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_core::ids::WorkflowKey;
use turnframe_core::operation::OperationSpec;
use turnframe_core::understanding::ActId;
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::{Expectation, RecordBrief, UnderstandingInput};
use crate::render;
use crate::schema::{nullable, object, one_of, span};
use crate::tasks::{check_one_of, check_span};
use crate::words::Span;

/// The answer for a record the request creates.
pub const NEW: &str = "new";
/// The answer for a record the user names that is not listed.
pub const BY_NAME: &str = "by_name";
/// The answer when several listed records fit.
pub const AMBIGUOUS: &str = "ambiguous";

const BUILT_IN: &str = include_str!("../../prompts/understand/locate.md");

/// The locating task, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct Locate<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> Locate<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }
}

/// A record a request may be about.
#[derive(Debug, Clone, Copy)]
pub enum Candidate<'a> {
    /// A record in view.
    Record(&'a RecordBrief),
    /// The record an earlier act of this message creates.
    SameTurn {
        /// That act.
        act: ActId,
        /// The words that asked for it.
        words: Span,
    },
}

/// One request and the records it may be about.
#[derive(Debug, Clone)]
pub struct LocateInput<'a> {
    /// How the unit is shown.
    pub label: &'static str,
    /// Its words.
    pub words: Span,
    /// The operation chosen for it.
    pub spec: &'a OperationSpec,
    /// Its workflow.
    pub workflow: &'a WorkflowKey,
    /// The records it may be about.
    pub candidates: Vec<Candidate<'a>>,
    /// Whether it may create a record.
    pub allow_new: bool,
    /// Whether it may name a record not in view.
    pub allow_not_listed: bool,
    /// What the whole-turn check found, when this call reads the act again.
    pub note: Option<String>,
}

impl LocateInput<'_> {
    /// Each candidate with the handle it is listed under: `r1` for a record in view, `s1`
    /// for one this message creates.
    #[must_use]
    pub fn handles(&self) -> Vec<(String, &Candidate<'_>)> {
        let (mut records, mut same_turn) = (0, 0);
        self.candidates
            .iter()
            .map(|candidate| {
                let handle = match candidate {
                    Candidate::Record(_) => {
                        records += 1;
                        format!("r{records}")
                    }
                    Candidate::SameTurn { .. } => {
                        same_turn += 1;
                        format!("s{same_turn}")
                    }
                };
                (handle, candidate)
            })
            .collect()
    }

    /// Every answer allowed.
    #[must_use]
    pub fn choices(&self) -> Vec<String> {
        let mut choices: Vec<String> = self
            .handles()
            .into_iter()
            .map(|(handle, _)| handle)
            .collect();
        if self.allow_new {
            choices.push(NEW.to_owned());
        }
        if self.allow_not_listed {
            choices.push(BY_NAME.to_owned());
        }
        choices.push(AMBIGUOUS.to_owned());
        choices
    }

    /// The candidate listed under `handle`.
    #[must_use]
    pub fn candidate(&self, handle: &str) -> Option<&Candidate<'_>> {
        self.handles()
            .into_iter()
            .find(|(listed, _)| listed == handle)
            .map(|(_, candidate)| candidate)
    }
}

/// The record chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// A handle, `new`, `by_name` or `ambiguous`.
    pub record: String,
    /// The words naming a record that is not listed.
    pub named: Option<Span>,
}

impl<'a> ModelTask for Locate<'a> {
    type Input = LocateInput<'a>;
    type Output = Location;

    fn kind(&self) -> TaskKind {
        TaskKind::Locate
    }

    fn prompt_name(&self) -> &str {
        "understand.locate"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, input: &LocateInput<'a>) -> Value {
        object(vec![
            ("record", one_of(input.choices())),
            ("named", nullable(span())),
        ])
    }

    fn render(&self, input: &LocateInput<'a>) -> Vec<Message> {
        let words = &self.turn.message;
        let mut records = String::from("Records:");
        let mut card_handle = None;
        let mut last_about = Vec::new();
        // Only an answer is pointed at the record the question asked about.
        let asked = (input.label == "Answer")
            .then(|| self.turn.expectation.as_ref().and_then(Expectation::record))
            .flatten();
        let mut asked_handle = None;
        for (handle, candidate) in input.handles() {
            match candidate {
                Candidate::Record(record) => {
                    let _ = write!(
                        records,
                        "\n- {handle}: {}",
                        render::record_line(record, true)
                    );
                    let on_card = self.turn.card.as_ref().and_then(|c| c.record.as_ref());
                    if on_card == Some(&record.token) {
                        card_handle = Some(handle.clone());
                    }
                    if self.turn.last_subjects.contains(&record.token) {
                        last_about.push(handle);
                    } else if asked == Some(&record.token) {
                        asked_handle = Some(handle);
                    }
                }
                Candidate::SameTurn { words: span, .. } => {
                    let said = words.slice(*span).unwrap_or_default();
                    let _ = write!(
                        records,
                        "\n- {handle}: the {} record this message creates, words {} to {}, {}",
                        input.workflow,
                        span.shown().0,
                        span.shown().1,
                        render::quoted(said)
                    );
                }
            }
        }
        if input.allow_new {
            let _ = write!(records, "\n- {NEW}: a new {} record", input.workflow);
        }
        if input.allow_not_listed {
            let _ = write!(
                records,
                "\n- {BY_NAME}: a record the user names that is not listed"
            );
        }
        let _ = write!(records, "\n- {AMBIGUOUS}: more than one listed record fits");
        vec![Message::user(render::sections([
            Some(format!(
                "Operation: {}",
                render::operation_line(input.spec, self.turn)
            )),
            Some(records),
            card_handle.map(|handle| format!("The card on screen is about {handle}.")),
            render::last_assistant(self.turn),
            last_about_line(&last_about, asked_handle.as_deref()),
            Some(render::message(words)),
            Some(render::unit(input.label, words, input.words)),
            input
                .note
                .as_ref()
                .map(|note| format!("A check of the whole message found: {note}")),
        ]))]
    }

    fn check(&self, input: &LocateInput<'a>, output: &Location) -> Result<(), StructuralError> {
        check_one_of("record", &output.record, &input.choices())?;
        if output.record == BY_NAME {
            let Some(named) = output.named else {
                return Err(StructuralError::new(
                    "missing_named",
                    "`named` must point at the words naming the record when `record` is by_name",
                ));
            };
            check_span("named", named, &self.turn.message)?;
        }
        Ok(())
    }

    fn agree(&self, left: &Location, right: &Location) -> bool {
        left.record == right.record
    }
}

/// The records the last assistant message was about, and apart the one its question asked
/// about when that is another.
fn last_about_line(changed: &[String], asked: Option<&str>) -> Option<String> {
    match (changed.is_empty(), asked) {
        (true, None) => None,
        (true, Some(asked)) => Some(format!("The last assistant message was about {asked}.")),
        (false, None) => Some(format!(
            "The last assistant message was about {}.",
            changed.join(" and ")
        )),
        (false, Some(asked)) => Some(format!(
            "The last assistant message was about {}. Its question was about {asked}.",
            changed.join(" and ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_from_the_whole_turn_check_is_shown() {
        let turn = UnderstandingInput::new("name Lisbon", "en-GB", chrono::NaiveDate::MIN);
        let spec = OperationSpec::new("trip.set_name").summary("Name the trip.");
        let key = WorkflowKey::from("trip");
        let input = LocateInput {
            label: "Request",
            words: Span::new(0, 1),
            spec: &spec,
            workflow: &key,
            candidates: Vec::new(),
            allow_new: true,
            allow_not_listed: false,
            note: Some("the record meant is named in «rent»".to_owned()),
        };
        let rendered = format!("{:?}", Locate::new(&turn).render(&input));
        assert!(
            rendered.contains(
                "A check of the whole message found: the record meant is named in «rent»"
            ),
            "{rendered}"
        );
    }

    fn two_records() -> (RecordBrief, RecordBrief, OperationSpec, WorkflowKey) {
        (
            RecordBrief::new("t_a", "A 1", "open"),
            RecordBrief::new("t_b", "A 2", "open"),
            OperationSpec::new("a.set_b").summary("Set B."),
            WorkflowKey::from("a"),
        )
    }

    /// What locate is shown for a unit labelled `label`, after a reply that changed A 1
    /// and asked its question about `asked`.
    fn shown(label: &'static str, asked: &str) -> String {
        let (first, second, spec, key) = two_records();
        let turn = UnderstandingInput::new("X", "en-GB", chrono::NaiveDate::MIN)
            .with_last_subject("t_a".into())
            .with_expectation(Expectation::Obligation {
                record: asked.into(),
                sentence: "What is B?".to_owned(),
            });
        let input = LocateInput {
            label,
            words: Span::new(0, 0),
            spec: &spec,
            workflow: &key,
            candidates: vec![Candidate::Record(&first), Candidate::Record(&second)],
            allow_new: false,
            allow_not_listed: false,
            note: None,
        };
        format!("{:?}", Locate::new(&turn).render(&input))
    }

    #[test]
    fn an_answer_is_told_the_record_the_question_asked_about() {
        let rendered = shown("Answer", "t_b");
        assert!(
            rendered
                .contains("The last assistant message was about r1. Its question was about r2."),
            "{rendered}"
        );
    }

    #[test]
    fn a_request_is_not_told_the_record_the_question_asked_about() {
        let rendered = shown("Request", "t_b");
        assert!(
            rendered.contains("The last assistant message was about r1."),
            "{rendered}"
        );
        assert!(!rendered.contains("Its question was about"), "{rendered}");
    }

    #[test]
    fn a_question_about_the_record_changed_adds_nothing() {
        let rendered = shown("Answer", "t_a");
        assert!(!rendered.contains("Its question was about"), "{rendered}");
    }
}
