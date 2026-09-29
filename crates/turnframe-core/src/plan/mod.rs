//! What an operation declares about the plans it may appear in: its target, whether it
//! mutates, and who may ask for it. The plan itself is an
//! [`Understanding`](crate::understanding::Understanding).

pub mod limits;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::understanding::ActTarget;

/// Which record an operation may be aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetPolicy {
    /// A record that exists: in view, created earlier this turn, or named by the user.
    RequiresExistingCase,
    /// A record the turn already has in view, or the one the card on screen is about.
    RequiresCatalogedCase,
    /// An existing record or a new one.
    AllowsNewCase,
    /// Only a new record: the operation opens one.
    NewCaseOnly,
    /// Only the record of the card on screen.
    ActiveInteractionOnly,
    /// No record at all.
    None,
}

impl TargetPolicy {
    /// Whether an act aimed at `target` is admissible under this policy.
    #[must_use]
    pub const fn permits(self, target: &ActTarget) -> bool {
        match self {
            Self::RequiresExistingCase => matches!(
                target,
                ActTarget::Record { .. }
                    | ActTarget::SameTurn { .. }
                    | ActTarget::NotListed { .. }
                    | ActTarget::Ambiguous { .. }
            ),
            Self::RequiresCatalogedCase => matches!(
                target,
                ActTarget::Record { .. }
                    | ActTarget::SameTurn { .. }
                    | ActTarget::Ambiguous { .. }
                    | ActTarget::Card
            ),
            Self::AllowsNewCase => !matches!(target, ActTarget::Card | ActTarget::Nothing),
            Self::NewCaseOnly => matches!(target, ActTarget::New { .. }),
            Self::ActiveInteractionOnly => matches!(target, ActTarget::Card),
            Self::None => true,
        }
    }
}

/// Who may ask for an operation.
///
/// A card's option may run an operation a model must never propose: the confirm button
/// of a recap. `CardOnly` keeps it off every task's closed set and admits it only from
/// the card that carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActAvailability {
    /// Understanding may route to it, and a card may carry it. The default.
    #[default]
    Proposable,
    /// Only an action a card carries may run it.
    CardOnly,
}

impl ActAvailability {
    /// Whether understanding may route a request to it.
    #[must_use]
    pub const fn is_proposable(self) -> bool {
        matches!(self, Self::Proposable)
    }
}

/// Whether an operation changes a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActMutability {
    /// Reads only.
    ReadOnly,
    /// Changes a record.
    Mutating,
}

/// Which state a question is answered against. The server may override an unsafe basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnswerBasis {
    /// What is committed now.
    CurrentCommittedState,
    /// What the turn proposes but has not committed.
    ProposedState,
    /// What will be committed after this turn's commands.
    CommittedStateAfterTurn,
    /// Domain knowledge independent of any record.
    GeneralDomainKnowledge,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::understanding::{ActId, UnitId};

    #[test]
    fn a_policy_admits_exactly_its_targets() {
        let record = ActTarget::Record { token: "t".into() };
        let same_turn = ActTarget::SameTurn {
            act: ActId::new(UnitId(1), 1),
        };
        let new = ActTarget::New {
            workflow: "w".into(),
        };
        assert!(TargetPolicy::RequiresExistingCase.permits(&record));
        assert!(TargetPolicy::RequiresExistingCase.permits(&same_turn));
        assert!(!TargetPolicy::RequiresExistingCase.permits(&new));
        assert!(
            !TargetPolicy::RequiresCatalogedCase.permits(&ActTarget::NotListed {
                workflow: "w".into(),
                words: None,
            })
        );
        assert!(TargetPolicy::NewCaseOnly.permits(&new));
        assert!(!TargetPolicy::NewCaseOnly.permits(&record));
        assert!(TargetPolicy::ActiveInteractionOnly.permits(&ActTarget::Card));
        assert!(!TargetPolicy::AllowsNewCase.permits(&ActTarget::Card));
    }
}
