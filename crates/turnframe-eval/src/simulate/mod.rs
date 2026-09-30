//! Conversations evaluated by simulated users (ADR-021): a model plays a person with a goal
//! and a manner, talks to the runtime until the goal or a turn limit, and code scores each
//! conversation from the stores and the turns.
//!
//! | Module | What it owns |
//! | --- | --- |
//! | [`goal`] | what the person wants, loaded strictly from `.toml`, and the state that proves it |
//! | [`user`] | what the person sees and does next: scripted, or played by a model |
//! | [`converse`](mod@converse) | one conversation held with a harness's runtime, and a run over many |
//! | [`score`](mod@score) | the classes a conversation is scored on, and a run's rates |
//!
//! The run and its classes are described in `docs/evaluation.md`.

pub mod converse;
pub mod goal;
pub mod score;
pub mod user;

pub use converse::{Simulation, converse, turn_at};
pub use goal::{Goal, Reached};
pub use score::{Conversation, ConversationScore, Ending, Exchange, SimulationReport, score};
pub use user::{CardOnScreen, ModelUser, Screen, ScriptedUser, SimulatedUser, UserMove};
