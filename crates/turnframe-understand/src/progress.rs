//! The steps of an understanding as it runs, for a consumer that wants to show them.
//!
//! A [`Step`] is published to the turn's [`StepSink`] the moment a task decides
//! something. It states what was understood, never what happened: nothing here is a
//! receipt. Each step carries its facts as data and a plain one-line [`Step::describe`];
//! a consumer that wants prose feeds the facts to its own writer.

use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use turnframe_core::ids::{OperationKey, TargetToken, WorkflowKey};
use turnframe_core::understanding::{ActId, NotUnderstoodReason, UnitId, UnitKind};

/// One thing the understanding decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Step {
    /// The message is being read.
    Reading {
        /// How many words it has.
        words: usize,
    },
    /// The message was split into units.
    Segmented {
        /// The model's one-line reading of the message.
        analysis: String,
        /// Each unit: its id, kind and words.
        units: Vec<UnitSummary>,
    },
    /// The coverage check added units the segmentation missed.
    Covered {
        /// The units added.
        added: Vec<UnitSummary>,
    },
    /// A round of the whole-turn check answered.
    CrossChecked {
        /// The round, from 1.
        round: u8,
        /// What it found.
        findings: usize,
    },
    /// A round of the whole-turn check did not run, or gave no answer to use.
    CrossCheckSkipped {
        /// The round, from 1.
        round: u8,
        /// Why, as a failure code.
        code: String,
    },
    /// A unit was routed.
    Routed {
        /// The unit.
        unit: UnitId,
        /// Where it went.
        to: Routing,
    },
    /// An act's record was decided.
    Located {
        /// The act.
        act: ActId,
        /// Its record.
        record: Located,
    },
    /// An act's arguments were read.
    Extracted {
        /// The act.
        act: ActId,
        /// Each argument given, as a person reads it.
        given: Vec<(String, String)>,
        /// The arguments the user did not give.
        not_given: Vec<String>,
    },
    /// An act was verified against the user's words.
    Verified {
        /// The act.
        act: ActId,
        /// Whether everything checked out.
        confirmed: bool,
        /// The verifier's reason.
        reason: String,
        /// The arguments found wanting.
        at_fault: Vec<String>,
    },
    /// An act's values are being read again, with feedback.
    Repairing {
        /// The act.
        act: ActId,
        /// Why.
        because: String,
    },
    /// The domain checked an act.
    Checked {
        /// The act.
        act: ActId,
        /// The argument it refused, and why, when it refused one.
        refused: Option<(String, String)>,
    },
    /// A unit produced nothing to act on.
    NotUnderstood {
        /// The unit.
        unit: UnitId,
        /// Why.
        reason: NotUnderstoodReason,
    },
    /// The understanding is assembled.
    Assembled {
        /// Acts ready for the reducer.
        ready: Vec<ActId>,
        /// Acts that will ask for a value.
        asking: Vec<ActId>,
        /// Acts held back.
        held: Vec<ActId>,
        /// Questions found.
        questions: usize,
    },
}

/// A unit, as a step names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitSummary {
    /// Its id.
    pub id: UnitId,
    /// What it is.
    pub kind: UnitKind,
    /// Its words, verbatim.
    pub text: String,
}

/// Where a unit was routed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Routing {
    /// An operation.
    Operation {
        /// Its key.
        operation: OperationKey,
    },
    /// Starting a workflow.
    Start {
        /// The workflow.
        workflow: WorkflowKey,
    },
    /// Nothing on offer does what it asks.
    Nothing,
}

/// The record an act was aimed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Located {
    /// A record in view, by its label.
    Record {
        /// Its token.
        token: TargetToken,
        /// Its label.
        label: String,
    },
    /// A record this message creates.
    New,
    /// The record an earlier act of this message creates.
    SameTurn {
        /// That act.
        act: ActId,
    },
    /// The record of the card on screen.
    Card,
    /// A record not in view.
    NotListed,
    /// Several records fit.
    Ambiguous,
    /// No record.
    Nothing,
}

