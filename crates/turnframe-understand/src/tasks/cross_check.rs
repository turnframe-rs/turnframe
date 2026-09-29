//! `cross_check`: whether what was understood of a message says what the message says.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::UnderstandingInput;
use crate::render;
use crate::schema::{any_of, array, object, one_of, span, variant};
use crate::tasks::{check_one_of, check_span};
use crate::words::Span;

const BUILT_IN: &str = include_str!("../../prompts/understand/cross_check.md");

/// The whole-turn check, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct CrossCheck<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> CrossCheck<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }
}

/// One act as the check is shown it.
#[derive(Debug, Clone)]
pub struct ShownAct {
    /// Its id, `u1.a1`.
    pub id: String,
    /// Its operation, record and values, each value with the words it came from.
    pub line: String,
    /// Its argument names.
    pub arguments: Vec<String>,
}

/// What the check is shown.
#[derive(Debug, Clone, Default)]
pub struct CrossCheckInput {
    /// Every act understood.
    pub acts: Vec<ShownAct>,
    /// Every question, by its words.
    pub questions: Vec<String>,
    /// Every constraint, by its words.
    pub constraints: Vec<String>,
    /// Words read as nothing to act on.
    pub unread: Vec<String>,
    /// Words already read as an act, a question, a constraint, small talk or a value.
    pub held: Vec<Span>,
}

/// One place the reading does not say what the message says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    /// Words asking for something nothing holds.
    Missing {
        /// The words.
        words: Span,
    },
    /// A value the message does not give, and the words it should come from.
    WrongValue {
        /// The act.
        act: String,
        /// Its argument.
        argument: String,
        /// The words the value should come from.
        words: Span,
    },
    /// A record the message does not mean, and the words naming the one it does.
    WrongRecord {
        /// The act.
        act: String,
        /// The words naming the record meant.
        words: Span,
    },
    /// An act the message does not ask for.
    NotAsked {
        /// The act.
        act: String,
    },
}

/// The check's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossChecked {
    /// Empty when the reading says what the message says.
    pub findings: Vec<Finding>,
}

