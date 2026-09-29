//! `route`: which operations a request asks for, among those on offer, in the order asked.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_core::ids::WorkflowKey;
use turnframe_core::operation::OperationSpec;
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::{UnderstandingInput, WorkflowBrief};
use crate::render;
use crate::schema::{array, object, one_of};
use crate::tasks::check_one_of;
use crate::words::Span;

/// The answer when no operation on offer does what was asked.
pub const NONE: &str = "none";

/// Prefixes the answer that starts a new case: `start:trip`.
pub const START: &str = "start:";

const BUILT_IN: &str = include_str!("../../prompts/understand/route.md");

/// The routing task, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct Route<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> Route<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }
}

/// One unit to route and what is on offer to it.
#[derive(Debug, Clone)]
pub struct RouteInput<'a> {
    /// `Request`, `Correction` or `Cancel`, as the unit is shown.
    pub label: &'static str,
    /// Its words.
    pub words: Span,
    /// The workflows whose operations are on offer.
    pub workflows: Vec<&'a WorkflowBrief>,
    /// What a second route is told of the first reading, or nothing.
    pub note: Option<String>,
    /// The words of the message's other parts, each routed on its own.
    pub others: Vec<Span>,
}

impl RouteInput<'_> {
    /// The operations on offer: proposable ones, each once.
    #[must_use]
    pub fn offered(&self) -> Vec<&OperationSpec> {
        self.workflows
            .iter()
            .flat_map(|workflow| workflow.operations.iter())
            .filter(|spec| spec.availability.is_proposable())
            .collect()
    }

    /// The workflows a new case may be started of, save those offering an operation
    /// that opens one: that operation carries arguments, a bare start does not.
    #[must_use]
    pub fn startable(&self) -> Vec<&WorkflowKey> {
        let offered = self.offered();
        self.workflows
            .iter()
            .filter(|workflow| workflow.startable)
            .filter(|workflow| {
                !workflow
                    .new_case
                    .iter()
                    .any(|key| offered.iter().any(|spec| &spec.key == key))
            })
            .map(|workflow| &workflow.key)
            .collect()
    }

    /// Every answer allowed.
    #[must_use]
    pub fn choices(&self) -> Vec<String> {
        let mut choices: Vec<String> = self
            .offered()
            .iter()
            .map(|spec| spec.key.to_string())
            .collect();
        choices.extend(self.startable().iter().map(|key| format!("{START}{key}")));
        choices.push(NONE.to_owned());
        choices
    }
}

/// The operations chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routing {
    /// Operation keys and `start:<workflow>` in the order asked, or `none` alone.
    pub operations: Vec<String>,
}

impl<'a> ModelTask for Route<'a> {
    type Input = RouteInput<'a>;
    type Output = Routing;

    /// Readings that find no operation never make a majority: a split with one that finds one
    /// is read once more, shown them all.
    fn agree(&self, left: &Routing, right: &Routing) -> bool {
        left == right && left.operations.iter().all(|operation| operation != NONE)
    }

    fn kind(&self) -> TaskKind {
        TaskKind::Route
    }

    fn prompt_name(&self) -> &str {
        "understand.route"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, input: &RouteInput<'a>) -> Value {
        object(vec![("operations", array(one_of(input.choices())))])
    }

    fn render(&self, input: &RouteInput<'a>) -> Vec<Message> {
        let mut operations = String::from("Operations:");
        for spec in input.offered() {
            let _ = write!(
                operations,
                "\n- {}",
                render::operation_line(spec, self.turn)
            );
        }
        for key in input.startable() {
            let _ = write!(operations, "\n- {START}{key}: start a new {key} record");
        }
        let _ = write!(
            operations,
            "\n- {NONE}: no operation listed does what is asked"
        );
        let mut records = String::from("Records:");
        for workflow in &input.workflows {
            for record in &workflow.records {
                let _ = write!(records, "\n- {}", render::record_line(record, true));
                if !record.obligations.is_empty() {
                    let _ = write!(records, " · still needs: {}", record.obligations.join("; "));
                }
                if let Some(briefing) = &record.briefing {
                    let _ = write!(records, "\n  Guidance: {briefing}");
                }
            }
        }
        let has_records = input.workflows.iter().any(|w| !w.records.is_empty());
        // A correction changes something the last turn did: routing sees what that was.
        let corrected = (input.label == "Correction")
            .then(|| render::receipts(self.turn))
            .flatten();
        // An answer completes what the user said before the question it answers.
        let completed = (input.label == "Answer")
            .then(|| render::user_before_last_assistant(self.turn))
            .flatten();
        vec![Message::user(render::sections([
            Some(operations),
            has_records.then_some(records),
            corrected,
            completed,
            render::last_assistant(self.turn),
            render::expectation(self.turn),
            Some(render::message(&self.turn.message)),
            Some(render::unit(input.label, &self.turn.message, input.words)),
            (!input.others.is_empty()).then(|| {
                let parts: Vec<String> = input
                    .others
                    .iter()
                    .map(|span| {
                        let (from, to) = span.shown();
                        let said = self.turn.message.slice(*span).unwrap_or_default();
                        format!("words {from} to {to}, «{said}»")
                    })
                    .collect();
                format!(
                    "Other parts of the message are routed on their own, and what they ask for \
                     is not this request's: {}.",
                    parts.join("; ")
                )
            }),
            input.note.clone(),
        ]))]
    }

