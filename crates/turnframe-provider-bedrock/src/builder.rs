//! Building a provider, and the things the builder refuses to do.
//!
//! [`BedrockProviderBuilder`] assembles a [`BedrockProvider`] from an SDK
//! client, a model id and a capability declaration. Every check it runs exists
//! to keep a promise:
//!
//! * **A declaration cannot outrun what Converse can carry.** The API has no
//!   response format, no JSON mode and no grammar, so
//!   [`NativeJsonSchema`](StructuredOutputCapability::NativeJsonSchema),
//!   [`JsonObject`](StructuredOutputCapability::JsonObject) and
//!   [`GrammarConstrained`](StructuredOutputCapability::GrammarConstrained) are
//!   refused outright — not lowered, not warned about, refused. There is no
//!   other constructor, so *no* [`BedrockProvider`] can exist claiming a
//!   transport this adapter does not send (spec §0 rule 9, §20.3).
//! * **The forced-tool transport needs tools.**
//!   [`NativeFunctionSchema`](StructuredOutputCapability::NativeFunctionSchema)
//!   *is* a tool call, so declaring it alongside
//!   [`ToolCallingCapability::None`](turnframe_provider::capabilities::ToolCallingCapability::None)
//!   is a contradiction the builder will not assemble.
//! * **The credential stays with the SDK.** There is no key setter and no
//!   `ApiKey`: SigV4 signing, credential resolution and refresh all belong to
//!   the AWS SDK, so a credential never reaches this crate's memory, `Debug`
//!   output or logs (spec §25.2).
//! * **A misconfiguration fails at build time**, as a [`ConfigError`] naming
//!   the field at fault, rather than as a `ValidationException` in production.
//!
//! # Capabilities are per model, and Bedrock will not tell you them
//!
//! Converse is one wire format in front of many vendors — Anthropic, Meta,
//! Mistral, Amazon, Cohere, AI21 — and they differ in what they support: some
//! take images, some take documents, some take no tools at all, and the tool
//! and document limits differ again. Bedrock exposes no capability endpoint,
//! and the request that would probe one costs a call and an answer.
//!
//! So this adapter never guesses. [`capabilities`](BedrockProviderBuilder::capabilities)
//! is where you record what you measured for **this model id**, and the
//! conformance suite is how you measure it. What is *not* left to the adopter
//! is the handful of properties the Converse protocol itself decides — see
//! [`BedrockProvider::converse_defaults`].

use std::time::Duration;

use aws_config::SdkConfig;
use aws_sdk_bedrockruntime::Client;
use aws_sdk_bedrockruntime::config::Config as BedrockConfig;
use turnframe_provider::capabilities::{
    MicroCents, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::request::DEFAULT_TIMEOUT;

use crate::provider::{BedrockProvider, Parts};
use crate::wire::request::WireLimits;

/// The provider key metrics and replay records carry when none is configured.
pub const DEFAULT_PROVIDER_KEY: &str = "bedrock";

/// Stop sequences Converse accepts by default.
///
/// Four is the lowest limit among the model families behind Converse, so it is
/// the safe default; raise it with
/// [`max_stop_sequences`](BedrockProviderBuilder::max_stop_sequences) for a
/// model you have checked.
pub const DEFAULT_MAX_STOP_SEQUENCES: usize = 4;

/// A provider could not be configured.
///
/// `Display` names the field at fault and never its value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// No SDK client, and no configuration to build one from.
    #[error("no Bedrock client: pass one, or the configuration to build one from")]
    MissingClient,
    /// No model was named.
    #[error("no model id: a provider instance is one provider-model pair")]
    MissingModel,
    /// The declared transport is one Converse does not have.
    #[error(
        "Converse has no response format and no grammar, so this adapter sends a forced tool, \
         a prompt description or nothing, and cannot honour {declared}"
    )]
    UnsupportedTransport {
        /// What was declared.
        declared: StructuredOutputCapability,
    },
    /// `NativeFunctionSchema` was declared without tool calling.
    #[error(
        "native_function_schema is a forced tool call, so it cannot be declared with \
         tool_calling: none"
    )]
    TransportNeedsToolCalling,
    /// The stop-sequence limit was set to zero, which would drop every stop
    /// sequence silently.
    #[error("max_stop_sequences is zero: a limit of none is not a limit")]
    NoStopSequencesAllowed,
}

impl From<ConfigError> for ProviderError {
    /// A configuration fault is an invalid request the adapter makes of itself,
    /// and it will not get better on a retry.
    fn from(value: ConfigError) -> Self {
        Self::invalid_request(match value {
            ConfigError::MissingClient => "missing_client",
            ConfigError::MissingModel => "missing_model",
            ConfigError::UnsupportedTransport { .. } => "unsupported_transport",
            ConfigError::TransportNeedsToolCalling => "transport_needs_tool_calling",
            ConfigError::NoStopSequencesAllowed => "no_stop_sequences_allowed",
        })
    }
}

