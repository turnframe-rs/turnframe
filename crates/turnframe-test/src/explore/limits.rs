//! How far bounded exploration is allowed to go.

use serde::{Deserialize, Serialize};

/// Bounds on a breadth-first exploration (spec §8.5).
///
/// Exploration is *bounded* on purpose: a workflow with free-text fields has an
/// infinite state space, so the model offers a finite menu of candidate
/// commands and these limits cap the search on top of it. Hitting a limit is
/// not a failure; it marks the report
/// [`truncated`](crate::explore::ExplorationReport::truncated) so a test can
/// tell "no violations found" from "no violations found so far".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExplorationLimits {
    /// Maximum number of distinct states to visit.
    pub max_states: usize,
    /// Maximum number of commands on the path from an initial state.
    pub max_depth: usize,
    /// Maximum number of candidate commands tried in one state.
    pub max_commands_per_state: usize,
}

impl ExplorationLimits {
    /// Builds explicit limits.
    #[must_use]
    pub const fn new(max_states: usize, max_depth: usize, max_commands_per_state: usize) -> Self {
        Self {
            max_states,
            max_depth,
            max_commands_per_state,
        }
    }

    /// Limits for a fast check in a unit test: 128 states, depth 6, 8 commands.
    #[must_use]
    pub const fn smoke() -> Self {
        Self::new(128, 6, 8)
    }

    /// The default: 1024 states, depth 16, 16 commands per state.
    #[must_use]
    pub const fn standard() -> Self {
        Self::new(1024, 16, 16)
    }

    /// Limits for an exhaustive run in CI: 8192 states, depth 32, 32 commands.
    #[must_use]
    pub const fn generous() -> Self {
        Self::new(8192, 32, 32)
    }

    /// Returns a copy with another state budget.
    #[must_use]
    pub const fn with_max_states(mut self, max_states: usize) -> Self {
        self.max_states = max_states;
        self
    }

    /// Returns a copy with another depth budget.
    #[must_use]
    pub const fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Returns a copy with another per-state command budget.
    #[must_use]
    pub const fn with_max_commands_per_state(mut self, max_commands_per_state: usize) -> Self {
        self.max_commands_per_state = max_commands_per_state;
        self
    }
}

impl Default for ExplorationLimits {
    fn default() -> Self {
        Self::standard()
    }
}
