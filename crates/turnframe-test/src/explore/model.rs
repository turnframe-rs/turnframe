//! What a domain must tell the explorer about its own transitions.

use turnframe_core::error::DomainRejection;
use turnframe_core::flow::WorkflowDefinition;

/// The result of applying one candidate command to one state (spec §8.5).
///
/// A transition is either applied — producing the next state and the events the
/// executor would commit — or refused by the domain. A refusal carries the
/// [`DomainRejection`] the real executor would return, so the explorer can check
/// it against [`WorkflowDefinition::validate_command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulatedTransition<S, E> {
    /// The command was applied.
    Applied {
        /// State after the command, or `None` when the case ceased to exist.
        state: Option<S>,
        /// Events the executor would commit.
        events: Vec<E>,
    },
    /// The domain refused the command; the input state is unchanged.
    Rejected(DomainRejection),
}

impl<S, E> SimulatedTransition<S, E> {
    /// A transition that produced a new state.
    #[must_use]
    pub fn applied(state: S, events: Vec<E>) -> Self {
        Self::Applied {
            state: Some(state),
            events,
        }
    }

    /// A transition that removed the case, which
    /// [`explore`](crate::explore::explore) reports as
    /// [`TransitionRemovesCase`](crate::explore::ExplorationViolationKind::TransitionRemovesCase).
    ///
    /// A case's identity outlives its content, so a domain whose working
    /// document is consumed on success moves the case to a terminal *status*
    /// instead of dropping it, the way the traveler sample's `Deleted` does.
    /// This constructor exists so that a model ported from a delete-on-completion
    /// design produces a reported violation rather than a shape the explorer
    /// cannot express — and so the check itself can be falsified.
    #[must_use]
    pub fn removed(events: Vec<E>) -> Self {
        Self::Applied {
            state: None,
            events,
        }
    }

    /// A refusal.
    #[must_use]
    pub fn rejected(rejection: DomainRejection) -> Self {
        Self::Rejected(rejection)
    }

    /// Returns `true` when the command was applied.
    #[must_use]
    pub fn is_applied(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }

    /// The state after the command, when the command applied and the case still
    /// exists.
    #[must_use]
    pub fn next_state(&self) -> Option<&S> {
        match self {
            Self::Applied { state, .. } => state.as_ref(),
            Self::Rejected(_) => None,
        }
    }

    /// The events the command would commit; empty for a refusal.
    #[must_use]
    pub fn events(&self) -> &[E] {
        match self {
            Self::Applied { events, .. } => events,
            Self::Rejected(_) => &[],
        }
    }

    /// The rejection, when the command was refused.
    #[must_use]
    pub fn rejection(&self) -> Option<&DomainRejection> {
        match self {
            Self::Rejected(rejection) => Some(rejection),
            Self::Applied { .. } => None,
        }
    }
}

/// The transition model of a workflow, used for bounded exploration (spec §8.5).
///
/// A [`WorkflowDefinition`] says how state *projects*; a `WorkflowModel` says
/// how state *moves*. Keeping them apart means the explorer never has to run an
/// executor, a store or a clock.
///
/// Implementations must be deterministic and side-effect free: the explorer
/// deduplicates states by their canonical JSON, so a model that mints a fresh
/// identifier on every call turns a small workflow into an infinite one. Derive
/// identifiers from the state instead (for example, the n-th line always gets
/// the n-th identifier of a fixed table).
pub trait WorkflowModel<W: WorkflowDefinition> {
    /// The states exploration starts from. `None` means "the case does not
    /// exist yet", which is where most workflows begin — and it never means
    /// "the case is over", because a case's identity outlives its content.
    fn initial_states(&self) -> Vec<Option<W::State>>;

    /// Commands worth trying in this state, in a stable order. Include commands
    /// you expect to be refused: the explorer checks that a refusal changes
    /// nothing.
    fn candidate_commands(&self, state: Option<&W::State>) -> Vec<W::Command>;

    /// Applies one candidate command purely.
    fn simulate(
        &self,
        state: Option<&W::State>,
        command: &W::Command,
    ) -> SimulatedTransition<W::State, W::Event>;

    /// Outcomes the workflow claims it can reach. The explorer reports every
    /// declared outcome no reachable state projects. The default is empty,
    /// which disables the check.
    fn declared_outcomes(&self) -> Vec<W::Outcome> {
        Vec::new()
    }
}
