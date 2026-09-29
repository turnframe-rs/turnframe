//! What a turn was understood to say, assembled by code from small model tasks.
//!
//! An [`Understanding`] is the reducer's input: every act with an [`ActId`], its target,
//! its parsed arguments and the words that state them, and the questions, constraints,
//! card answer and disputes of the same message. Nothing in it is model-facing; the
//! tasks that produced it have their own schemas.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::hash::{Digest, HashError, canonical_digest};
use crate::ids::{OperationKey, OptionId, TargetToken, WorkflowKey};
use crate::plan::AnswerBasis;

/// One unit of a message: `u1`. The segmentation's units are numbered from 1 in message order,
/// and units coverage adds follow them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnitId(pub u16);

impl fmt::Display for UnitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "u{}", self.0)
    }
}

/// One act of a unit: `u2.a1`. Command ids, card keys and minted case ids derive from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActId {
    /// The unit that asked for it.
    pub unit: UnitId,
    /// Its position among the unit's acts, from 1.
    pub act: u16,
}

impl ActId {
    /// The `act`-th act of `unit`.
    #[must_use]
    pub const fn new(unit: UnitId, act: u16) -> Self {
        Self { unit, act }
    }
}

impl fmt::Display for ActId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.a{}", self.unit, self.act)
    }
}

/// An act identifier that is not of the form `u<n>.a<m>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not an act identifier")]
pub struct ActIdError(pub String);

impl FromStr for ActId {
    type Err = ActIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || ActIdError(text.to_owned());
        let (unit, act) = text.split_once(".a").ok_or_else(invalid)?;
        let unit = unit.strip_prefix('u').ok_or_else(invalid)?;
        Ok(Self {
            unit: UnitId(unit.parse().map_err(|_| invalid())?),
            act: act.parse().map_err(|_| invalid())?,
        })
    }
}

impl Serialize for ActId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ActId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Which message some words are in: the turn's own, or one of the transcript window shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageRef {
    /// The message being understood.
    Current,
    /// The transcript window's message at this index, oldest first.
    Earlier {
        /// Index into the window.
        index: usize,
    },
}

/// Words of a message: word indices, inclusive, and the byte range they cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WordRange {
    /// First word.
    pub first: usize,
    /// Last word.
    pub last: usize,
    /// Byte offset of the first word's start.
    pub start: usize,
    /// Byte offset just past the last word.
    pub end: usize,
}

/// Words and the message they are in: the evidence for a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Excerpt {
    /// The message.
    pub message: MessageRef,
    /// The words.
    pub words: WordRange,
}

/// What a unit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnitKind {
    /// Asks for something to be done.
    Request,
    /// Asks something.
    Question,
    /// A condition on the whole turn.
    Constraint,
    /// Changes something asked for earlier.
    Correction,
    /// Withdraws something asked for earlier.
    Cancel,
    /// Answers the card on screen.
    CardAnswer,
    /// Contests something the assistant reported doing.
    Dispute,
    /// Gives a value the assistant asked for.
    ProvidesValue,
    /// Greets, thanks, or says nothing that asks for anything.
    Chitchat,
}

/// Which task found a unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoundBy {
    /// The segmentation of the message.
    Segment,
    /// The check for requests the segmentation missed.
    Coverage,
    /// The check of the whole turn.
    CrossCheck,
}

/// One unit of the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    /// Its identifier.
    pub id: UnitId,
    /// What it is.
    pub kind: UnitKind,
    /// Its words in the current message.
    pub words: WordRange,
    /// The workflow it is about, when one was named or implied.
    pub workflow: Option<WorkflowKey>,
    /// Which task found it.
    pub found_by: FoundBy,
}

/// What an act does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActAction {
    /// Applies an operation.
    Apply {
        /// The operation.
        operation: OperationKey,
    },
    /// Starts a new case of a workflow.
    Start {
        /// The workflow.
        workflow: WorkflowKey,
    },
}

