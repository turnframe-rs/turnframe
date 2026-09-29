//! `segment`: which units the message holds, what kind each is, and its words.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use turnframe_core::plan::AnswerBasis;
use turnframe_core::understanding::{ConstraintKind, UnitKind};
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::UnderstandingInput;
use crate::render;
use crate::schema::{any_of, array, nullable, object, one_of, span, text, variant};
use crate::tasks::{check_one_of, check_span};
use crate::words::Span;

/// Names a unit's workflow when the message does not say which.
pub const UNKNOWN: &str = "unknown";

const BUILT_IN: &str = include_str!("../../prompts/understand/segment.md");

const BASES: [&str; 4] = [
    "current_committed_state",
    "proposed_state",
    "committed_state_after_turn",
    "general_domain_knowledge",
];

const CONSTRAINTS: [&str; 7] = [
    "do_not_submit",
    "do_not_delete",
    "draft_only",
    "ask_before_applying",
    "apply_only_if",
    "no_external_effects",
    "keep_unchanged",
];

/// The segmentation task, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct Segment<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> Segment<'a> {
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

    /// Whether the message can answer the assistant: it asked for a value, or it said
    /// something last that a value can answer.
    fn answers_the_assistant(&self) -> bool {
        self.turn.expectation.is_some()
            || self
                .turn
                .transcript
                .iter()
                .any(|message| message.speaker == crate::input::Speaker::Assistant)
    }

    fn options(&self) -> Vec<String> {
        self.turn
            .card
            .as_ref()
            .filter(|card| card.accepts_typed_answer)
            .map(|card| card.options.iter().map(|o| o.id.to_string()).collect())
            .unwrap_or_default()
    }

    fn receipts(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.turn.receipts.iter().map(|r| r.key.clone()).collect();
        keys.push(UNKNOWN.to_owned());
        keys
    }
}

/// The units of a message, as the model found them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segmentation {
    /// What the message asks for, in order, in a sentence or two.
    pub analysis: String,
    /// The units, in the order listed.
    pub units: Vec<SegmentedUnit>,
}

/// One unit as the model found it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum SegmentedUnit {
    Request {
        words: Span,
        workflow: String,
    },
    Question {
        words: Span,
        workflow: String,
        basis: AnswerBasis,
        continues_previous: bool,
    },
    Constraint {
        words: Span,
        constraint: ConstraintKind,
    },
    Correction {
        words: Span,
        workflow: String,
        /// The unit number it changes, from 1; `None` for an earlier turn.
        corrects: Option<usize>,
    },
    Cancel {
        words: Span,
        workflow: String,
        /// The unit number it withdraws, from 1; `None` for an earlier turn.
        cancels: Option<usize>,
    },
    CardAnswer {
        words: Span,
        option: String,
    },
    Dispute {
        words: Span,
        receipt: String,
    },
    ProvidesValue {
        words: Span,
    },
    Chitchat {
        words: Span,
    },
}

impl SegmentedUnit {
    /// Its words.
    #[must_use]
    pub const fn words(&self) -> Span {
        match self {
            Self::Request { words, .. }
            | Self::Question { words, .. }
            | Self::Constraint { words, .. }
            | Self::Correction { words, .. }
            | Self::Cancel { words, .. }
            | Self::CardAnswer { words, .. }
            | Self::Dispute { words, .. }
            | Self::ProvidesValue { words }
            | Self::Chitchat { words } => *words,
        }
    }

    /// The same unit over `words`.
    fn at(&self, words: Span) -> Self {
        let mut unit = self.clone();
        match &mut unit {
            Self::Request { words: own, .. }
            | Self::Question { words: own, .. }
            | Self::Constraint { words: own, .. }
            | Self::Correction { words: own, .. }
            | Self::Cancel { words: own, .. }
            | Self::CardAnswer { words: own, .. }
            | Self::Dispute { words: own, .. }
            | Self::ProvidesValue { words: own }
            | Self::Chitchat { words: own } => *own = words,
        }
        unit
    }

