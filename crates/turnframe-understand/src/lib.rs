//! Turn understanding as small verified model tasks.
//!
//! An [`Understander`] reads one turn through a fixed set of narrow tasks run by the
//! `turnframe-tasks` engine, then assembles their answers into one
//! [`Understanding`](turnframe_core::understanding::Understanding) by code. Models judge
//! language; code checks structure: a task points at the user's words instead of
//! quoting them, gives dates and amounts as expressions code evaluates, and chooses only
//! from closed sets built for its call. A verifier can take an act away, never add one.
//!
//! | Module | What it holds |
//! | --- | --- |
//! | [`input`] | what a turn's understanding may see |
//! | [`words`] | the user's words, numbered, and pointers into them |
//! | [`tasks`] | the task kinds, their prompts, schemas and checks |
//! | [`values`] | from an extraction to the operation's values |
//! | [`check`] | the domain's own check of an act |
//! | [`progress`] | the steps a turn's understanding publishes as it runs |
//! | [`pipeline`] | the order tasks run in, and the settings |

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod assemble;
pub mod check;
pub mod input;
pub mod pipeline;
pub mod progress;
mod render;
mod schema;
pub mod tasks;
pub mod values;
pub mod words;

pub use check::{ActChecker, NoChecks};
pub use input::{
    CardOption, Expectation, OpenCard, PendingAct, PreviousReceipt, RecordBrief, Speaker,
    TranscriptMessage, UnderstandingInput, WorkflowBrief,
};
pub use pipeline::{Settings, TurnUnderstander, Understander, VerifyPolicy};
pub use progress::{ChannelSteps, NoSteps, RecordedSteps, Step, StepSink};
pub use words::{Span, Words};
