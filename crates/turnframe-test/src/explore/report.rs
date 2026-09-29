//! What an exploration found.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use turnframe_core::error::{DomainRejection, InteractionSpecError, InvariantViolation};
use turnframe_core::ids::{OperationKey, WorkflowKey, WorkflowVersion};

use crate::explore::ExplorationLimits;

/// One rule an explored state or transition broke.
///
/// The §8.4 rules about phase ownership, terminal outcomes and duplicate
/// obligation identifiers are reported as [`Self::Projection`]: they come from
/// [`turnframe_core::flow::check_view`], which is the same check the runtime
/// runs in production. The other variants are exploration-only rules that need
/// two projections or a transition to observe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExplorationViolationKind {
    /// A projection invariant of spec §8.4 was broken.
    #[error("projection invariant: {0}")]
    Projection(InvariantViolation),
    /// Two projections of the same state produced different obligation
    /// identifiers, so an obligation cannot be addressed twice in a row.
    #[error("obligation identifiers are not stable across two projections")]
    UnstableObligationIds {
        /// Identifiers of the first projection.
        first: Vec<String>,
        /// Identifiers of the second projection.
        second: Vec<String>,
    },
    /// Two projections of the same state differed. Projection must be pure (I2).
    #[error("projection is not deterministic for this state")]
    NonDeterministicProjection {
        /// The first erased view.
        first: serde_json::Value,
        /// The second erased view.
        second: serde_json::Value,
    },
    /// The same state projected differently at two case revisions.
    ///
    /// A view is a function of the *state*: the revision on the case reference
    /// says which state was read, and travels into the cards built from the
    /// view, but it may not change the phase, the obligations, the blocking
    /// requirement, the notices or the outcome. A projector that reads it — to
    /// hide an obligation on a fresh case, say, or to phrase a notice
    /// differently after an edit — makes the map depend on how often the case
    /// was written, which no caller can reason about.
    #[error(
        "the projection of this state changes between revision {left} and revision {right}: {detail}"
    )]
    ProjectionVariesWithRevision {
        /// The first revision projected at.
        left: u64,
        /// The second revision projected at.
        right: u64,
        /// Which part of the view differs, as a stable label.
        detail: String,
    },
    /// A refused command changed the state it was given.
    #[error("refused command {command_type} mutated the state it was given")]
    RejectedCommandMutatedState {
        /// Serialized command.
        command: serde_json::Value,
        /// Stable label of the command, for logs.
        command_type: String,
    },
    /// An applied command changed the state it was given instead of returning a
    /// new one.
    #[error("applied command {command_type} mutated the state it was given")]
    AppliedCommandMutatedInputState {
        /// Serialized command.
        command: serde_json::Value,
        /// Stable label of the command, for logs.
        command_type: String,
    },
    /// `validate_command` refused a command the model then applied. Validation
    /// and execution disagree, so the executor would commit something the
    /// runtime believes it blocked.
    #[error("command {command_type} was applied although validation refused it ({})", rejection.code)]
    RejectedCommandApplied {
        /// Serialized command.
        command: serde_json::Value,
        /// Stable label of the command, for logs.
        command_type: String,
        /// What validation said.
        rejection: DomainRejection,
    },
    /// A state's catalogue offers an operation its own `compile_act` does not
    /// recognise.
    ///
    /// The declaration is in one function and the translation in another, and
    /// nothing else relates them — so an operation added to the catalogue and
    /// forgotten in the compiler fails as far downstream as a mistake can. The
    /// catalogue offers it, the interpreter proposes it correctly with the right
    /// arguments, and the user is told his request could not be carried out, on
    /// a sentence that was understood perfectly.
    ///
    /// Only a workflow returning
    /// [`turnframe_core::error::UNKNOWN_OPERATION`] is reported. A refusal for
    /// any other reason is a domain refusal and is left alone: an act whose
    /// arguments the explorer could not invent is refused honestly, and a check
    /// that demanded good arguments would be testing the wrong thing.
    #[error("the catalogue offers {operation}, which compile_act does not recognise")]
    CatalogedOperationDoesNotCompile {
        /// The operation the catalogue offered.
        operation: OperationKey,
    },
    /// The blocking requirement of the phase could not be turned into a card.
    #[error("the blocking requirement could not be built into a card ({})", rejection.code)]
    BlockingInteractionNotBuildable {
        /// Why the workflow refused to build it.
        rejection: DomainRejection,
    },
    /// The card built from the blocking requirement cannot be answered, so the
    /// phase would block its case forever (I6).
    #[error("the blocking card cannot be answered: {error}")]
    BlockingInteractionNotAnswerable {
        /// What is wrong with the card.
        error: InteractionSpecError,
    },
    /// A non-terminal state offers no candidate command and no blocking
    /// interaction: the conversation cannot move on from here.
    #[error("dead end: a non-terminal state with no candidate command and no blocking interaction")]
    DeadEnd,
    /// The projector gave an *absent* state a terminal phase or an outcome, so
    /// it describes a case that ends by disappearing.
    ///
    /// A case's identity outlives its content: removal is a status, never an
    /// absence, and an absent state therefore means *not yet* and never *no
    /// longer*. A projector that reads absence as completion is indistinguishable
    /// from one that reads it as a case nobody has started, because the executor
    /// returns the same `None` for both — so the same view has to serve a
    /// finished case and a fresh one, and the assistant congratulates the user
    /// and then asks them to start over. Give the terminal step a status in the
    /// state instead, the way the traveler sample's `Deleted` does.
    #[error(
        "an absent state projects to a terminal phase or an outcome: a case's identity outlives \
         its content, so removal is a status and an absent state means not yet, never no longer"
    )]
    CaseEndsByDisappearing {
        /// The phase the absent state projected to.
        phase: serde_json::Value,
        /// The outcome it carried, when it carried one.
        outcome: Option<serde_json::Value>,
    },
    /// A simulated transition dropped the case: it was given a state and
    /// returned none.
    ///
    /// The other half of [`Self::CaseEndsByDisappearing`], seen from the model
    /// rather than from the projector. A domain whose working document is
    /// consumed on success — a draft that becomes a record, an application that
    /// becomes an account — keeps the case and moves it to a terminal status;
    /// the record it produced is a different case, in its own workflow.
    #[error("applied command {command_type} removed the case instead of moving it to a status")]
    TransitionRemovesCase {
        /// Serialized command.
        command: serde_json::Value,
        /// Stable label of the command, for logs.
        command_type: String,
    },
    /// An outcome the model declares reachable was never projected. Only
    /// reported when the search ran to completion: see
    /// [`ExplorationReport::truncated`].
    #[error("declared outcome was never reached")]
    UnreachableOutcome {
        /// The outcome that was never projected.
        outcome: serde_json::Value,
    },
    /// A state could not be serialized, so it cannot be deduplicated or shown.
    #[error("state could not be serialized")]
    UnserializableState,
    /// A command could not be serialized, so it cannot be shown in a path.
    #[error("command could not be serialized")]
    UnserializableCommand,
}

