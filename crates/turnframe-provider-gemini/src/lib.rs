//! `turnframe-provider-gemini`: the Google Gemini adapter (spec §20.5), for the Gemini
//! developer API (an API key) and Vertex AI (a project- and region-shaped path and a
//! short-lived bearer token from the caller's [`credential::TokenSource`]). Both are
//! [`profile::EndpointProfile`]s: the translation is shared, the plumbing is not.
//!
//! [`schema::translate_schema`] refuses a keyword Gemini's dialect cannot carry rather than
//! narrowing it. A schema is enforced by `responseSchema` or by a forced function call, as the
//! profile declares. Tool calls have no ids on this wire, so they are synthesized and flagged.
//!
//! ```
//! use turnframe_provider::prelude::*;
//! use turnframe_provider_gemini::GeminiProvider;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = GeminiProvider::gemini()
//!     .api_key(ApiKey::new("AIza-not-a-real-key"))
//!     .model("gemini-2.5-flash")
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
//! ```
//! use std::sync::Arc;
//! use turnframe_provider::secret::ApiKey;
//! use turnframe_provider_gemini::GeminiProvider;
//! use turnframe_provider_gemini::credential::{TokenError, TokenFn};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = GeminiProvider::vertex_ai("aurora-prod", "europe-west4")
//!     .token_source(Arc::new(TokenFn::new(|| async {
//!         // Ask whatever already owns your Google credentials.
//!         std::env::var("VERTEX_ACCESS_TOKEN")
//!             .map(ApiKey::new)
//!             .map_err(|_| TokenError::failed("env_var_missing"))
//!     })))
//!     .model("gemini-2.5-flash")
//!     .build()?;
//! assert!(provider.endpoint().contains("/locations/europe-west4/"));
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
//! | Module | What it holds |
//! |---|---|
//! | [`profile`] | [`EndpointProfile`], [`RouteShape`], [`AuthScheme`], [`Quirks`], [`SafetySetting`] |
//! | [`credential`] | [`TokenSource`], [`StaticToken`], [`TokenFn`], [`TokenError`] |
//! | [`schema`] | [`translate_schema`] and [`SchemaError`] |
//! | [`builder`] | [`GeminiProviderBuilder`] and [`ConfigError`] |
//! | root | [`GeminiProvider`], the [`ModelProvider`](turnframe_provider::provider::ModelProvider) implementation |
//!
//! Wire conversion stays private; [`schema`] is public so an application can find a schema
//! Gemini cannot express at start-up.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod builder;
pub mod credential;
mod error;
pub mod profile;
mod provider;
pub mod schema;
mod wire;

pub use builder::{ConfigError, GeminiProviderBuilder};
pub use credential::{StaticToken, TokenError, TokenFn, TokenSource};
pub use profile::{
    AuthScheme, EndpointProfile, HarmBlockThreshold, HarmCategory, Quirks, RouteShape,
    SafetySetting,
};
pub use provider::GeminiProvider;
pub use schema::{SchemaDialect, SchemaError, translate_schema};
