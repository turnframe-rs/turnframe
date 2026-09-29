//! `turnframe-provider`: the provider-neutral model layer of Turnframe (spec §20).
//!
//! The core runtime never sees OpenAI-, Anthropic-, Gemini- or Bedrock-specific
//! wire types (spec §0 rule 8). It speaks this crate's normalized vocabulary:
//!
//! * a [`request::ModelRequest`] describes one model call: purpose, messages,
//!   expected [`request::OutputSpec`], read-only tools, limits and metadata;
//! * a [`response::ModelResponse`] carries the normalized answer, a
//!   [`response::FinishReason`] and [`response::TokenUsage`];
//! * a [`stream::ModelStream`] delivers the same answer incrementally and
//!   [`stream::reconstruct`] rebuilds the full response deterministically;
//! * [`structured::parse_structured`] turns a response into a typed value
//!   **all-or-nothing** (spec I18): schema validation with deny-unknown semantics,
//!   then typed deserialization; a single malformed act rejects the whole output;
//! * [`error::ProviderError`] is the typed failure family, classified by
//!   [`error::RetryClass`] and free of secrets, request bodies and headers;
//! * [`provider::ModelProvider`] is the trait every adapter implements against a
//!   configured [`capabilities::ModelProfile`]; capabilities are declared per
//!   provider-model pair, never inferred from the brand (spec §20.3);
//! * [`router::PolicyRouter`] selects candidates by capability fit first, then
//!   tenant policy, then health and preference — and refuses to downgrade the
//!   structured-output requirement of a critical stage (spec §0 rule 9, §20.4);
//! * [`fallback::execute_with_fallback`] tries candidates in order, retries by
//!   class, records every attempt and never merges partial outputs (spec §20.7).
//!
//! Concrete adapters live in sibling crates (`turnframe-provider-openai`, …) and
//! prove themselves with the reusable [`conformance`] suite (feature
//! `conformance`, spec §20.8).
//!
//! # Where the safety rules live
//!
//! | Rule | Where it is enforced |
//! |------|----------------------|
//! | Model arrays are all-or-nothing (I18) | [`structured`], [`stream::StreamAccumulator`] |
//! | No silent capability downgrade (§0.9) | [`purpose::ModelPurpose::requirements`], [`router::PolicyRouter`] |
//! | Provider failure cannot repeat effects (I17) | [`fallback::FallbackStage`] is a required parameter |
//! | Secrets never reach prompts or logs (§25.2) | [`secret::ApiKey`], [`secret::Redactor`], error `Display` |
//! | Record every provider attempt (§20.7) | [`fallback::ProviderAttempt`] |

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod capabilities;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod dialect;
pub mod error;
pub mod fallback;
pub mod ids;
pub mod provider;
pub mod purpose;
pub mod request;
pub mod response;
pub mod router;
pub mod secret;
pub mod stream;
pub mod structured;
pub mod testing;
pub mod trace;

/// The most used items, for `use turnframe_provider::prelude::*`.
pub mod prelude {
    pub use crate::capabilities::{
        CapabilityMismatch, CapabilityRequirements, MicroCents, MissingCapability, ModelProfile,
        ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
    };
    pub use crate::error::{ErrorCode, ProviderError, ProviderErrorKind, RetryClass};
    pub use crate::fallback::{
        FallbackFailure, FallbackOptions, FallbackOutcome, FallbackStage, ProviderAttempt,
        RetryPolicy, execute_with_fallback,
    };
    pub use crate::ids::{AttemptNumber, CallId, ModelKey, ModelRef, ProviderKey, RequestId};
    pub use crate::provider::ModelProvider;
    pub use crate::purpose::{LoggingPolicy, ModelPurpose, SafetyMode};
    pub use crate::request::{
        CacheHint, ContentPart, DocumentSource, ImageSource, Message, ModelRequest, OutputSpec,
        RequestMetadata, Role, ToolCall, ToolChoice, ToolResult, ToolSpec,
    };
    pub use crate::response::{FinishReason, ModelResponse, ResponseWarning, TokenUsage};
    pub use crate::router::{
        Clock, PolicyRouter, ProviderCandidate, ProviderPool, ProviderPoolBuilder, ProviderRouter,
        RoutingError, RoutingPolicy,
    };
    pub use crate::secret::{ApiKey, DefaultRedactor, Redactor};
    pub use crate::stream::{ModelStream, StreamAccumulator, StreamEvent, reconstruct};
    pub use crate::structured::{
        CompiledSchema, SchemaCache, StructuredOutputError, parse_structured,
    };
}