/// A violation together with where it was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorationViolation {
    /// The rule that was broken.
    pub kind: ExplorationViolationKind,
    /// Canonical JSON of the state, `null` when the case does not exist.
    pub state: serde_json::Value,
    /// Depth at which the state was first reached.
    pub depth: usize,
    /// Shortest command path from an initial state, as canonical JSON.
    pub path: Vec<serde_json::Value>,
}

impl ExplorationViolation {
    /// Renders the violation with its state and path, for a test failure
    /// message. Unlike [`Display`](std::fmt::Display) on
    /// [`ExplorationViolationKind`], this includes domain values.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut out = format!("{}\n  state: {}\n  path:", self.kind, self.state);
        if self.path.is_empty() {
            out.push_str(" <initial state>");
        } else {
            for (step, command) in self.path.iter().enumerate() {
                let _ = write!(out, "\n    {}. {command}", step + 1);
            }
        }
        out
    }
}

/// Everything one bounded exploration observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorationReport {
    /// The workflow explored.
    pub workflow: WorkflowKey,
    /// The version that produced the projections.
    pub workflow_version: WorkflowVersion,
    /// The limits the search ran under.
    pub limits: ExplorationLimits,
    /// Number of distinct states visited.
    pub states_explored: usize,
    /// Number of candidate commands simulated.
    pub transitions_simulated: usize,
    /// Greatest depth reached.
    pub max_depth_reached: usize,
    /// `true` when a limit stopped the search before the frontier was empty.
    /// A truncated report proves that nothing it *visited* is broken, not that
    /// nothing is; the unreachable-outcome check is skipped for it.
    pub truncated: bool,
    /// Canonical JSON of every phase projected by a reachable state, in the
    /// order first reached.
    ///
    /// # Why a count of states was not enough
    ///
    /// `states_explored` and `truncated` say how much was looked at, never
    /// **where**. A workflow's eleven required fields are eleven steps of a
    /// breadth-first search, and the standard budget ran out at depth eight:
    /// four phases of the workflow — the optional question, the summary, its
    /// rejection and the promotion — were never projected at all. Every
    /// invariant reported clean over half a workflow for months, and the
    /// catalogue check found nothing because it never reached the phase where
    /// something was wrong.
    ///
    /// `truncated` did say so, and it is a blunt instrument: a workflow with a
    /// large collection half truncates every time, so the flag stops carrying
    /// information. The question worth asking is whether the search saw every
    /// phase, and that is [`Self::reached_phase`].
    pub reached_phases: Vec<serde_json::Value>,
    /// Canonical JSON of every outcome projected by a reachable state, in the
    /// order first reached.
    pub reached_outcomes: Vec<serde_json::Value>,
    /// Every rule broken, in the order found.
    pub violations: Vec<ExplorationViolation>,
}

impl ExplorationReport {
    /// Returns `true` when nothing was violated.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }

    /// Returns `true` when a reachable state projected `phase`.
    ///
    /// What turns "no violations found" into "no violations found, and here is
    /// where I looked". Assert it for every phase a workflow declares, and a
    /// budget that stops short becomes a failing test instead of a clean one.
    pub fn reached_phase<T: serde::Serialize + ?Sized>(&self, phase: &T) -> bool {
        turnframe_core::hash::canonical_value(phase)
            .is_ok_and(|value| self.reached_phases.contains(&value))
    }

    /// Returns `true` when a reachable state projected `outcome`.
    pub fn reached_outcome<T: serde::Serialize + ?Sized>(&self, outcome: &T) -> bool {
        turnframe_core::hash::canonical_value(outcome)
            .is_ok_and(|value| self.reached_outcomes.contains(&value))
    }

    /// A multi-line summary suitable for an assertion message: the counters,
    /// then every violation with its state and shortest path.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut out = format!(
            "{} {}: {} states, {} transitions, depth {}{}, {} phase(s) reached, {} violation(s)",
            self.workflow,
            self.workflow_version,
            self.states_explored,
            self.transitions_simulated,
            self.max_depth_reached,
            if self.truncated { " (truncated)" } else { "" },
            self.reached_phases.len(),
            self.violations.len(),
        );
        for violation in &self.violations {
            let _ = write!(out, "\n- {}", violation.describe().replace('\n', "\n  "));
        }
        out
    }
}
