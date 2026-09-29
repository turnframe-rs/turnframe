//! `turnframe-provider-openai`: the chat-completions adapter (spec §20.5), for OpenAI, Azure
//! OpenAI, and every OpenAI-compatible gateway or self-hosted runtime, each configured as a
//! [`profile::EndpointProfile`] with a [`profile::Preset`] to start from. The README says why.
//!
//! A shared wire format is not shared behaviour: endpoints, and models behind one endpoint,
//! differ on schema enforcement, call ids, usage, streaming and errors. So capabilities are
//! declared per profile and model, never inferred (spec §20.3), and only a passing
//! conformance run against that endpoint and model licenses one (spec §20.8).
//!
//! ```
//! use turnframe_provider::prelude::*;
//! use turnframe_provider_openai::OpenAiProvider;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = OpenAiProvider::openai()
//!     .api_key(ApiKey::new("sk-not-a-real-key"))
//!     .model("gpt-4o-2024-08-06")
//!     .build()?;
//!
//! // The declaration an understanding task is admitted by.
//! assert!(
//!     provider
//!         .supports(&ModelPurpose::Extract.requirements())
//!         .is_ok()
//! );
//! # Ok(())
//! # }
//! ```
//!
//! ```rust,ignore
//! let request = ModelRequest::new(ModelPurpose::Extract)
//!     .with_system("Fill each argument from the user's own words.")
//!     .with_message(Message::user("sposta il volo al 30"))
//!     .with_output(OutputSpec::json("extract", schema));
//!
//! let response = provider.generate(request).await?;
//! let arguments: Value = parse_structured(&response, &compiled)?;
//! ```
//!
//! | Declaration | On the wire |
//! |---|---|
//! | `native_json_schema` | a `json_schema` response format carrying the schema |
//! | `native_function_schema` | one function whose parameters *are* the schema, with the tool choice pinned to it |
//! | `grammar_constrained` | vLLM's `guided_json`, or a GBNF grammar compiled from the schema for `llama.cpp` |
//! | `json_object` | a `json_object` response format, plus the schema described in the prompt |
//! | `prompt_only` | the schema described in the prompt |
//!
//! The declaration picks the transport, never the request; a schema a transport cannot carry
//! fails by name. Wire conversion stays private, so no vendor type reaches the runtime.
//!
//! | Module | What it holds |
//! |---|---|
//! | [`profile`] | [`EndpointProfile`], [`RouteShape`], [`AuthScheme`], [`Quirks`], [`GrammarDialect`], [`Preset`] |
//! | [`builder`] | [`OpenAiProviderBuilder`] and [`ConfigError`] |
//! | root | [`OpenAiProvider`], the [`ModelProvider`](turnframe_provider::provider::ModelProvider) implementation |

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod builder;
mod error;
mod grammar;
pub mod profile;
mod provider;
pub mod strict;
mod wire;

pub use builder::{ConfigError, OpenAiProviderBuilder};
pub use profile::{
    AuthScheme, EndpointProfile, GrammarDialect, Preset, Quirks, RouteShape, SystemRole,
};
pub use provider::OpenAiProvider;
