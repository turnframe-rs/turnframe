//! The adapter itself.
//!
//! One [`AnthropicProvider`] is one endpoint, one credential, one model and one
//! honest capability declaration (spec §20.1). It translates and nothing else:
//! it holds no workflow policy, never sees a workflow view, has no opinion
//! about which acts are safe, and receives no command handler and no external
//! credential (spec §21.3).
//!
//! # The deadline
//!
//! Every call runs under `min(builder timeout, request timeout)`, enforced both
//! by the HTTP client and by an outer deadline, so a caller can always ask for
//! less time than the deployment allows and never for more. Dropping the future
//! cancels the call; nothing is left running behind it.
//!
//! # What never leaves
//!
//! The credential lives in an [`ApiKey`] and reaches the wire as the
//! `x-api-key` header value, marked sensitive. [`Debug`] renders a fingerprint
//! — a digest prefix that identifies *which* key is configured without
//! revealing it — and the header names, never their values. Every string this
//! adapter lifts off the wire into an error passes through a [`Redactor`]
//! seeded with the configured key first.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use turnframe_provider::capabilities::{MicroCents, ModelProfile, ProviderCapabilities};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::request::ModelRequest;
use turnframe_provider::response::ModelResponse;
use turnframe_provider::secret::{ApiKey, DefaultRedactor, Redactor};
use turnframe_provider::stream::ModelStream;

use crate::builder::{AnthropicProviderBuilder, IDEMPOTENCY_HEADER};
use crate::error::{ApiErrorEnvelope, classify, retry_after, transport};
use crate::profile::EndpointProfile;
use crate::wire::request::{MessagesRequest, build_request};
use crate::wire::response::{MessageResponse, build_response};
use crate::wire::stream::model_stream;

/// Everything [`AnthropicProviderBuilder::build`] assembled, handed over in one
/// piece so the provider has no public constructor of its own.
pub(crate) struct Parts {
    pub(crate) provider: ProviderKey,
    pub(crate) model: ModelKey,
    pub(crate) capabilities: ProviderCapabilities,
    pub(crate) profile: EndpointProfile,
    pub(crate) base_url: String,
    pub(crate) endpoint: String,
    pub(crate) api_key: Option<ApiKey>,
    pub(crate) headers: HeaderMap,
    pub(crate) client: reqwest::Client,
    pub(crate) timeout: Duration,
    pub(crate) redactor: DefaultRedactor,
    pub(crate) cost: Option<(MicroCents, MicroCents)>,
    pub(crate) region: Option<String>,
    pub(crate) tags: Vec<String>,
}