impl ModelTask for CrossCheck<'_> {
    type Input = CrossCheckInput;
    type Output = CrossChecked;

    fn kind(&self) -> TaskKind {
        TaskKind::CrossCheck
    }

    fn prompt_name(&self) -> &str {
        "understand.cross_check"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, input: &CrossCheckInput) -> Value {
        let acts: Vec<String> = input.acts.iter().map(|act| act.id.clone()).collect();
        let mut arguments: Vec<String> = input
            .acts
            .iter()
            .flat_map(|act| act.arguments.clone())
            .collect();
        arguments.sort();
        arguments.dedup();
        let mut kinds = vec![variant("missing", vec![("words", span())])];
        if !acts.is_empty() {
            if !arguments.is_empty() {
                kinds.push(variant(
                    "wrong_value",
                    vec![
                        ("act", one_of(acts.clone())),
                        ("argument", one_of(arguments)),
                        ("words", span()),
                    ],
                ));
            }
            kinds.push(variant(
                "wrong_record",
                vec![("act", one_of(acts.clone())), ("words", span())],
            ));
            kinds.push(variant("not_asked", vec![("act", one_of(acts))]));
        }
        object(vec![("findings", array(any_of(kinds)))])
    }

    fn render(&self, input: &CrossCheckInput) -> Vec<Message> {
        let list = |title: &str, lines: &[String]| {
            (!lines.is_empty()).then(|| {
                let mut out = format!("{title}:");
                for line in lines {
                    let _ = write!(out, "\n- {line}");
                }
                out
            })
        };
        let acts: Vec<String> = input.acts.iter().map(|act| act.line.clone()).collect();
        vec![Message::user(render::sections([
            render::last_assistant(self.turn),
            Some(render::message(&self.turn.message)),
            list("Acts understood", &acts).or_else(|| Some("Acts understood: none".to_owned())),
            list("Questions", &input.questions),
            list("Constraints", &input.constraints),
            list("Read as nothing to act on", &input.unread),
        ]))]
    }

    fn check(&self, input: &CrossCheckInput, output: &CrossChecked) -> Result<(), StructuralError> {
        let acts: Vec<String> = input.acts.iter().map(|act| act.id.clone()).collect();
        let words = &self.turn.message;
        for (position, finding) in output.findings.iter().enumerate() {
            let what = format!("finding {}", position + 1);
            match finding {
                Finding::Missing { words: span } => {
                    check_span(&what, *span, words)?;
                    let read = input
                        .held
                        .iter()
                        .any(|held| held.from <= span.to && span.from <= held.to);
                    if read {
                        return Err(StructuralError::new(
                            "words_already_read",
                            format!(
                                "{what}: those words are already read; missing words are \
                                 words nothing holds"
                            ),
                        ));
                    }
                }
                Finding::WrongValue {
                    act,
                    argument,
                    words: span,
                } => {
                    check_one_of("act", act, &acts)?;
                    check_span(&what, *span, words)?;
                    let known = input
                        .acts
                        .iter()
                        .find(|shown| &shown.id == act)
                        .is_some_and(|shown| shown.arguments.contains(argument));
                    if !known {
                        return Err(StructuralError::new(
                            "not_an_argument_of_the_act",
                            format!("{what}: {act} has no argument `{argument}`"),
                        ));
                    }
                }
                Finding::WrongRecord { act, words: span } => {
                    check_one_of("act", act, &acts)?;
                    check_span(&what, *span, words)?;
                }
                Finding::NotAsked { act } => check_one_of("act", act, &acts)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::UnderstandingInput;

    fn input() -> CrossCheckInput {
        CrossCheckInput {
            acts: vec![ShownAct {
                id: "u1.a1".to_owned(),
                line: "u1.a1 trip.set_name on Trip 1: value «Lisbon» (words 2 to 2)".to_owned(),
                arguments: vec!["value".to_owned()],
            }],
            questions: Vec::new(),
            constraints: Vec::new(),
            unread: Vec::new(),
            held: vec![Span::new(1, 1)],
        }
    }

    fn turn() -> UnderstandingInput {
        // [1]name [2]Lisbon [3]and [4]meals [5]too
        UnderstandingInput::new("name Lisbon and meals too", "en-GB", chrono::NaiveDate::MIN)
    }

    #[test]
    fn a_finding_must_name_an_act_and_an_argument_it_has() {
        let turn = turn();
        let task = CrossCheck::new(&turn);
        let wrong = CrossChecked {
            findings: vec![Finding::WrongValue {
                act: "u1.a1".to_owned(),
                argument: "due".to_owned(),
                words: Span::new(0, 0),
            }],
        };
        assert_eq!(
            task.check(&input(), &wrong).unwrap_err().code,
            "not_an_argument_of_the_act"
        );
        let unknown = CrossChecked {
            findings: vec![Finding::NotAsked {
                act: "u9.a1".to_owned(),
            }],
        };
        assert_eq!(
            task.check(&input(), &unknown).unwrap_err().code,
            "not_in_set"
        );
    }

    #[test]
    fn missing_words_lie_outside_what_was_read() {
        let turn = turn();
        let task = CrossCheck::new(&turn);
        let read = CrossChecked {
            findings: vec![Finding::Missing {
                words: Span::new(1, 2),
            }],
        };
        assert_eq!(
            task.check(&input(), &read).unwrap_err().code,
            "words_already_read"
        );
        let fresh = CrossChecked {
            findings: vec![Finding::Missing {
                words: Span::new(3, 4),
            }],
        };
        assert!(task.check(&input(), &fresh).is_ok());
    }

    #[test]
    fn an_empty_answer_is_an_answer() {
        let turn = turn();
        let task = CrossCheck::new(&turn);
        assert!(
            task.check(
                &input(),
                &CrossChecked {
                    findings: Vec::new()
                }
            )
            .is_ok()
        );
    }
}
