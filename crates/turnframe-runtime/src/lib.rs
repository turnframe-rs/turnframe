//! `turnframe-runtime`: what a user's turn actually does. The model proposes meaning,
//! deterministic code decides effects, committed events decide claims.
//!
//! [`orchestrator::Orchestrator::handle_turn`] runs the pipeline of spec §23, one stage
//! per module, so a trace, a phase marker and a replay record point at the same place.
//! The invariants the stages make concrete are tabulated in `docs/architecture.md`, and
//! where each signal fires in `docs/telemetry.md`.
//!
//! | Stage | Module | What it decides |
//! | --- | --- | --- |
//! | configuration | [`config`] | how much autonomy the model gets, which risk classes a sandbox refuses, the conservative defaults |
//! | budget | [`budget`] | what a turn may spend, and which bound stopped it |
//! | understand | `understand` (private) | what the turn asks, read by small verified model tasks into one [`Understanding`](turnframe_core::understanding::Understanding) |
//! | resolve | [`resolve`] | which record "the Ferri trip" is, or that it is a question; never a guess (I8) |
//! | policy | [`policy`] | whether a command may run now, and if not, which card would authorize it |
//! | reduce | [`reduce`] | every act's explicit result for the whole turn (§13, I11) |
//! | interactions | [`interactions`] | durable cards, persisted before any sentence refers to them (§15) |
//! | resume | [`resume`] | what a card remembers, so answering it continues the act it interrupted |
//! | execute | [`execute`] | admission before effect, optimistic concurrency, the outbox, one atomic commit (§16) |
//! | compose | [`compose`] | receipts from committed events, one answer per question, and an acknowledgement written from the turn's outcome and reviewed |
//! | stream | [`stream`] | nothing that states an outcome goes on the wire before the commit (§18.5) |
//! | trace | [`trace`] | every event and model call of a turn, as one JSON line each |
//! | recover | [`recover`] | after a crash: resume by idempotency key, regenerate the answer, or reconcile |
//! | dispatch | [`dispatch`] | the external-effect saga: claim a due outbox row, send it, settle it |
//! | planning | [`planning`] | the pipeline stopped before the first effect, for a path running beside an existing one |
//! | divergence | [`divergence`] | what the two paths disagreed about |
//!
//! # Example
//!
//! Configure the runtime and check that a sandbox refuses what §11.4 says it
//! must.
//!
//! ```
//! use turnframe_core::prelude::*;
//! use turnframe_runtime::config::{OrchestrationMode, OrchestratorConfig, ResourceBudget,
//!     SandboxAcknowledgement};
//!
//! let config = OrchestratorConfig::conservative();
//! config.validate()?;
//! assert_eq!(config.mode, OrchestrationMode::Deterministic);
//!
//! let sandbox = OrchestrationMode::sandboxed_autonomous(
//!     ResourceBudget::conservative(),
//!     SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes(),
//! );
//! assert!(sandbox.allows_risk(RiskClass::ReversibleLowRisk));
//! assert!(!sandbox.allows_risk(RiskClass::ExternalRegulated));
//! # Ok::<(), turnframe_runtime::config::ConfigError>(())
//! ```
//!
//! Wiring a whole orchestrator needs a workflow registry, a provider pool and a
//! set of stores; the runnable version lives in the integration tests, where
//! [`tests/support/mod.rs`] assembles the sample trip and traveler domains
//! against the in-memory stores and a scripted provider.
//!
//! [`tests/support/mod.rs`]: https://github.com/turnframe-rs/turnframe/blob/main/crates/turnframe-runtime/tests/support/mod.rs

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// `OrchestratorError` is the library's canonical failure family (spec §24) and
// it is deliberately wide: it carries the domain rejection, the revision
// conflict or the store failure that caused it, because a caller that has to
// re-read a `Display` string to find out whether an effect may exist has been
// handed the wrong type. It is returned at most once per turn, on a path that
// already did I/O, so boxing it would move an allocation onto the happy path to
// save copying a value on the failing one.
#![allow(clippy::result_large_err)]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod attachments;
pub mod budget;
pub mod compose;
pub mod config;
pub mod conversation;
pub mod copy;
pub mod dispatch;
pub mod divergence;
pub mod effort;
pub mod execute;
pub mod interactions;
mod narrate;
pub mod orchestrator;
pub mod planning;
pub mod policy;
pub mod recover;
pub mod reduce;
pub mod resolve;
pub mod resume;
mod signals;
pub mod stream;
pub mod trace;
mod turn;
mod understand;
