//! Small, typed model tasks: the engine that runs every model call of a turn.
//!
//! A [`ModelTask`] states what one call needs and what it may answer. The
//! [`TaskEngine`] resolves its instructions, applies its kind's [`TaskProfile`],
//! reserves the call against the turn's [`Budget`], checks the answer, repairs,
//! votes and escalates, and writes one [`TaskRecord`](turnframe_core::replay::TaskRecord)
//! per call into the [`TaskScope`] the turn owns.
//!
//! | Module | What it decides |
//! | --- | --- |
//! | [`task`] | the task trait, identifiers and structural errors |
//! | [`profile`] | per-kind settings and their defaults |
//! | [`budget`] | what a turn may spend, and which bound stopped it |
//! | [`instructions`] | which instruction text a call runs under |
//! | [`engine`] | running a task: repair, votes, escalation, records |
//! | [`testing`] | a provider that answers each task by its id |

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod budget;
pub mod engine;
pub mod instructions;
pub mod profile;
mod signals;
pub mod task;
pub mod testing;

pub use budget::{Budget, BudgetBound, BudgetTracker};
pub use engine::{
    RecordPolicy, TASK_LABEL, TURN_LABEL, TaskCall, TaskEngine, TaskEngineBuilder, TaskFailure,
    TaskOutcome, TaskScope,
};
pub use profile::{Disagreement, ProfileChange, ProfileChanges, TaskProfile, TaskProfiles};
pub use task::{ModelTask, StructuralError, TaskId, TaskKind};
