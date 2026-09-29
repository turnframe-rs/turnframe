//! The adapter itself.
//!
//! One [`BedrockProvider`] is one client, one region, one model and one honest
//! capability declaration (spec §20.1). It translates and nothing else: it
//! holds no workflow policy, never sees a workflow view, has no opinion about
//! which acts are safe, and receives no command handler and no external
//! credential (spec §21.3).
//!
//! # The credential is the SDK's, and stays there
//!
//! This adapter has no key field and no `ApiKey`. Bedrock is signed with SigV4
//! by the AWS SDK, from whatever credential provider the adopter configured —
//! an assumed role, an instance profile, a container task role, an SSO session.
//! The adapter never sees the credential, never renders it and never logs it,
//! because it never holds it (spec §25.2). What it does hold is a
//! [`Redactor`], through which every string it lifts off the wire into an error
//! passes first.
//!
//! # The deadline
//!
//! Every call runs under `min(builder timeout, request timeout)`. For
//! [`generate`](BedrockProvider::generate) that deadline bounds the whole
//! exchange; for [`stream`](BedrockProvider::stream) it bounds establishing the
//! call *and* each wait for the next event, so a stream that stalls ends as a
//! timeout instead of hanging the turn. Dropping the future or the stream
//! cancels the call; nothing is left running behind it.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use aws_sdk_bedrockruntime::Client;
use aws_sdk_bedrockruntime::operation::RequestId;
use turnframe_provider::capabilities::{MicroCents, ModelProfile, ProviderCapabilities};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::request::ModelRequest;
use turnframe_provider::response::ModelResponse;
use turnframe_provider::secret::{DefaultRedactor, Redactor};
use turnframe_provider::stream::ModelStream;

use crate::builder::BedrockProviderBuilder;
use crate::error::classify;
use crate::wire::request::{ConvertedRequest, WireLimits, build_request};
use crate::wire::response::build_response;
use crate::wire::stream::model_stream;

/// Everything [`BedrockProviderBuilder::build`] assembled, handed over in one
/// piece so the provider has no public constructor of its own.
pub(crate) struct Parts {
    pub(crate) provider: ProviderKey,
    pub(crate) model: ModelKey,
    pub(crate) capabilities: ProviderCapabilities,
    pub(crate) client: Client,
    pub(crate) timeout: Duration,
    pub(crate) limits: WireLimits,
    pub(crate) cost: Option<(MicroCents, MicroCents)>,
    pub(crate) region: Option<String>,
    pub(crate) tags: Vec<String>,
}