    /// Its kind.
    #[must_use]
    pub const fn kind(&self) -> UnitKind {
        match self {
            Self::Request { .. } => UnitKind::Request,
            Self::Question { .. } => UnitKind::Question,
            Self::Constraint { .. } => UnitKind::Constraint,
            Self::Correction { .. } => UnitKind::Correction,
            Self::Cancel { .. } => UnitKind::Cancel,
            Self::CardAnswer { .. } => UnitKind::CardAnswer,
            Self::Dispute { .. } => UnitKind::Dispute,
            Self::ProvidesValue { .. } => UnitKind::ProvidesValue,
            Self::Chitchat { .. } => UnitKind::Chitchat,
        }
    }

    /// The workflow it names, `unknown` included.
    #[must_use]
    pub fn workflow(&self) -> Option<&str> {
        match self {
            Self::Request { workflow, .. }
            | Self::Question { workflow, .. }
            | Self::Correction { workflow, .. }
            | Self::Cancel { workflow, .. } => Some(workflow),
            _ => None,
        }
    }

    /// The unit number it corrects or cancels, when it is about this message.
    #[must_use]
    pub const fn refers_to(&self) -> Option<usize> {
        match self {
            Self::Correction { corrects, .. } => *corrects,
            Self::Cancel { cancels, .. } => *cancels,
            _ => None,
        }
    }
}

impl ModelTask for Segment<'_> {
    type Input = ();
    type Output = Segmentation;

    fn kind(&self) -> TaskKind {
        TaskKind::Segment
    }

    fn prompt_name(&self) -> &str {
        "understand.segment"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, _input: &()) -> Value {
        let workflow = one_of(self.workflows());
        let words = || ("words", span());
        let mut variants = vec![
            variant("request", vec![words(), ("workflow", workflow.clone())]),
            variant(
                "question",
                vec![
                    words(),
                    ("workflow", workflow.clone()),
                    ("basis", one_of(BASES)),
                    ("continues_previous", json!({ "type": "boolean" })),
                ],
            ),
            variant(
                "constraint",
                vec![words(), ("constraint", one_of(CONSTRAINTS))],
            ),
            variant(
                "correction",
                vec![
                    words(),
                    ("workflow", workflow.clone()),
                    ("corrects", nullable(unit_number())),
                ],
            ),
            variant(
                "cancel",
                vec![
                    words(),
                    ("workflow", workflow),
                    ("cancels", nullable(unit_number())),
                ],
            ),
        ];
        let options = self.options();
        if !options.is_empty() {
            variants.push(variant(
                "card_answer",
                vec![words(), ("option", one_of(options))],
            ));
        }
        // Both answer something the assistant said: with nothing said, there is nothing
        // to contest or to give a value for.
        if self.answers_the_assistant() {
            variants.push(variant(
                "dispute",
                vec![words(), ("receipt", one_of(self.receipts()))],
            ));
            variants.push(variant("provides_value", vec![words()]));
        }
        variants.push(variant("chitchat", vec![words()]));
        object(vec![
            (
                "analysis",
                text("What the message asks for, in order, in a sentence or two."),
            ),
            ("units", array(any_of(variants))),
        ])
    }

    fn render(&self, _input: &()) -> Vec<Message> {
        let turn = self.turn;
        vec![Message::user(render::sections([
            Some(render::workflows(turn)),
            render::last_assistant(turn),
            render::card(turn),
            render::expectation(turn),
            render::receipts(turn),
            Some(render::message(&turn.message)),
        ]))]
    }

    fn check(&self, _input: &(), output: &Segmentation) -> Result<(), StructuralError> {
        if output.units.is_empty() {
            return Err(StructuralError::new(
                "no_units",
                "`units` is empty; every message has at least one unit, chitchat included",
            ));
        }
        let (workflows, options, receipts) = (self.workflows(), self.options(), self.receipts());
        let mut card_answers = 0;
        for (position, unit) in output.units.iter().enumerate() {
            let number = position + 1;
            check_span(&format!("unit {number}"), unit.words(), &self.turn.message)?;
            if let Some(workflow) = unit.workflow() {
                check_one_of("workflow", workflow, &workflows)?;
            }
            match unit {
                SegmentedUnit::CardAnswer { option, .. } => {
                    card_answers += 1;
                    check_one_of("option", option, &options)?;
                }
                SegmentedUnit::Dispute { .. } if !self.answers_the_assistant() => {
                    return Err(StructuralError::new(
                        "nothing_said",
                        "a dispute contests what the assistant did or said, and it has said nothing",
                    ));
                }
                SegmentedUnit::Dispute { receipt, .. } => {
                    check_one_of("receipt", receipt, &receipts)?;
                }
                SegmentedUnit::ProvidesValue { .. } if !self.answers_the_assistant() => {
                    return Err(StructuralError::new(
                        "no_expectation",
                        "provides_value needs the assistant to have asked for a value",
                    ));
                }
                _ => {}
            }
        }
        if card_answers > 1 {
            return Err(StructuralError::new(
                "several_card_answers",
                "the card on screen takes one answer; list one card_answer unit at most",
            ));
        }
        check_disjoint(&output.units)
    }

    /// Readings that differ only by a single word at a unit's edge agree: one keeps a word
    /// joining two parts that the other leaves out, and both say the same of the message.
    fn agree(&self, left: &Segmentation, right: &Segmentation) -> bool {
        left.units.len() == right.units.len()
            && left.units.iter().zip(&right.units).all(|(a, b)| {
                let (mine, theirs) = (a.words(), b.words());
                a.at(theirs) == *b
                    && mine.from.abs_diff(theirs.from) <= 1
                    && mine.to.abs_diff(theirs.to) <= 1
            })
    }
}