/// Assembles a [`BedrockProvider`].
///
/// A model with no capability declaration gets
/// [`BedrockProvider::converse_defaults`], which claims nothing
/// model-specific — so it can write replies, and an understanding task
/// is refused rather than served on a guess.
///
/// ```
/// use aws_sdk_bedrockruntime::config::{BehaviorVersion, Credentials, Region};
/// use turnframe_provider::provider::ModelProvider;
/// use turnframe_provider_bedrock::BedrockProvider;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let aws = aws_sdk_bedrockruntime::Config::builder()
///     .behavior_version(BehaviorVersion::latest())
///     .region(Region::new("eu-central-1"))
///     .credentials_provider(Credentials::new(
///         "AKIAEXAMPLE", "not-a-real-secret", None, None, "example",
///     ))
///     .build();
///
/// let provider = BedrockProvider::builder()
///     .service_config(aws)
///     .model("meta.llama3-70b-instruct-v1:0")
///     .build()?;
///
/// assert!(provider.capabilities().streaming);
/// assert!(!provider.capabilities().supports_tools());
/// # Ok(())
/// # }
/// ```
pub struct BedrockProviderBuilder {
    provider: Option<ProviderKey>,
    model: Option<ModelKey>,
    client: Option<Client>,
    capabilities: Option<ProviderCapabilities>,
    timeout: Duration,
    max_stop_sequences: usize,
    send_request_metadata: bool,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl std::fmt::Debug for BedrockProviderBuilder {
    /// The SDK client is deliberately absent: it owns the credential provider.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BedrockProviderBuilder")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("has_client", &self.client.is_some())
            .field("capabilities", &self.capabilities)
            .field("timeout", &self.timeout)
            .field("max_stop_sequences", &self.max_stop_sequences)
            .field("send_request_metadata", &self.send_request_metadata)
            .field("region", &self.region)
            .field("tags", &self.tags)
            .finish_non_exhaustive()
    }
}