/// An Anthropic Messages API endpoint.
///
/// Build it with [`AnthropicProvider::anthropic`] or
/// [`AnthropicProvider::compatible`].
pub struct AnthropicProvider {
    provider: ProviderKey,
    model: ModelKey,
    capabilities: ProviderCapabilities,
    profile: EndpointProfile,
    base_url: String,
    endpoint: String,
    api_key: Option<ApiKey>,
    headers: HeaderMap,
    client: reqwest::Client,
    timeout: Duration,
    redactor: Arc<DefaultRedactor>,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl AnthropicProvider {
    /// A builder for `profile`.
    #[must_use]
    pub fn builder(profile: EndpointProfile) -> AnthropicProviderBuilder {
        AnthropicProviderBuilder::new(profile)
    }

    /// A builder for Anthropic's own API.
    #[must_use]
    pub fn anthropic() -> AnthropicProviderBuilder {
        AnthropicProviderBuilder::anthropic()
    }

    /// A builder for another endpoint that reimplements the Messages API.
    #[must_use]
    pub fn compatible(provider: impl Into<ProviderKey>) -> AnthropicProviderBuilder {
        AnthropicProviderBuilder::compatible(provider)
    }

    /// Assembles the provider. Crate-private: the builder is the only door.
    pub(crate) fn assemble(parts: Parts) -> Self {
        Self {
            provider: parts.provider,
            model: parts.model,
            capabilities: parts.capabilities,
            profile: parts.profile,
            base_url: parts.base_url,
            endpoint: parts.endpoint,
            api_key: parts.api_key,
            headers: parts.headers,
            client: parts.client,
            timeout: parts.timeout,
            redactor: Arc::new(parts.redactor),
            cost: parts.cost,
            region: parts.region,
            tags: parts.tags,
        }
    }

    /// The full Messages URL every request goes to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The endpoint root.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The endpoint profile in force.
    #[must_use]
    pub const fn endpoint_profile(&self) -> &EndpointProfile {
        &self.profile
    }

    /// The transport deadline configured on the builder.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// A short, non-reversible hint identifying which credential is configured,
    /// or `None` when the endpoint takes none.
    #[must_use]
    pub fn key_fingerprint(&self) -> Option<String> {
        self.api_key.as_ref().map(ApiKey::fingerprint)
    }

    /// The redactor every wire-borne string passes through before it can reach
    /// a log line or an error.
    #[must_use]
    pub fn redactor(&self) -> &dyn Redactor {
        self.redactor.as_ref()
    }

    /// The headers sent with every request, for the builder's own tests.
    #[cfg(test)]
    pub(crate) const fn headers_for_test(&self) -> &HeaderMap {
        &self.headers
    }

    /// The effective deadline: the smaller of the two.
    fn deadline(&self, request: &ModelRequest) -> Duration {
        self.timeout.min(request.timeout)
    }

    /// Labels an error with this provider-model pair.
    fn label(&self, error: ProviderError) -> ProviderError {
        error.with_model(&self.reference())
    }

    /// Sends one request and returns the response, or the classified failure.
    ///
    /// Success means a status below 400; everything else is read, classified
    /// and discarded here, so no body travels further into the process.
    async fn dispatch(
        &self,
        request: &ModelRequest,
        body: &MessagesRequest,
        deadline: Duration,
    ) -> Result<reqwest::Response, ProviderError> {
        let builder = self
            .client
            .post(&self.endpoint)
            .headers(self.headers.clone())
            // The stable request id is the idempotency hint (spec §20.7): a
            // retry of the same logical call carries the same one.
            .header(IDEMPOTENCY_HEADER, request.request_id.to_string())
            .timeout(deadline)
            .json(body);
        let sent = match tokio::time::timeout(deadline, builder.send()).await {
            Err(_elapsed) => return Err(self.label(ProviderError::timeout())),
            Ok(Err(error)) => return Err(self.label(transport(&error))),
            Ok(Ok(sent)) => sent,
        };
        let status = sent.status().as_u16();
        if status < 400 {
            return Ok(sent);
        }
        let hint = retry_after(sent.headers());
        let envelope = match tokio::time::timeout(deadline, sent.text()).await {
            Ok(Ok(text)) => ApiErrorEnvelope::decode(&text),
            // The status is the classification; a body we could not read only
            // ever refines it.
            Ok(Err(_)) | Err(_) => ApiErrorEnvelope::default(),
        };
        Err(self.label(classify(status, hint, &envelope, self.redactor.as_ref())))
    }
}

impl fmt::Debug for AnthropicProvider {
    /// Renders configuration, never credentials: header **names**, and a
    /// fingerprint of the key rather than the key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header_names: Vec<&str> = self
            .headers
            .keys()
            .map(reqwest::header::HeaderName::as_str)
            .collect();
        f.debug_struct("AnthropicProvider")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("api_version", &self.profile.api_version())
            .field("auth_header", &self.profile.auth().header_name())
            .field("key_fingerprint", &self.key_fingerprint())
            .field("headers", &header_names)
            .field("timeout", &self.timeout)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
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
        let converted = build_request(
            &request,
            self.model.as_str(),
            &self.capabilities,
            self.profile.quirks(),
            false,
        )
        .map_err(|error| self.label(error))?;

        let sent = self.dispatch(&request, &converted.body, deadline).await?;
        let raw = match tokio::time::timeout(deadline, sent.text()).await {
            Err(_elapsed) => return Err(self.label(ProviderError::timeout())),
            Ok(Err(error)) => return Err(self.label(transport(&error))),
            Ok(Ok(raw)) => raw,
        };
        if raw.trim().is_empty() {
            return Err(self.label(ProviderError::malformed("empty_body")));
        }
        let body: MessageResponse = serde_json::from_str(&raw)
            .map_err(|_| self.label(ProviderError::malformed("body_not_json")))?;

