//! Size limits on what a turn may be understood to ask.
//!
//! An understanding over a limit is refused whole, with the count and the bound, never
//! trimmed to fit. Every limit is off by default: how much one message may ask for is
//! the deployment's call.

use serde::{Deserialize, Serialize};

use crate::understanding::Understanding;

/// Which limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanLimitKind {
    /// Too many acts.
    Acts,
    /// Too many questions.
    Questions,
    /// Too many constraints.
    Constraints,
}

/// An understanding exceeded a limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("plan exceeds {kind:?} limit: {actual} > {limit}")]
pub struct PlanLimitError {
    /// Which limit.
    pub kind: PlanLimitKind,
    /// The configured limit.
    pub limit: usize,
    /// The observed count.
    pub actual: usize,
}

/// Limits applied to an [`Understanding`]; `None` is unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PlanLimits {
    /// Maximum number of acts.
    pub max_acts: Option<usize>,
    /// Maximum number of questions.
    pub max_questions: Option<usize>,
    /// Maximum number of constraints.
    pub max_constraints: Option<usize>,
}

impl PlanLimits {
    /// No limits at all, which is what ships.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            max_acts: None,
            max_questions: None,
            max_constraints: None,
        }
    }

    /// Returns a copy with another act limit.
    #[must_use]
    pub const fn with_max_acts(mut self, max_acts: Option<usize>) -> Self {
        self.max_acts = max_acts;
        self
    }

    /// Returns a copy with another question limit.
    #[must_use]
    pub const fn with_max_questions(mut self, max_questions: Option<usize>) -> Self {
        self.max_questions = max_questions;
        self
    }

    /// Returns a copy with another constraint limit.
    #[must_use]
    pub const fn with_max_constraints(mut self, max_constraints: Option<usize>) -> Self {
        self.max_constraints = max_constraints;
        self
    }

    /// Checks `understanding` against the limits.
    ///
    /// # Errors
    ///
    /// The first limit exceeded.
    pub fn enforce(&self, understanding: &Understanding) -> Result<(), PlanLimitError> {
        check(PlanLimitKind::Acts, self.max_acts, understanding.acts.len())?;
        check(
            PlanLimitKind::Questions,
            self.max_questions,
            understanding.questions.len(),
        )?;
        check(
            PlanLimitKind::Constraints,
            self.max_constraints,
            understanding.constraints.len(),
        )
    }
}

fn check(kind: PlanLimitKind, limit: Option<usize>, actual: usize) -> Result<(), PlanLimitError> {
    match limit {
        Some(limit) if actual > limit => Err(PlanLimitError {
            kind,
            limit,
            actual,
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::understanding::{
        ActAction, ActId, ActStatus, ActTarget, UnderstoodAct, UnitId, WordRange,
    };

    fn act(unit: u16) -> UnderstoodAct {
        UnderstoodAct {
            id: ActId::new(UnitId(unit), 1),
            action: ActAction::Start {
                workflow: "w".into(),
            },
            target: ActTarget::New {
                workflow: "w".into(),
            },
            arguments: std::collections::BTreeMap::new(),
            words: WordRange {
                first: 0,
                last: 0,
                start: 0,
                end: 1,
            },
            depends_on: Vec::new(),
            status: ActStatus::Ready,
        }
    }

    #[test]
    fn limits_are_enforced_with_the_count_and_the_bound() {
        let limits = PlanLimits::default().with_max_acts(Some(1));
        let understanding = Understanding {
            acts: vec![act(1), act(2)],
            ..Understanding::default()
        };
        let error = limits.enforce(&understanding).unwrap_err();
        assert_eq!(error.kind, PlanLimitKind::Acts);
        assert_eq!((error.limit, error.actual), (1, 2));
        assert!(PlanLimits::default().enforce(&understanding).is_ok());
    }
}