    fn check(&self, input: &RouteInput<'a>, output: &Routing) -> Result<(), StructuralError> {
        let choices = input.choices();
        let operations = &output.operations;
        for operation in operations {
            check_one_of("operations", operation, &choices)?;
        }
        if operations.is_empty() {
            return Err(StructuralError::new(
                "no_operation",
                "`operations` is empty; list what the request asks for, or none alone",
            ));
        }
        if operations.len() > 1 && operations.iter().any(|operation| operation == NONE) {
            return Err(StructuralError::new(
                "none_among_others",
                "none stands alone: it says that nothing listed does what is asked",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Speaker;

    #[test]
    fn readings_that_find_no_operation_never_outvote_one_that_finds_one() {
        let turn = UnderstandingInput::new("can I?", "en-GB", chrono::NaiveDate::MIN);
        let route = Route::new(&turn);
        let reading = |ops: &[&str]| Routing {
            operations: ops.iter().map(|op| (*op).to_owned()).collect(),
        };
        assert!(!route.agree(&reading(&[NONE]), &reading(&[NONE])));
        assert!(route.agree(&reading(&["trip.set_name"]), &reading(&["trip.set_name"])));
        assert!(!route.agree(&reading(&["trip.set_name"]), &reading(&[NONE])));
    }

    fn rendered(label: &'static str) -> String {
        let turn = UnderstandingInput::new("10 each", "en-GB", chrono::NaiveDate::MIN)
            .with_earlier(Speaker::User, "I sold 2 hats")
            .with_earlier(Speaker::Assistant, "At what price?");
        let input = RouteInput {
            label,
            words: Span::new(0, 1),
            workflows: Vec::new(),
            note: None,
            others: Vec::new(),
        };
        format!("{:?}", Route::new(&turn).render(&input))
    }

    #[test]
    fn the_other_parts_of_the_message_are_named_as_routed_on_their_own() {
        let turn = UnderstandingInput::new(
            "register Beta, then add a line",
            "en-GB",
            chrono::NaiveDate::MIN,
        );
        let input = RouteInput {
            label: "Request",
            words: Span::new(3, 5),
            workflows: Vec::new(),
            note: None,
            others: vec![Span::new(0, 1)],
        };
        let rendered = format!("{:?}", Route::new(&turn).render(&input));
        assert!(
            rendered.contains(
                "Other parts of the message are routed on their own, and what they ask for is \
                 not this request's: words 1 to 2, «register Beta,»."
            ),
            "{rendered}"
        );
    }

    #[test]
    fn an_answer_is_routed_seeing_what_the_user_said_before_the_question() {
        let answer = rendered("Answer");
        let before = answer
            .find("Last user message: «I sold 2 hats»")
            .expect(&answer);
        let asked = answer
            .find("Last assistant message: «At what price?»")
            .expect(&answer);
        assert!(before < asked, "{answer}");
        assert!(!rendered("Request").contains("Last user message"));
    }

    #[test]
    fn an_operation_is_offered_in_the_turns_language() {
        use turnframe_core::operation::OperationSpec;
        let spec = OperationSpec::new("trip.set_name")
            .summary("Name the trip.")
            .summary_in("it-IT", "Dà un nome al viaggio.");
        let workflow = WorkflowBrief::new("trip").operation(spec);
        let turn = UnderstandingInput::new(
            "posso chiamare il viaggio?",
            "it-IT",
            chrono::NaiveDate::MIN,
        )
        .with_workflow(workflow.clone());
        let input = RouteInput {
            label: "Request",
            words: Span::new(0, 2),
            workflows: vec![&workflow],
            note: None,
            others: Vec::new(),
        };
        let rendered = format!("{:?}", Route::new(&turn).render(&input));
        assert!(
            rendered.contains("- trip.set_name: Dà un nome al viaggio."),
            "{rendered}"
        );
    }
}
