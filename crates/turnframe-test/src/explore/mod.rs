//! Bounded workflow exploration (spec §8.5).
//!
//! Fixtures prove that the states you thought of behave. Exploration proves
//! something else: that *every state reachable from them* still satisfies the
//! Flow Map invariants. You give the explorer a [`WorkflowModel`] — the initial
//! states, the commands worth trying and a pure simulation of each — and it
//! walks the reachable states breadth-first, projecting each one and checking
//! the §8.4 rules on it.
//!
//! Because the search is breadth-first and states are deduplicated by their
//! canonical JSON, a violation is reported with the *shortest* command path
//! that reaches it, which is usually the smallest reproduction you can get.
//!
//! One of the rules is a position rather than a mechanical check: a case's
//! identity outlives its content, so an absent state means *not yet* and never
//! *no longer*. A projector that gives an absent state a terminal phase or an
//! outcome, or a model that answers a command by dropping the case, is
//! describing a case that ends by disappearing, and the explorer reports both.
//! See [`ExplorationViolationKind::CaseEndsByDisappearing`].
//!
//! ```
//! use turnframe_test::explore::{ExplorationLimits, explore};
//! use turnframe_test::workflows::trip::{TripModel, TripWorkflow};
//!
//! let report = explore(
//!     &TripWorkflow::default(),
//!     &TripModel::default(),
//!     ExplorationLimits::smoke(),
//! );
//! assert!(report.is_clean(), "{}", report.describe());
//! assert!(report.states_explored > 1);
//! ```

mod limits;
mod model;
mod report;
mod search;

pub use limits::ExplorationLimits;
pub use model::{SimulatedTransition, WorkflowModel};
pub use report::{ExplorationReport, ExplorationViolation, ExplorationViolationKind};
pub use search::{EXPLORATION_CASE_ID, EXPLORATION_REVISIONS, explore, reachable_states};
