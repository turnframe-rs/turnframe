//! `turnframe-provider-bedrock`: the AWS Bedrock Converse adapter (spec §20.5), through the
//! AWS SDK for Rust: SigV4 signing, credential refresh and endpoint resolution hang off the
//! SDK, which holds the credential so this crate never does (spec §25.2).
//!
//! Structured output is a forced tool whose `inputSchema` is the schema, never executed: the
//! [`NativeFunctionSchema`](turnframe_provider::capabilities::StructuredOutputCapability::NativeFunctionSchema)
//! of spec §20.4. Capabilities are declared per model id, since Bedrock is a marketplace and
//! not a model; [`BedrockProvider::converse_defaults`] claims only what the protocol decides.
//!
//! ```
//! use aws_sdk_bedrockruntime::config::{BehaviorVersion, Credentials, Region};
//! use turnframe_provider::capabilities::{StructuredOutputCapability, ToolCallingCapability};
//! use turnframe_provider::prelude::*;
//! use turnframe_provider_bedrock::BedrockProvider;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // In an application this is `aws_config::load_from_env().await`, passed to
//! // `.sdk_config(&sdk)`; spelled out here so the example needs no environment.
//! let aws = aws_sdk_bedrockruntime::Config::builder()
//!     .behavior_version(BehaviorVersion::latest())
//!     .region(Region::new("eu-central-1"))
//!     .credentials_provider(Credentials::new(
//!         "AKIAEXAMPLE", "not-a-real-secret", None, None, "example",
//!     ))
//!     .build();
//!
//! let provider = BedrockProvider::builder()
//!     .service_config(aws)
//!     .model("anthropic.claude-sonnet-4-5-20250929-v1:0")
//!     .capabilities(
//!         BedrockProvider::converse_defaults()
//!             .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
//!             .with_tool_calling(ToolCallingCapability::Parallel)
//!             .with_vision(true)
//!             .with_documents(true)
//!             .with_max_context_tokens(200_000),
//!     )
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
//! | | Converse | Handled by |
//! |---|---|---|
//! | The system prompt | a separate `system` array | hoisting [`Role::System`](turnframe_provider::request::Role::System) into it |
//! | A tool result | a block in the **user** turn | mapping [`Role::Tool`](turnframe_provider::request::Role::Tool) to `user`, results first |
//! | Consecutive same-role turns | rejected | merging them into one turn |
//! | A failed tool result | a real `status` field | no marker prefix is invented |
//! | Attachments | `image` and `document` blocks, with an explicit format and inline bytes | the media type routes them; a non-S3 URL has nowhere to go and is dropped with a warning |
//! | `toolChoice: none` | does not exist | the tools are withheld, which is what the caller asked for |
//! | Prompt caching | explicit `cachePoint` blocks | [`CacheHint`](turnframe_provider::request::CacheHint) |
//! | Idempotency | no token | the request id travels as a `requestMetadata` correlation label |
//! | Usage | `inputTokens` **excludes** the cache counters | they are added back, so `input` means the whole prompt |
//!
//! | Module | What it holds |
//! |---|---|
//! | [`builder`] | [`BedrockProviderBuilder`] and [`ConfigError`] |
//! | root | [`BedrockProvider`], the [`ModelProvider`](turnframe_provider::provider::ModelProvider) implementation |
//!
//! Wire conversion stays private, so no SDK type reaches the runtime.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod builder;
mod error;
mod provider;
mod wire;

pub use builder::{
    BedrockProviderBuilder, ConfigError, DEFAULT_MAX_STOP_SEQUENCES, DEFAULT_PROVIDER_KEY,
};
pub use error::{QUOTA_SCOPE_PROVISIONED_THROUGHPUT, QUOTA_SCOPE_SERVICE_QUOTA};
pub use provider::BedrockProvider;