impl Default for BedrockProviderBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BedrockProviderBuilder {
    /// A builder with nothing configured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            provider: None,
            model: None,
            client: None,
            capabilities: None,
            timeout: DEFAULT_TIMEOUT,
            max_stop_sequences: DEFAULT_MAX_STOP_SEQUENCES,
            send_request_metadata: true,
            cost: None,
            region: None,
            tags: Vec::new(),
        }
    }

    /// Uses an SDK client the adopter already built.
    ///
    /// The most direct of the three: whatever credential provider, region,
    /// endpoint, retry policy and interceptors that client carries are the ones
    /// every call uses.
    #[must_use]
    pub fn client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Builds a client from a shared [`SdkConfig`].
    ///
    /// The usual path: `aws_config::load_from_env().await` once, then one
    /// provider per model on top of it.
    ///
    /// # Panics
    ///
    /// The SDK panics — with a message naming the missing piece — when the
    /// configuration has no behaviour version, no HTTP client or no sleep
    /// implementation while retries or timeouts are enabled. Use
    /// [`client`](Self::client) to build the client yourself if you would
    /// rather handle that at the call site.
    #[must_use]
    pub fn sdk_config(self, config: &SdkConfig) -> Self {
        let region = config.region().map(|region| region.to_string());
        let builder = self.client(Client::new(config));
        match region {
            Some(region) => builder.region(region),
            None => builder,
        }
    }

    /// Builds a client from a Bedrock-specific [`BedrockConfig`].
    ///
    /// The path for a deployment that needs a service-level override — a VPC
    /// endpoint, its own retry policy, a test endpoint.
    ///
    /// # Panics
    ///
    /// As [`sdk_config`](Self::sdk_config).
    #[must_use]
    pub fn service_config(self, config: BedrockConfig) -> Self {
        self.client(Client::from_conf(config))
    }

    /// The model id, e.g. `anthropic.claude-sonnet-4-5-20250929-v1:0`.
    ///
    /// One provider instance is one provider-model pair, and an inference
    /// profile ARN is as good a value here as a bare model id.
    #[must_use]
    pub fn model(mut self, model: impl Into<ModelKey>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// The capability declaration for this model.
    ///
    /// This is the honest place to record what a conformance run measured. The
    /// default is [`BedrockProvider::converse_defaults`], which claims only
    /// what the Converse protocol itself decides; everything model-specific
    /// starts off and is added here.
    #[must_use]
    pub fn capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// Overrides the provider key metrics and replay records are labelled with.
    #[must_use]
    pub fn provider_key(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// The transport deadline.
    ///
    /// The effective deadline of a call is the smaller of this and
    /// [`ModelRequest::timeout`](turnframe_provider::request::ModelRequest::timeout),
    /// so a caller can always ask for less time but never for more.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// How many stop sequences this model accepts.
    #[must_use]
    pub const fn max_stop_sequences(mut self, limit: usize) -> Self {
        self.max_stop_sequences = limit;
        self
    }

    /// Whether request metadata may be sent.
    ///
    /// Bedrock's `requestMetadata` is only useful where model invocation
    /// logging is on, and some accounts reject it outright; turning it off
    /// drops the labels with a warning rather than failing the call.
    #[must_use]
    pub const fn send_request_metadata(mut self, send: bool) -> Self {
        self.send_request_metadata = send;
        self
    }

    /// Per-million-token prices, for cost-aware routing.
    #[must_use]
    pub const fn cost(mut self, input: MicroCents, output: MicroCents) -> Self {
        self.cost = Some((input, output));
        self
    }

    /// The data-residency label, for region-aware routing.
    ///
    /// [`sdk_config`](Self::sdk_config) fills it in from the configured AWS
    /// region; set it explicitly to record something else, such as `"eu"`.
    #[must_use]
    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// A routing tag.
    #[must_use]
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Builds the provider.
    ///
    /// # Errors
    ///
    /// Returns the [`ConfigError`] naming the field at fault.
    pub fn build(self) -> Result<BedrockProvider, ConfigError> {
        let capabilities = self
            .capabilities
            .unwrap_or_else(BedrockProvider::converse_defaults);
        check_transport(&capabilities)?;
        if self.max_stop_sequences == 0 {
            return Err(ConfigError::NoStopSequencesAllowed);
        }
        let client = self.client.ok_or(ConfigError::MissingClient)?;
        let model = self.model.ok_or(ConfigError::MissingModel)?;
        if model.is_empty() {
            return Err(ConfigError::MissingModel);
        }

        Ok(BedrockProvider::assemble(Parts {
            provider: self
                .provider
                .unwrap_or_else(|| ProviderKey::from(DEFAULT_PROVIDER_KEY)),
            model,
            capabilities,
            client,
            timeout: self.timeout,
            limits: WireLimits {
                max_stop_sequences: self.max_stop_sequences,
                send_request_metadata: self.send_request_metadata,
            },
            cost: self.cost,
            region: self.region,
            tags: self.tags,
        }))
    }
}

/// Refuses a declaration this adapter cannot back.
fn check_transport(capabilities: &ProviderCapabilities) -> Result<(), ConfigError> {
    let declared = capabilities.structured_output;
    match declared {
        StructuredOutputCapability::NativeFunctionSchema
        | StructuredOutputCapability::PromptOnly
        | StructuredOutputCapability::None => {}
        other => return Err(ConfigError::UnsupportedTransport { declared: other }),
    }
    if declared == StructuredOutputCapability::NativeFunctionSchema
        && !capabilities.supports_tools()
    {
        return Err(ConfigError::TransportNeedsToolCalling);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_provider::capabilities::ToolCallingCapability;

    #[test]
    fn a_transport_converse_does_not_have_cannot_be_declared() {
        for declared in [
            StructuredOutputCapability::NativeJsonSchema,
            StructuredOutputCapability::JsonObject,
            StructuredOutputCapability::GrammarConstrained,
        ] {
            let capabilities = ProviderCapabilities::minimal().with_structured_output(declared);
            assert_eq!(
                check_transport(&capabilities),
                Err(ConfigError::UnsupportedTransport { declared })
            );
        }
    }

    #[test]
    fn the_forced_tool_transport_cannot_be_declared_without_tools() {
        let capabilities = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeFunctionSchema);
        assert_eq!(
            check_transport(&capabilities),
            Err(ConfigError::TransportNeedsToolCalling)
        );
        let with_tools = capabilities.with_tool_calling(ToolCallingCapability::Sequential);
        assert!(check_transport(&with_tools).is_ok());
    }

    #[test]
    fn a_builder_with_no_client_names_the_field_at_fault() {
        let error = BedrockProviderBuilder::new()
            .model("anthropic.claude-sonnet-4-5-20250929-v1:0")
            .build()
            .expect_err("no client was configured");
        assert_eq!(error, ConfigError::MissingClient);
        assert_eq!(
            ProviderError::from(error).code().map(ToString::to_string),
            Some("missing_client".to_owned())
        );
    }

    #[test]
    fn the_default_declaration_claims_streaming_and_nothing_model_specific() {
        let defaults = BedrockProvider::converse_defaults();
        assert!(
            defaults.streaming,
            "ConverseStream backs every Converse model"
        );
        assert!(defaults.preserves_call_ids);
        assert_eq!(
            defaults.structured_output,
            StructuredOutputCapability::None,
            "a structured-output claim is measured per model, never assumed"
        );
        assert_eq!(defaults.tool_calling, ToolCallingCapability::None);
        assert!(!defaults.vision);
        assert!(check_transport(&defaults).is_ok());
    }
}
