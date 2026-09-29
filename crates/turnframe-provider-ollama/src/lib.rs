//! `turnframe-provider-ollama`: the Ollama native chat adapter (spec §20.5).
//!
//! Ollama can be reached two ways, and this crate takes the native one.
//!
//! | Route | Endpoint | Adapter |
//! |---|---|---|
//! | **Native** | `POST /api/chat` | this crate |
//! | Compatibility | `POST /v1/chat/completions` | [`turnframe-provider-openai`](https://docs.rs/turnframe-provider-openai) as a compatible profile |
//!
//! An adopter already standardized on the compatibility path loses nothing by
//! staying there. The native route is preferred here because the knobs are on
//! it — `options.num_ctx` is how a caller stops the daemon silently truncating a
//! prompt to its small default window, and `keep_alive` stops it unloading
//! gigabytes between turns, and neither has a home in the compatibility layer —
//! because `format` takes a JSON Schema and constrains decoding against it
//! rather than inheriting a weaker transport, and because the native error body
//! carries the daemon's own words, which is what turns a model nobody pulled
//! into a [`ModelNotFound`](turnframe_provider::error::ProviderErrorKind::ModelNotFound)
//! naming the fix instead of a generic 404.
//!
//! A local runtime is not a small cloud, and three differences shape the rest.
//! There is usually **no credential**, so a bearer token is optional and the
//! ordinary local profile carries none — one is still accepted, because the same
//! daemon behind an authenticating proxy is the other half of this adapter's
//! job. It reports **no cached tokens**, so `cached_input` is always zero and
//! `prompt_caching` is not declared. And it has **no tool-call ids**: they come
//! from this adapter, `preserves_call_ids` is false, and the builder refuses to
//! let a profile claim otherwise.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod builder;
pub mod declarations;
mod error;
mod provider;
mod wire;

pub use builder::{
    CHAT_PATH, ConfigError, DEFAULT_BASE_URL, DEFAULT_PROVIDER_KEY, OllamaProviderBuilder,
    RESERVED_HEADERS,
};
pub use error::{
    DAEMON_UNREACHABLE_CODE, MODEL_NOT_PULLED_CODE, QUOTA_SCOPE_CREDIT_BALANCE, QUOTA_SCOPE_QUOTA,
};
pub use provider::OllamaProvider;
