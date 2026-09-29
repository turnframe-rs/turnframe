//! `turnframe-provider-anthropic`: the Messages API adapter (spec §20.5), pinned to a dated
//! [`anthropic-version`](profile::VERSION_HEADER); a surface that reimplements it is an
//! [`EndpointProfile::compatible`] with a declaration you measured.
//!
//! Structured output is a forced tool. The API has no JSON mode and no grammar, so one tool
//! whose `input_schema` is the required schema is pinned in `tool_choice`, and its input is
//! the document, the [`NativeFunctionSchema`](turnframe_provider::capabilities::StructuredOutputCapability::NativeFunctionSchema)
//! of spec §20.4, never executed (§21.4). The builder refuses every other transport.
//!
//! ```
//! use turnframe_provider::prelude::*;
//! use turnframe_provider_anthropic::AnthropicProvider;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = AnthropicProvider::anthropic()
//!     .api_key(ApiKey::new("sk-ant-not-a-real-key"))
//!     .model("claude-sonnet-4-5-20250929")
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
//! | | Messages API | Handled by |
//! |---|---|---|
//! | The system prompt | a top-level field, not a message | hoisting [`Role::System`](turnframe_provider::request::Role::System) into it |
//! | A tool result | a block in the **user** turn | mapping [`Role::Tool`](turnframe_provider::request::Role::Tool) to `user`, results first |
//! | A failed tool result | a real `is_error` flag | no marker prefix is invented |
//! | `max_tokens` | required | [`Quirks::default_max_output_tokens`](profile::Quirks::default_max_output_tokens) |
//! | Attachments | separate `image` and `document` blocks | the media type routes them |
//! | Prompt caching | `cache_control` breakpoints | [`CacheHint`](turnframe_provider::request::CacheHint) |
//! | Usage | `input_tokens` **excludes** the cache counters | they are added back, so `input` means the whole prompt |
//!
//! | Module | What it holds |
//! |---|---|
//! | [`profile`] | [`EndpointProfile`], [`AuthScheme`], [`Quirks`] |
//! | [`builder`] | [`AnthropicProviderBuilder`] and [`ConfigError`] |
//! | root | [`AnthropicProvider`], the [`ModelProvider`](turnframe_provider::provider::ModelProvider) implementation |
//!
//! Wire conversion stays private, so no vendor type reaches the runtime.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod builder;
mod error;
pub mod profile;
mod provider;
mod wire;

pub use builder::{AnthropicProviderBuilder, ConfigError};
pub use error::{QUOTA_SCOPE_CREDIT_BALANCE, QUOTA_SCOPE_QUOTA, QUOTA_SCOPE_SPEND_LIMIT};
pub use profile::{AuthScheme, EndpointProfile, Quirks};
pub use provider::AnthropicProvider;
