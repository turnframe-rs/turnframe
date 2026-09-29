//! `turnframe-prompt`: the prompt sources Turnframe ships, and nothing else.
//!
//! The contract — [`PromptSource`], [`PromptSelector`], [`LoadedPrompt`],
//! [`PromptError`] and the [`PromptRef`] a replay record stores — lives in
//! `turnframe-core`, so the runtime can accept a source without depending on
//! any particular way of getting one. This crate is the implementations:
//!
//! * [`FilePromptSource`] — prompts compiled into the binary from files in the
//!   adopter's own repository. No network, no credential, no configuration.
//! * [`CachedPromptSource`] — a bounded cache with a freshness window over any
//!   other source, which keeps answering from a version it already holds when a
//!   fetch fails.
//! * [`langfuse`] — behind the `langfuse` feature, a source backed by a
//!   Langfuse project over the **Langfuse v4 public API**.
//!
//! # The repository is the recommended source of truth
//!
//! [`FilePromptSource`] is the setup to reach for in production, and the reason
//! is not convenience. A turn's meaning must not change because somebody edited
//! a registry entry while the system was running. When the prompt is a file in
//! the repository, changing what the assistant does is a code review, a commit
//! and a deploy — the same path as any other behaviour change — and it shows up
//! in the same history. When the prompt is fetched at runtime, an edit made in
//! a browser changes a production system with nobody's name on it, and the
//! turns before and after the edit are no longer comparable even though the
//! binary never changed.
//!
//! The second reason is availability: a prompt compiled into the binary cannot
//! fail to load, so a registry being down cannot stop the assistant from
//! answering.
//!
//! # If you do use the registry, pin a version
//!
//! An adopter who wants registry-managed prompts should select
//! [`PromptSelector::Version`] and not a moving label. A pinned version is
//! immutable, so a registry edit cannot silently change what a running system
//! does; a deploy is still required, which is the property the repository-served
//! setup has for free. The pinned version is also what the replay record cites,
//! so an audit of a turn names something that cannot have been rewritten
//! underneath it.
//!
//! Tracking a label such as `production` is the opposite trade — an edit takes
//! effect without a deploy — and it is a legitimate choice, but it should be a
//! deliberate one.
//!
//! # A source in three lines
//!
//! ```
//! use std::sync::Arc;
//! use turnframe_prompt::{FilePromptSource, PromptFile, PromptSource};
//!
//! static FILES: &[PromptFile] = &[PromptFile::new(
//!     "interpret.system",
//!     "Answer with the plan document only.",
//! )];
//! static PROMPTS: FilePromptSource = FilePromptSource::new(FILES);
//!
//! let source: Arc<dyn PromptSource> = Arc::new(PROMPTS);
//! assert_eq!(source.describe(), "file");
//! ```
//!
//! In a real application the table comes from [`prompt_dir!`], which reads the
//! files from a directory of the adopter's repository at compile time.
//!
//! # Features
//!
//! | Feature | What it turns on |
//! |---|---|
//! | *(none)* | [`FilePromptSource`], [`CachedPromptSource`]: no network, no credential |
//! | `langfuse` | [`langfuse`]: a source backed by a Langfuse project over the Langfuse v4 public API, and the HTTP client it needs |

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod cache;
pub mod files;
#[cfg(feature = "langfuse")]
pub mod langfuse;

pub use crate::cache::{CachedPromptSource, Clock, ManualClock, SystemClock};
pub use crate::files::{FilePromptSource, PromptFile, VERSION_HEX_LEN, version_of};

/// The contract, re-exported so an application configuring a source needs one
/// `use` rather than two.
///
/// These items are defined in `turnframe-core`; they are here for convenience
/// only, and using them from either path is the same type.
pub use turnframe_core::prompt::{
    LoadedPrompt, PromptError, PromptName, PromptRef, PromptSelector, PromptSource, PromptVersion,
};