impl Step {
    /// One plain line saying what was decided.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Reading { words } => format!("Reading the message ({words} words)."),
            Self::Segmented { analysis, units } => {
                let listed: Vec<String> = units.iter().map(UnitSummary::describe).collect();
                let units = format!("Units: {}.", listed.join("; "));
                if analysis.is_empty() {
                    units
                } else {
                    format!("{analysis} {units}")
                }
            }
            Self::Covered { added } => {
                let listed: Vec<String> = added.iter().map(UnitSummary::describe).collect();
                format!("The check found more: {}.", listed.join("; "))
            }
            Self::CrossChecked { round, findings } => match findings {
                0 => format!("Checking the whole message, round {round}: it all holds."),
                _ => format!(
                    "Checking the whole message, round {round}: {findings} thing(s) to read again."
                ),
            },
            Self::CrossCheckSkipped { round, code } => {
                format!("The whole message was not checked in round {round} ({code}).")
            }
            Self::Routed { unit, to } => match to {
                Routing::Operation { operation } => format!("{unit} asks for {operation}."),
                Routing::Start { workflow } => format!("{unit} starts a new {workflow}."),
                Routing::Nothing => format!("{unit} asks for nothing on offer."),
            },
            Self::Located { act, record } => match record {
                Located::Record { label, .. } => format!("{act} is about {label}."),
                Located::New => format!("{act} creates a new record."),
                Located::SameTurn { act: earlier } => {
                    format!("{act} is about the record {earlier} creates.")
                }
                Located::Card => format!("{act} is about the card on screen."),
                Located::NotListed => format!("{act} names a record not in view."),
                Located::Ambiguous => format!("{act} could be about more than one record."),
                Located::Nothing => format!("{act} is about no record."),
            },
            Self::Extracted {
                act,
                given,
                not_given,
            } => {
                let mut parts: Vec<String> = given
                    .iter()
                    .map(|(name, value)| format!("{name} = {value}"))
                    .collect();
                parts.extend(not_given.iter().map(|name| format!("{name} not given")));
                if parts.is_empty() {
                    format!("{act} takes no arguments.")
                } else {
                    format!("{act}: {}.", parts.join(", "))
                }
            }
            Self::Verified {
                act,
                confirmed,
                reason,
                ..
            } => {
                if *confirmed {
                    format!("{act} checks out: {reason}")
                } else {
                    format!("{act} does not check out: {reason}")
                }
            }
            Self::Repairing { act, because } => format!("Reading {act} again: {because}"),
            Self::Checked { act, refused } => match refused {
                Some((argument, reason)) => format!("{act}: {argument} was refused: {reason}"),
                None => format!("{act} passes the domain's checks."),
            },
            Self::NotUnderstood { unit, reason } => {
                format!("{unit} was not understood: {}.", reason_text(reason))
            }
            Self::Assembled {
                ready,
                asking,
                held,
                questions,
            } => format!(
                "{} ready, {} asking for a value, {} held, {questions} questions.",
                ready.len(),
                asking.len(),
                held.len()
            ),
        }
    }
}

impl UnitSummary {
    fn describe(&self) -> String {
        format!("{} {:?} «{}»", self.id, self.kind, self.text)
    }
}

fn reason_text(reason: &NotUnderstoodReason) -> String {
    match reason {
        NotUnderstoodReason::NoOperation => "nothing on offer does it".to_owned(),
        NotUnderstoodReason::Unclear => "the readings disagreed".to_owned(),
        NotUnderstoodReason::NotRequested => "the user did not ask for it".to_owned(),
        NotUnderstoodReason::TaskFailed { task, code } => format!("{task} failed ({code})"),
        NotUnderstoodReason::KeptUnchanged { constraint } => {
            format!("it would change what {constraint} keeps")
        }
        _ => "no reason recorded".to_owned(),
    }
}

/// Receives the steps of a turn as they happen.
pub trait StepSink: Send + Sync {
    /// Delivers one step. Implementations must not block.
    fn step(&self, step: Step);
}

/// A sink that drops every step.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSteps;

impl StepSink for NoSteps {
    fn step(&self, _step: Step) {}
}

/// A sink that keeps every step, for tests and for a caller that reads them at the end.
#[derive(Debug, Default)]
pub struct RecordedSteps {
    steps: Mutex<Vec<Step>>,
}

impl RecordedSteps {
    /// An empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every step so far, in order.
    #[must_use]
    pub fn steps(&self) -> Vec<Step> {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl StepSink for RecordedSteps {
    fn step(&self, step: Step) {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(step);
    }
}

/// A sink that sends each step down an unbounded channel.
#[derive(Debug, Clone)]
pub struct ChannelSteps(tokio::sync::mpsc::UnboundedSender<Step>);

impl ChannelSteps {
    /// A sink and the receiver it feeds.
    #[must_use]
    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Step>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self(sender), receiver)
    }
}

impl StepSink for ChannelSteps {
    fn step(&self, step: Step) {
        let _ = self.0.send(step);
    }
}

impl<F: Fn(Step) + Send + Sync> StepSink for F {
    fn step(&self, step: Step) {
        self(step);
    }
}
