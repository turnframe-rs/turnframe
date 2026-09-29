//! What an item expects of each understanding task, beside what it expects of the turn.
//!
//! A turn-level expectation says what happened; these say which task got it right or
//! wrong: how the message split into units, which operation and record each request
//! reached, and which arguments were given or not. They are parsed and validated with
//! the rest of the item, and scored against the task records a turn leaves behind.
//!
//! ```toml
//! [[expect.understanding.units]]
//! kind = "request"
//! words = "the subject needs changing"
//! operation = "trip.set_name"
//! record = "trip-1"
//! arguments = { value = { not_given = true } }
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use turnframe_core::understanding::{
    ActAction, ActTarget, ArgumentValue, UnderstoodAct, Unit, UnitKind as CoreUnitKind,
};
use turnframe_runtime::resume::CARD_UNIT;

use crate::corpus::CorpusError;
use crate::observation::Observation;

/// The units a message splits into, in message order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct UnderstandingExpectation {
    /// Every unit the message carries, in the order the user wrote them.
    #[serde(default)]
    pub units: Vec<UnitExpectation>,
}

/// One unit of a message, and what the tasks after segmentation must make of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct UnitExpectation {
    /// What kind of thing the user did in these words.
    pub kind: UnitKind,
    /// The user's words the unit covers, exactly as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<String>,
    /// The operation a request reaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// The record it aims at: a seeded case id, or `new`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// Expected arguments, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub arguments: BTreeMap<String, ArgumentExpectation>,
}

/// The kinds of unit segmentation distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnitKind {
    /// Something to do.
    Request,
    /// Something asked.
    Question,
    /// A condition on the whole turn.
    Constraint,
    /// A change to something said earlier in the same message.
    Correction,
    /// Withdrawing something.
    Cancel,
    /// An answer to the card on screen.
    CardAnswer,
    /// Contesting what the assistant did or said.
    Dispute,
    /// The value the assistant asked for.
    ProvidesValue,
    /// Nothing to act on.
    Chitchat,
}

impl UnitKind {
    /// Whether units of this kind are routed to an operation.
    #[must_use]
    pub const fn reaches_an_operation(self) -> bool {
        matches!(
            self,
            Self::Request | Self::Correction | Self::Cancel | Self::ProvidesValue
        )
    }
}

/// What one argument must come out as: a value, or explicitly not given.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ArgumentExpectation {
    /// The value it must equal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<serde_json::Value>,
    /// The user did not give it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_given: bool,
}

impl UnderstandingExpectation {
    /// Checks each unit against the turn text it is about.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] when a unit quotes words the turn does not contain,
    /// names an operation its kind cannot reach, or states an argument both ways.
    pub fn validate(&self, text: Option<&str>) -> Result<(), CorpusError> {
        for unit in &self.units {
            unit.validate(text)?;
        }
        Ok(())
    }
}

impl UnitExpectation {
    fn validate(&self, text: Option<&str>) -> Result<(), CorpusError> {
        const FIELD: &str = "expect.understanding.units";
        let invalid = |reason: &str| CorpusError::Invalid {
            field: FIELD.to_owned(),
            reason: reason.to_owned(),
        };
        if let Some(words) = &self.words {
            if words.trim().is_empty() {
                return Err(invalid("`words` is empty"));
            }
            if !text.is_some_and(|text| text.contains(words.as_str())) {
                return Err(invalid(&format!(
                    "`words` «{words}» is not in the turn's text"
                )));
            }
        }
        let routed = self.operation.is_some() || self.record.is_some();
        if (routed || !self.arguments.is_empty()) && !self.kind.reaches_an_operation() {
            return Err(invalid(&format!(
                "a {:?} unit reaches no operation, so it has no operation, record or arguments",
                self.kind
            )));
        }
        if !self.arguments.is_empty() && self.operation.is_none() {
            return Err(invalid("arguments belong to an operation; name it"));
        }
        for (name, argument) in &self.arguments {
            if argument.not_given == argument.equals.is_some() {
                return Err(invalid(&format!(
                    "argument `{name}` needs exactly one of `equals` and `not_given`"
                )));
            }
        }
        Ok(())
    }
}

/// How each understanding task did on one sample: `None` where the item states nothing
/// that task decides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskScores {
    /// The message split into the units expected, of the kinds expected.
    pub segment: Option<bool>,
    /// Each request reached the operation expected.
    pub route: Option<bool>,
    /// Each request aimed at the record expected.
    pub locate: Option<bool>,
    /// Each argument came out as expected: that value, or not given.
    pub extract: Option<bool>,
}

impl TaskScores {
    /// Each task's score, by the task's name.
    #[must_use]
    pub const fn by_task(&self) -> [(&'static str, Option<bool>); 4] {
        [
            ("segment", self.segment),
            ("route", self.route),
            ("locate", self.locate),
            ("extract", self.extract),
        ]
    }
}

/// Words compared as a reader compares them: trimmed, without the punctuation around.
fn normalized(words: &str) -> String {
    words
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '!' | '?'))
        .to_lowercase()
}