/// One Bedrock model, reached through the Converse API.
///
/// Build it with [`BedrockProvider::builder`].
pub struct BedrockProvider {
    provider: ProviderKey,
    model: ModelKey,
    capabilities: ProviderCapabilities,
    client: Client,
    timeout: Duration,
    limits: WireLimits,
    redactor: Arc<DefaultRedactor>,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl BedrockProvider {
    /// A builder for one Bedrock model.
    #[must_use]
    pub fn builder() -> BedrockProviderBuilder {
        BedrockProviderBuilder::new()
    }

    /// The declaration the **Converse protocol** backs, whatever model family
    /// is behind it.
    ///
    /// Two claims, and only two, are properties of the API rather than of the
    /// model:
    ///
    /// * `streaming` — every model reachable through `Converse` is reachable
    ///   through `ConverseStream`, and this adapter implements it over that
    ///   operation rather than by chopping up a finished answer;
    /// * `preserves_call_ids` — a `toolUse` block carries the model's own
    ///   `toolUseId` in both directions, so nothing is ever renumbered.
    ///
    /// Everything else starts **off**: structured output, tool calling, vision,
    /// document input, prompt caching and the context window all differ between
    /// the families
    /// behind Converse, Bedrock exposes no capability endpoint, and a
    /// declaration that was guessed is the most dangerous misconfiguration in
    /// the system (spec §20.3). Add what you measured on top:
    ///
    /// ```
    /// use turnframe_provider::capabilities::{StructuredOutputCapability, ToolCallingCapability};
    /// use turnframe_provider_bedrock::BedrockProvider;
    ///
    /// let measured = BedrockProvider::converse_defaults()
    ///     .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
    ///     .with_tool_calling(ToolCallingCapability::Parallel)
    ///     .with_vision(true)
    ///     .with_documents(true)
    ///     .with_max_context_tokens(200_000);
    ///
    /// assert!(measured.streaming);
    /// assert!(measured.structured_output.enforces_schema());
    /// ```
    #[must_use]
    pub const fn converse_defaults() -> ProviderCapabilities {
        ProviderCapabilities::minimal()
            .with_streaming(true)
            .with_preserves_call_ids(true)
            .with_temperature(true)
    }

    /// Assembles the provider. Crate-private: the builder is the only door.
    pub(crate) fn assemble(parts: Parts) -> Self {
        Self {
            provider: parts.provider,
            model: parts.model,
            capabilities: parts.capabilities,
            client: parts.client,
            timeout: parts.timeout,
            limits: parts.limits,
            redactor: Arc::new(DefaultRedactor::new()),
            cost: parts.cost,
            region: parts.region,
            tags: parts.tags,
        }
    }

    /// The SDK client every call goes through.
    ///
    /// Exposed so an adopter can read the configuration it handed over — the
    /// resolved endpoint, the retry policy — without this crate re-exporting
    /// half the SDK.
    #[must_use]
    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// The transport deadline configured on the builder.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The redactor every wire-borne string passes through before it can reach
    /// a log line or an error.
    #[must_use]
    pub fn redactor(&self) -> &dyn Redactor {
        self.redactor.as_ref()
    }

    /// The effective deadline: the smaller of the two.
    fn deadline(&self, request: &ModelRequest) -> Duration {
        self.timeout.min(request.timeout)
    }

    /// Labels an error with this provider-model pair.
    fn label(&self, error: ProviderError) -> ProviderError {
        error.with_model(&self.reference())
    }

    /// Converts the request, labelling any refusal with this pair.
    fn convert(&self, request: &ModelRequest) -> Result<ConvertedRequest, ProviderError> {
        build_request(request, &self.capabilities, self.limits).map_err(|error| self.label(error))
    }
}

impl fmt::Debug for BedrockProvider {
    /// Renders configuration, never credentials.
    ///
    /// The SDK client is deliberately absent: it owns the credential provider,
    /// and a rendering of the adapter is exactly the sort of thing that ends up
    /// in a log line (spec §25.2).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BedrockProvider")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("region", &self.region)
            .field("timeout", &self.timeout)
            .field("capabilities", &self.capabilities)
            .field("tags", &self.tags)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModelProvider for BedrockProvider {
    fn provider_key(&self) -> ProviderKey {
        self.provider.clone()
    }

    fn model_key(&self) -> ModelKey {
        self.model.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn profile(&self) -> ModelProfile {
        let mut profile = ModelProfile::new(
            self.provider.clone(),
            self.model.clone(),
            self.capabilities.clone(),
        );
        if let Some((input, output)) = self.cost {
            profile = profile.with_cost(input, output);
        }
        if let Some(region) = &self.region {
            profile = profile.with_region(region.clone());
        }
        for tag in &self.tags {
            profile = profile.with_tag(tag.clone());
        }
        profile
    }

    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        let started = Instant::now();
        let deadline = self.deadline(&request);
        let converted = self.convert(&request)?;

        let mut call = self.client.converse().model_id(self.model.as_str());
        if !converted.system.is_empty() {
            call = call.set_system(Some(converted.system));
        }
        if !converted.request_metadata.is_empty() {
            call = call.set_request_metadata(Some(converted.request_metadata));
        }
        let call = call
            .set_messages(Some(converted.messages))
            .set_inference_config(converted.inference)
            .set_tool_config(converted.tool_config)
            .send();

        let output = match tokio::time::timeout(deadline, call).await {
            Err(_elapsed) => return Err(self.label(ProviderError::timeout())),
            Ok(Err(error)) => {
                return Err(self.label(classify(&error, self.redactor.as_ref())));
            }
            Ok(Ok(output)) => output,
        };

        let mut response = build_response(
            &output,
            request.request_id,
            &self.provider,
            &self.model,
            self.capabilities.preserves_call_ids,
            output.request_id(),
        )
        .map_err(|error| self.label(error))?;
        response.warnings.extend(converted.warnings);
        Ok(response.with_latency(started.elapsed()))
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        if !self.capabilities.streaming {
            return Err(self.label(ProviderError::unsupported("streaming")));
        }
        let deadline = self.deadline(&request);
        let expires_at = tokio::time::Instant::now() + deadline;
        let converted = self.convert(&request)?;

        let mut call = self.client.converse_stream().model_id(self.model.as_str());
        if !converted.system.is_empty() {
            call = call.set_system(Some(converted.system));
        }
        if !converted.request_metadata.is_empty() {
            call = call.set_request_metadata(Some(converted.request_metadata));
        }
        let call = call
            .set_messages(Some(converted.messages))
            .set_inference_config(converted.inference)
            .set_tool_config(converted.tool_config)
            .send();

        let output = match tokio::time::timeout(deadline, call).await {
            Err(_elapsed) => return Err(self.label(ProviderError::timeout())),
            Ok(Err(error)) => {
                return Err(self.label(classify(&error, self.redactor.as_ref())));
            }
            Ok(Ok(output)) => output,
        };
        Ok(model_stream(
            output,
            self.reference(),
            Box::new(self.redactor.as_ref().clone()),
            expires_at,
            converted.warnings,
        ))
    }
}
