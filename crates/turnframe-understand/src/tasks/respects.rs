//! `respects`: whether an act changes what a keep-unchanged constraint keeps.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::UnderstandingInput;
use crate::render;
use crate::schema::{object, text};
use crate::words::Span;

const BUILT_IN: &str = include_str!("../../prompts/understand/respects.md");

/// The check of one act against one constraint, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct Respects<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> Respects<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }
}

/// What the check is shown.
#[derive(Debug, Clone)]
pub struct RespectsInput {
    /// The constraint's words.
    pub constraint: Span,
    /// The act: its operation, record and values, each value with the words it came from.
    pub act: String,
}

/// The check's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Respect {
    /// Why, in one sentence.
    pub reason: String,
    /// Whether the act changes what the constraint keeps.
    pub changes: bool,
}

impl ModelTask for Respects<'_> {
    type Input = RespectsInput;
    type Output = Respect;

    fn kind(&self) -> TaskKind {
        TaskKind::Respects
    }

    fn prompt_name(&self) -> &str {
        "understand.respects"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, _input: &RespectsInput) -> Value {
        object(vec![
            ("reason", text("Why, in one sentence.")),
            ("changes", json!({"type": "boolean"})),
        ])
    }

    fn render(&self, input: &RespectsInput) -> Vec<Message> {
        vec![Message::user(render::sections([
            Some(render::message(&self.turn.message)),
            Some(render::unit(
                "Keep as it is",
                &self.turn.message,
                input.constraint,
            )),
            Some(format!("Act: {}", input.act)),
        ]))]
    }

    fn check(&self, _input: &RespectsInput, output: &Respect) -> Result<(), StructuralError> {
        if output.reason.trim().is_empty() {
            return Err(StructuralError::new(
                "empty_reason",
                "`reason` is empty; say in one sentence why",
            ));
        }
        Ok(())
    }

    fn agree(&self, left: &Respect, right: &Respect) -> bool {
        left.changes == right.changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_is_shown_the_words_to_keep_and_the_act() {
        let turn = UnderstandingInput::new(
            "rebook the first leg, don't touch the return",
            "en-GB",
            chrono::NaiveDate::MIN,
        );
        let input = RespectsInput {
            constraint: Span::new(4, 7),
            act: "u1.a1 trip.request_rebooking on Trip 1: leg 1".to_owned(),
        };
        let messages = Respects::new(&turn).render(&input);
        let shown = format!("{messages:?}");
        assert!(
            shown.contains("Keep as it is: words 5 to 8, «don't touch the return»"),
            "{shown}"
        );
        assert!(
            shown.contains("Act: u1.a1 trip.request_rebooking"),
            "{shown}"
        );
    }
}