        let mut response = build_response(
            &body,
            request.request_id,
            &self.provider,
            &self.model,
            self.capabilities.preserves_call_ids,
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
        let converted = build_request(
            &request,
            self.model.as_str(),
            &self.capabilities,
            self.profile.quirks(),
            true,
        )
        .map_err(|error| self.label(error))?;
        let sent = self.dispatch(&request, &converted.body, deadline).await?;
        Ok(model_stream(
            sent,
            self.reference(),
            Box::new(self.redactor.as_ref().clone()),
            converted.warnings,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_provider::capabilities::StructuredOutputCapability;
    use turnframe_provider::purpose::ModelPurpose;
    use turnframe_provider::request::Message;

    const PLANTED: &str = "sk-ant-planted-0123456789abcdefghij";

    fn provider() -> AnthropicProvider {
        AnthropicProvider::anthropic()
            .api_key(ApiKey::new(PLANTED))
            .model("claude-sonnet-4-5-20250929")
            .header("x-gateway-route", "eu")
            .cost(MicroCents::from_cents(300), MicroCents::from_cents(1500))
            .region("eu")
            .tag("critical")
            .build()
            .expect("builds")
    }

    #[test]
    fn the_key_appears_in_no_rendering_of_the_adapter() {
        let provider = provider();
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("sk-ant-planted"), "{rendered}");
        assert!(!rendered.contains(PLANTED), "{rendered}");
        // The headers are there by name, so an operator can see what is sent.
        assert!(rendered.contains("x-api-key"), "{rendered}");
        assert!(rendered.contains("anthropic-version"), "{rendered}");
        // And the key is identifiable without being readable.
        let fingerprint = provider.key_fingerprint().expect("a key is configured");
        assert_eq!(fingerprint.len(), 8);
        assert!(rendered.contains(&fingerprint), "{rendered}");
        assert!(!PLANTED.contains(&fingerprint));
        // Even the raw header map keeps its secret.
        assert!(!format!("{:?}", provider.headers).contains("sk-ant-planted"));
        // And so does the redactor the adapter hands out.
        assert_eq!(
            provider.redactor().redact(&format!("x-api-key: {PLANTED}")),
            "x-api-key: [REDACTED]"
        );
    }

    #[test]
    fn the_routing_profile_carries_cost_region_and_tags() {
        let profile = provider().profile();
        assert_eq!(
            profile.max_cost_per_million(),
            Some(MicroCents::from_cents(1500))
        );
        assert_eq!(profile.region.as_deref(), Some("eu"));
        assert_eq!(profile.tags, vec!["critical".to_owned()]);
        assert_eq!(
            profile.reference().to_string(),
            "anthropic/claude-sonnet-4-5-20250929"
        );
    }

    #[test]
    fn the_effective_deadline_is_the_smaller_of_the_two() {
        let provider = AnthropicProvider::anthropic()
            .api_key(ApiKey::new(PLANTED))
            .model("claude-x")
            .timeout(Duration::from_secs(5))
            .build()
            .expect("builds");
        let patient =
            ModelRequest::new(ModelPurpose::Acknowledge).with_timeout(Duration::from_secs(30));
        assert_eq!(provider.deadline(&patient), Duration::from_secs(5));
        let hurried =
            ModelRequest::new(ModelPurpose::Acknowledge).with_timeout(Duration::from_secs(1));
        assert_eq!(provider.deadline(&hurried), Duration::from_secs(1));
        assert_eq!(provider.timeout(), Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_profile_without_streaming_refuses_rather_than_faking_it() {
        let provider = AnthropicProvider::anthropic()
            .api_key(ApiKey::new(PLANTED))
            .model("claude-x")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::PromptOnly)
                    .with_streaming(false),
            )
            .build()
            .expect("builds");
        let error = provider
            .stream(
                ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("ciao")),
            )
            .await
            .expect_err("refused");
        assert!(
            error.to_string().contains("unsupported(streaming)"),
            "{error}"
        );
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fallback
        );
    }

    #[test]
    fn the_declared_capabilities_are_what_supports_answers_with() {
        let provider = provider();
        let mutation = ModelPurpose::Extract.requirements();
        assert!(
            provider.supports(&mutation).is_ok(),
            "a forced tool is an admitted transport for an understanding task"
        );
        assert_eq!(
            provider.endpoint_profile().api_version(),
            crate::profile::DEFAULT_ANTHROPIC_VERSION
        );

        let weak = AnthropicProvider::anthropic()
            .api_key(ApiKey::new(PLANTED))
            .model("claude-x")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::PromptOnly),
            )
            .build()
            .expect("builds");
        let mismatch = weak
            .supports(&mutation)
            .expect_err("prompt_only is not enough");
        assert!(mismatch.structured_output_unmet());
    }
}