/// The record an act applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActTarget {
    /// A record in view, by its token.
    Record {
        /// The token.
        token: TargetToken,
    },
    /// A record this act creates.
    New {
        /// Its workflow.
        workflow: WorkflowKey,
    },
    /// The record an earlier act of the same turn creates.
    SameTurn {
        /// That act.
        act: ActId,
    },
    /// The record of the card on screen.
    Card,
    /// A record the user named that is not in view; the application looks it up.
    NotListed {
        /// Its workflow.
        workflow: WorkflowKey,
        /// The words naming it, when the user used any.
        words: Option<WordRange>,
    },
    /// Several records fit; the user is asked which.
    Ambiguous {
        /// The records that fit.
        candidates: Vec<TargetToken>,
    },
    /// The operation applies to no record.
    Nothing,
}

/// A record an argument names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordValue {
    /// A record in view.
    Record {
        /// Its token.
        token: TargetToken,
    },
    /// The record an earlier act of the same turn creates.
    SameTurn {
        /// That act.
        act: ActId,
    },
    /// A record the user named that is not in view; the runtime looks it up.
    Named {
        /// The workflow it belongs to.
        workflow: WorkflowKey,
        /// The words that name it.
        named: String,
    },
}

/// An argument's value, parsed and evaluated by code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ArgumentValue {
    /// A value in the operation's own schema: text, a date, an amount, an enum value.
    Json(serde_json::Value),
    /// A record, resolved by the runtime into the operation's representation.
    Record(RecordValue),
}

/// An argument and the words that state it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnderstoodArgument {
    /// The value.
    pub value: ArgumentValue,
    /// The words it comes from; absent for a value carried over from an earlier turn.
    pub excerpt: Option<Excerpt>,
}

/// Whether an act may proceed to the reducer as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActStatus {
    /// Complete and verified.
    Ready,
    /// Missing, unstated or rejected arguments; the user is asked, nothing is written.
    NeedsValue {
        /// The arguments to ask for.
        arguments: Vec<String>,
        /// The domain's explanation, when it rejected a value.
        reason: Option<String>,
    },
    /// Another unit aimed at the same record was not understood.
    Held {
        /// That unit.
        because: UnitId,
    },
}

/// One act the turn asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnderstoodAct {
    /// Its identifier.
    pub id: ActId,
    /// What it does.
    pub action: ActAction,
    /// What it applies to.
    pub target: ActTarget,
    /// Its arguments, by name.
    pub arguments: BTreeMap<String, UnderstoodArgument>,
    /// The words of the unit that asked for it.
    pub words: WordRange,
    /// Acts of the same turn it needs first.
    pub depends_on: Vec<ActId>,
    /// Whether it may proceed.
    pub status: ActStatus,
}

impl UnderstoodAct {
    /// The operation it applies, when it applies one.
    #[must_use]
    pub const fn operation(&self) -> Option<&OperationKey> {
        match &self.action {
            ActAction::Apply { operation } => Some(operation),
            ActAction::Start { .. } => None,
        }
    }

    /// Snake-case name of what it does: `apply_operation` or `start_workflow`.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self.action {
            ActAction::Apply { .. } => "apply_operation",
            ActAction::Start { .. } => "start_workflow",
        }
    }
}

/// An act a later unit of the same message replaced or withdrew.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Superseded {
    /// The act.
    pub act: ActId,
    /// What it asked for, kept because the act itself is gone.
    pub action: ActAction,
    /// The correction or cancellation.
    pub by: UnitId,
}

/// A question the user asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnderstoodQuestion {
    /// Its unit.
    pub unit: UnitId,
    /// Its words.
    pub words: WordRange,
    /// The workflow it is about.
    pub workflow: Option<WorkflowKey>,
    /// The record it is about.
    pub record: Option<TargetToken>,
    /// The declared subjects it asks about.
    pub subjects: Vec<String>,
    /// Which state answers it.
    pub basis: AnswerBasis,
    /// What kind of thing it asks.
    #[serde(default)]
    pub topic: QuestionTopic,
    /// Whether it follows up the assistant's last message.
    pub continues_previous: bool,
}

/// What kind of thing a question asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum QuestionTopic {
    /// What a record holds, or where it stands.
    #[default]
    RecordState,
    /// Which values a field accepts.
    AcceptedValues,
    /// What the user can do here, or whether something can be done.
    Capabilities,
    /// Anything else the domain knows.
    Knowledge,
}