impl UnderstandingExpectation {
    /// Scores the understanding a turn recorded against what this item expects of
    /// each task. A unit is paired with the understood unit quoting the same words,
    /// else with the one at the same position.
    #[must_use]
    pub fn score(&self, text: &str, observed: &Observation) -> TaskScores {
        if self.units.is_empty() {
            return TaskScores::default();
        }
        let failed = |stated: bool| stated.then_some(false);
        let stated = |test: fn(&UnitExpectation) -> bool| self.units.iter().any(test);
        let Some(understood) = observed.understanding.as_ref() else {
            return TaskScores {
                segment: Some(false),
                route: failed(stated(|unit| unit.operation.is_some())),
                locate: failed(stated(|unit| unit.record.is_some())),
                extract: failed(stated(|unit| !unit.arguments.is_empty())),
            };
        };
        let mut units: Vec<&Unit> = understood
            .units
            .iter()
            .filter(|unit| unit.id != CARD_UNIT)
            .collect();
        units.sort_by_key(|unit| unit.words.start);
        let said = |unit: &Unit| {
            normalized(
                text.get(unit.words.start..unit.words.end)
                    .unwrap_or_default(),
            )
        };
        let kind_of = |kind: UnitKind| serde_json::to_value(kind).ok();
        let same_kind = |expected: UnitKind, found: CoreUnitKind| {
            kind_of(expected).is_some() && kind_of(expected) == serde_json::to_value(found).ok()
        };
        let segment = units.len() == self.units.len()
            && self.units.iter().zip(&units).all(|(expected, found)| {
                same_kind(expected.kind, found.kind)
                    && expected
                        .words
                        .as_deref()
                        .is_none_or(|words| normalized(words) == said(found))
            });
        let paired = |at: usize, expected: &UnitExpectation| -> Option<&Unit> {
            match expected.words.as_deref() {
                Some(words) => units
                    .iter()
                    .copied()
                    .find(|found| said(found) == normalized(words)),
                None if units.len() == self.units.len() => units.get(at).copied(),
                None => None,
            }
        };
        let message_acts: Vec<&UnderstoodAct> = understood
            .acts
            .iter()
            .filter(|act| act.id.unit != CARD_UNIT)
            .collect();
        let (mut route, mut locate, mut extract) = (None, None, None);
        let record = |score: &mut Option<bool>, pass: bool| {
            *score = Some(score.unwrap_or(true) && pass);
        };
        for (at, expected) in self.units.iter().enumerate() {
            let does = |act: &UnderstoodAct, operation: &str| match &act.action {
                ActAction::Apply { operation: found } => found.as_str() == operation,
                ActAction::Start { workflow } => operation == format!("start:{workflow}"),
            };
            // A unit may ask for several things: the act scored is the one doing the
            // expected operation, when there is one.
            let act = paired(at, expected).and_then(|unit| {
                let mut of_unit = message_acts
                    .iter()
                    .copied()
                    .filter(|act| act.id.unit == unit.id);
                let first = of_unit.clone().next();
                expected
                    .operation
                    .as_deref()
                    .and_then(|operation| of_unit.find(|act| does(act, operation)))
                    .or(first)
            });
            if let Some(operation) = &expected.operation {
                let reached = act.is_some_and(|act| does(act, operation));
                record(&mut route, reached);
            }
            if let Some(target) = &expected.record {
                let aimed = act.is_some_and(|act| {
                    if target == "new" {
                        return matches!(act.target, ActTarget::New { .. });
                    }
                    let at = message_acts.iter().position(|other| other.id == act.id);
                    observed.target_resolutions.iter().any(|resolution| {
                        Some(resolution.act_index) == at
                            && resolution
                                .case_id
                                .as_ref()
                                .is_some_and(|id| id.as_str() == target)
                    })
                });
                record(&mut locate, aimed);
            }
            if !expected.arguments.is_empty() {
                let given = act.is_some_and(|act| {
                    expected.arguments.iter().all(|(name, argument)| {
                        let found = act.arguments.get(name).map(|given| &given.value);
                        match (&argument.equals, found) {
                            (None, found) => found.is_none(),
                            (Some(value), Some(ArgumentValue::Json(found))) => found == value,
                            (Some(_), _) => false,
                        }
                    })
                });
                record(&mut extract, given);
            }
        }
        TaskScores {
            segment: Some(segment),
            route,
            locate,
            extract,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(toml: &str) -> UnitExpectation {
        toml::from_str(toml).expect("the unit parses")
    }

    #[test]
    fn a_request_with_a_missing_value_validates() {
        let unit = unit(
            r#"
            kind = "request"
            words = "the subject needs changing"
            operation = "trip.set_name"
            arguments = { value = { not_given = true } }
            "#,
        );
        assert!(unit.validate(Some("the subject needs changing")).is_ok());
    }

    #[test]
    fn words_the_turn_does_not_contain_are_refused() {
        let unit = unit(
            r#"kind = "question"
words = "is it valid""#,
        );
        assert!(unit.validate(Some("that is wrong")).is_err());
    }

    #[test]
    fn a_dispute_names_no_operation() {
        let unit = unit(
            r#"kind = "dispute"
operation = "trip.set_name""#,
        );
        assert!(unit.validate(None).is_err());
    }

    #[test]
    fn an_argument_is_given_or_not_never_both() {
        let unit = unit(
            r#"
            kind = "request"
            operation = "trip.set_name"
            arguments = { value = { equals = "x", not_given = true } }
            "#,
        );
        assert!(unit.validate(None).is_err());
    }
}
