//! `coverage`: which requests, questions or constraints the segmentation missed.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_core::understanding::UnitKind;
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::UnderstandingInput;
use crate::render;
use crate::schema::{array, object, one_of, span};
use crate::tasks::segment::UNKNOWN;
use crate::tasks::{check_one_of, check_span};
use crate::words::Span;

const BUILT_IN: &str = include_str!("../../prompts/understand/coverage.md");

/// The coverage task, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct Coverage<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> Coverage<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }

    fn workflows(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .turn
            .workflows
            .iter()
            .map(|workflow| workflow.key.to_string())
            .collect();
        names.push(UNKNOWN.to_owned());
        names
    }
}

/// The units already found, which coverage looks past.
#[derive(Debug, Clone)]
pub struct Found {
    /// Kind and words of each unit, in message order.
    pub units: Vec<(UnitKind, Span)>,
}

/// What the segmentation missed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Missed {
    /// The missed units.
    pub missed: Vec<MissedUnit>,
}

/// One missed unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissedUnit {
    /// What it is.
    pub kind: MissedKind,
    /// Its words.
    pub words: Span,
    /// The workflow it is about, or `unknown`.
    pub workflow: String,
}

/// The kinds a missed unit may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum MissedKind {
    Request,
    Question,
    Constraint,
    Correction,
    Cancel,
}

impl From<MissedKind> for UnitKind {
    fn from(kind: MissedKind) -> Self {
        match kind {
            MissedKind::Request => Self::Request,
            MissedKind::Question => Self::Question,
            MissedKind::Constraint => Self::Constraint,
            MissedKind::Correction => Self::Correction,
            MissedKind::Cancel => Self::Cancel,
        }
    }
}

pub(crate) fn kind_name(kind: UnitKind) -> &'static str {
    match kind {
        UnitKind::Request => "request",
        UnitKind::Question => "question",
        UnitKind::Constraint => "constraint",
        UnitKind::Correction => "correction",
        UnitKind::Cancel => "cancel",
        UnitKind::CardAnswer => "card_answer",
        UnitKind::Dispute => "dispute",
        UnitKind::ProvidesValue => "provides_value",
        _ => "chitchat",
    }
}

impl ModelTask for Coverage<'_> {
    type Input = Found;
    type Output = Missed;

    fn kind(&self) -> TaskKind {
        TaskKind::Coverage
    }

    fn prompt_name(&self) -> &str {
        "understand.coverage"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, _input: &Found) -> Value {
        let kinds = one_of(["request", "question", "constraint", "correction", "cancel"]);
        object(vec![(
            "missed",
            array(object(vec![
                ("kind", kinds),
                ("words", span()),
                ("workflow", one_of(self.workflows())),
            ])),
        )])
    }

    fn render(&self, input: &Found) -> Vec<Message> {
        let words = &self.turn.message;
        let mut found = String::from("Units found:");
        for (position, (kind, span)) in input.units.iter().enumerate() {
            let text = words.slice(*span).unwrap_or_default();
            let _ = write!(
                found,
                "\n{}. {}: words {} to {}, {}",
                position + 1,
                kind_name(*kind),
                span.shown().0,
                span.shown().1,
                render::quoted(text)
            );
        }
        vec![Message::user(render::sections([
            Some(render::workflows(self.turn)),
            Some(render::message(words)),
            Some(found),
        ]))]
    }

    fn check(&self, _input: &Found, output: &Missed) -> Result<(), StructuralError> {
        let workflows = self.workflows();
        for (position, unit) in output.missed.iter().enumerate() {
            check_span(
                &format!("missed unit {}", position + 1),
                unit.words,
                &self.turn.message,
            )?;
            check_one_of("workflow", &unit.workflow, &workflows)?;
        }
        Ok(())
    }
}
