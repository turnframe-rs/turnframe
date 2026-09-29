//! `turnframe-test` — the test kit for Turnframe.
//!
//! The promise that business effects stay deterministic while the conversation
//! stays natural is worth the tests behind it, and those are hard to write: a
//! plan whose spans must quote the user exactly, a workflow whose invariants
//! must hold in *every* reachable state, a model double that must refuse to
//! improvise, a receipt that may not claim more than the ledger says. This crate
//! writes that machinery once.
//!
//! | Module | What it gives you |
//! |---|---|
//! | [`executors`] | the conformance suite for a [`WorkflowExecutor`](turnframe_core::flow::WorkflowExecutor) you wrote, sharpest about the batch that half-committed |
//! | [`explore`] | bounded breadth-first exploration of a workflow's reachable states, with the shortest command path to anything that breaks |
//! | [`strategies`] | `proptest` strategies over the core types, including understandings *grounded* in a text so every range is words of it |
//! | [`workflows`] | three complete sample domains, a travel-disruption desk (trip, traveler, expense claim), each with a pure transition function, an in-memory executor and an exploration model |
//! | [`providers`] | a scripted provider that fails loudly on an unscripted call, an [`UnderstandingBuilder`](providers::UnderstandingBuilder) that turns quotes into word ranges, a scripted understanding, and the provider conformance harness |
//! | [`stores`] | the in-memory persistence layer with failure injection at named crash boundaries, call counting and a frozen clock |
//! | [`replay`] | two executions compared artefact by artefact, and a replay record asked to account for its own turn |
//! | [`assertions`] | the checks that keep coming up: no high-risk command without a trusted origin, no receipt without events, one phase per projection |
//!
//! Doubles for the runtime itself arrive with `turnframe-runtime`: this crate
//! never depends on it, so a runtime test can use it without a cycle.
//!
//! # Example
//!
//! Explore the trip sample and assert that nothing in its reachable state
//! space breaks a projection invariant.
//!
//! ```
//! use turnframe_test::explore::{ExplorationLimits, explore};
//! use turnframe_test::workflows::trip::{TripModel, TripOutcome, TripWorkflow};
//!
//! let report = explore(
//!     &TripWorkflow::default(),
//!     &TripModel::default(),
//!     ExplorationLimits::standard(),
//! );
//!
//! assert!(report.is_clean(), "{}", report.describe());
//! assert!(report.reached_outcome(&TripOutcome::Notified));
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its examples cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod assertions;
pub mod executors;
pub mod explore;
pub mod projection;
pub mod providers;
pub mod replay;
pub mod stores;
pub mod strategies;
pub mod workflows;

/// The items a test file usually wants: `use turnframe_test::prelude::*;`.
///
/// It re-exports [`turnframe_core::prelude`] as well, so a test rarely needs
/// more than this one import.
pub mod prelude {
    pub use turnframe_core::prelude::*;

    pub use crate::assertions::{
        AssertionFailure, identical_blocks, no_high_risk_without_trusted_origin,
        origin_satisfies_policy, receipts_backed_by_events, same_phase_in, single_phase,
    };
    pub use crate::executors::{
        self, CHECK_COUNT as EXECUTOR_CHECK_COUNT, ExecutorFactory, InMemoryCase, SeededCase,
    };
    pub use crate::explore::{
        ExplorationLimits, ExplorationReport, ExplorationViolation, ExplorationViolationKind,
        SimulatedTransition, WorkflowModel, explore, reachable_states,
    };
    pub use crate::providers::{
        RecordedCall, ScriptViolation, ScriptedProvider, ScriptedReply, ScriptedUnderstanding,
        UnderstandingBuilder,
    };
    pub use crate::replay::{
        ReplayDivergence, ReplayEvidence, ReplayGap, ReplayGaps, TurnExecution, same_turn,
    };
    pub use crate::stores::{FailurePoint, FakeStores, Stores, crash_boundary};
    pub use crate::strategies;
    pub use crate::workflows::claim::{
        ClaimCommand, ClaimEvent, ClaimExecutor, ClaimField, ClaimModel, ClaimObligation,
        ClaimOutcome, ClaimPhase, ClaimState, ClaimStatus, ClaimWorkflow, Proposal, ProposedField,
        RecordedField,
    };
    pub use crate::workflows::traveler::{
        DeclineReason, FieldState, TravelerCommand, TravelerEvent, TravelerExecutor, TravelerModel,
        TravelerObligation, TravelerOutcome, TravelerPhase, TravelerState, TravelerStatus,
        TravelerWorkflow,
    };
    pub use crate::workflows::trip::{
        TripCommand, TripEvent, TripExecutor, TripModel, TripObligation, TripOutcome, TripPhase,
        TripState, TripStatus, TripWorkflow,
    };
    pub use crate::workflows::{Applied, InMemoryExecutor, PureWorkflow, simulate};
}