/// A condition the user placed on the whole turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConstraintKind {
    /// Submit nothing.
    DoNotSubmit,
    /// Delete nothing.
    DoNotDelete,
    /// Keep everything a draft.
    DraftOnly,
    /// Ask before applying anything.
    AskBeforeApplying,
    /// Apply only if a condition holds; its words are the condition.
    ApplyOnlyIf,
    /// No external effects.
    NoExternalEffects,
    /// Leave what its words name as it is: an act that would change it runs nothing.
    KeepUnchanged,
}

/// A constraint and its words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnConstraint {
    /// Its unit.
    pub unit: UnitId,
    /// Which constraint.
    pub kind: ConstraintKind,
    /// Its words.
    pub words: WordRange,
}

/// A typed answer to the card on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardAnswer {
    /// Its unit.
    pub unit: UnitId,
    /// The option chosen.
    pub option: OptionId,
    /// Its words.
    pub words: WordRange,
}

/// Something the assistant reported that the user contests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispute {
    /// Its unit.
    pub unit: UnitId,
    /// Its words.
    pub words: WordRange,
    /// The receipt contested, by the key it was shown under.
    pub receipt: Option<String>,
}

/// Why a unit was not understood.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NotUnderstoodReason {
    /// No operation on offer does what it asks.
    NoOperation,
    /// Its answers disagreed and the profile asks instead of choosing.
    Unclear,
    /// The verifier found the act was not asked for.
    NotRequested,
    /// It would change what a keep-unchanged constraint keeps.
    KeptUnchanged {
        /// The constraint's unit.
        constraint: UnitId,
    },
    /// A task failed after its repairs and escalation.
    TaskFailed {
        /// The task.
        task: String,
        /// Its failure code.
        code: String,
    },
}

/// A unit that produced nothing to act on, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotUnderstood {
    /// The unit.
    pub unit: UnitId,
    /// Its words.
    pub words: WordRange,
    /// Why.
    pub reason: NotUnderstoodReason,
}

/// Why a whole message could not be read, so no act of it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Unreadable {
    /// The segmentation failed.
    Segmentation {
        /// Its failure code.
        code: String,
    },
    /// A constraint was found by coverage, so its kind is unknown.
    LostConstraint,
}

/// Everything a turn was understood to say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Understanding {
    /// The units, in message order.
    pub units: Vec<Unit>,
    /// The acts, in message order.
    pub acts: Vec<UnderstoodAct>,
    /// Acts replaced or withdrawn by a later unit.
    pub superseded: Vec<Superseded>,
    /// The questions.
    pub questions: Vec<UnderstoodQuestion>,
    /// The constraints.
    pub constraints: Vec<TurnConstraint>,
    /// The typed answer to the card on screen.
    pub card_answer: Option<CardAnswer>,
    /// The disputes.
    pub disputes: Vec<Dispute>,
    /// Units that produced nothing to act on.
    pub not_understood: Vec<NotUnderstood>,
    /// Set when the message could not be read at all; then nothing else is.
    pub unreadable: Option<Unreadable>,
}

impl Understanding {
    /// A message that could not be read: no acts, no questions, only the reason.
    #[must_use]
    pub fn unreadable(reason: Unreadable) -> Self {
        Self {
            unreadable: Some(reason),
            ..Self::default()
        }
    }

    /// The act with this identifier.
    #[must_use]
    pub fn act(&self, id: ActId) -> Option<&UnderstoodAct> {
        self.acts.iter().find(|act| act.id == id)
    }

    /// Canonical digest, so a replay can tell the same understanding from another.
    ///
    /// # Errors
    ///
    /// [`HashError`] when a value cannot be written as canonical JSON.
    pub fn hash(&self) -> Result<Digest, HashError> {
        canonical_digest(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn act_identifiers_read_and_parse_as_unit_and_position() {
        let id = ActId::new(UnitId(2), 1);
        assert_eq!(id.to_string(), "u2.a1");
        assert_eq!("u2.a1".parse::<ActId>().unwrap(), id);
        assert!("u2".parse::<ActId>().is_err());
        assert!("x2.a1".parse::<ActId>().is_err());
        let json = serde_json::to_value(id).unwrap();
        assert_eq!(json, serde_json::json!("u2.a1"));
        assert_eq!(serde_json::from_value::<ActId>(json).unwrap(), id);
    }
}