fn unit_number() -> Value {
    json!({ "type": "integer", "minimum": 1 })
}

fn check_disjoint(units: &[SegmentedUnit]) -> Result<(), StructuralError> {
    let mut spans: Vec<(Span, usize)> = units
        .iter()
        .enumerate()
        .map(|(index, unit)| (unit.words(), index + 1))
        .collect();
    spans.sort();
    for pair in spans.windows(2) {
        let ((first, a), (second, b)) = (pair[0], pair[1]);
        if second.from <= first.to {
            return Err(StructuralError::new(
                "units_overlap",
                format!("units {a} and {b} share words; each word belongs to one unit at most"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_that_differ_by_one_word_at_a_units_edge_agree() {
        let turn = UnderstandingInput::new("x", "en-GB", chrono::NaiveDate::MIN);
        let task = Segment::new(&turn);
        let reading = |units: Vec<SegmentedUnit>| Segmentation {
            analysis: String::new(),
            units,
        };
        let request = |from, to| SegmentedUnit::Request {
            words: Span::new(from, to),
            workflow: "trip".to_owned(),
        };
        let kept = reading(vec![request(12, 24), request(26, 32)]);
        assert!(task.agree(&kept, &reading(vec![request(12, 24), request(25, 32)])));
        assert!(!task.agree(&kept, &reading(vec![request(12, 24), request(24, 32)])));
        assert!(!task.agree(
            &kept,
            &reading(vec![
                request(12, 24),
                SegmentedUnit::Chitchat {
                    words: Span::new(25, 32)
                }
            ])
        ));
    }

    #[test]
    fn the_listed_bases_and_constraints_are_the_ones_that_deserialize() {
        for basis in BASES {
            serde_json::from_value::<AnswerBasis>(json!(basis)).unwrap();
        }
        for constraint in CONSTRAINTS {
            serde_json::from_value::<ConstraintKind>(json!(constraint)).unwrap();
        }
    }
}
