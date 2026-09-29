//! The adapter itself.
//!
//! One [`OllamaProvider`] is one daemon, one model and one honest capability
//! declaration (spec §20.1). It translates and nothing else: it holds no
//! workflow policy, never sees a workflow view, has no opinion about which acts
//! are safe, and receives no command handler and no external credential
//! (spec §21.3).
//!
//! # The deadline
//!
//! Every call runs under `min(builder timeout, request timeout)`, enforced both
//! by the HTTP client and by an outer deadline, so a caller can always ask for
//! less time than the deployment allows and never for more. Dropping the future
//! cancels the call; nothing is left running behind it.
//!
//! A local daemon has a failure mode a hosted API does not: the **first** call
//! for a model loads several gigabytes off disk, and the answer starts only
//! after that. A deadline tuned for a warm cloud endpoint will time out on a
//! cold local one, which is a configuration problem rather than a defect, and
//! [`keep_alive`](crate::OllamaProviderBuilder::keep_alive) is how it is
//! avoided.
//!
//! # What never leaves
//!
//! Where a credential is configured at all it lives in an [`ApiKey`] and
//! reaches the wire as a header value marked sensitive. [`Debug`] renders a
//! fingerprint — a digest prefix that identifies *which* token is configured
//! without revealing it — and the header names, never their values. Every
//! string this adapter lifts off the wire into an error passes through a
//! [`Redactor`] seeded with the configured token first.

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

use crate::builder::OllamaProviderBuilder;
use crate::error::{ApiErrorEnvelope, classify, retry_after, transport};
use crate::wire::request::{ChatRequest, WireSettings, build_request};
use crate::wire::response::{ChatResponse, build_response};
use crate::wire::stream::model_stream;

/// Everything [`OllamaProviderBuilder::build`] assembled, handed over in one
/// piece so the provider has no public constructor of its own.
pub(crate) struct Parts {
    pub(crate) provider: ProviderKey,
    pub(crate) model: ModelKey,
    pub(crate) capabilities: ProviderCapabilities,
    pub(crate) base_url: String,
    pub(crate) endpoint: String,
    pub(crate) api_key: Option<ApiKey>,
    pub(crate) headers: HeaderMap,
    pub(crate) client: reqwest::Client,
    pub(crate) timeout: Duration,
    pub(crate) redactor: DefaultRedactor,
    pub(crate) settings: WireSettings,
    pub(crate) cost: Option<(MicroCents, MicroCents)>,
    pub(crate) region: Option<String>,
    pub(crate) tags: Vec<String>,
}

/// An Ollama daemon's native chat endpoint, for one model.
///
/// Build it with [`OllamaProvider::local`], [`OllamaProvider::at`] or
/// [`OllamaProvider::builder`].
pub struct OllamaProvider {
    provider: ProviderKey,
    model: ModelKey,
    capabilities: ProviderCapabilities,
    base_url: String,
    endpoint: String,
    api_key: Option<ApiKey>,
    headers: HeaderMap,
    client: reqwest::Client,
    timeout: Duration,
    redactor: Arc<DefaultRedactor>,
    settings: WireSettings,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl OllamaProvider {
    /// A builder for the daemon at `base_url`.
    #[must_use]
    pub fn builder(base_url: impl Into<String>) -> OllamaProviderBuilder {
        OllamaProviderBuilder::new(base_url)
    }

    /// A builder for the daemon at `base_url`. Reads better at a call site.
    #[must_use]
    pub fn at(base_url: impl Into<String>) -> OllamaProviderBuilder {
        OllamaProviderBuilder::new(base_url)
    }

    /// A builder for the daemon on this machine.
    #[must_use]
    pub fn local() -> OllamaProviderBuilder {
        OllamaProviderBuilder::local()
    }

    /// Assembles the provider. Crate-private: the builder is the only door.
    pub(crate) fn assemble(parts: Parts) -> Self {
        Self {
            provider: parts.provider,
            model: parts.model,
            capabilities: parts.capabilities,
            base_url: parts.base_url,
            endpoint: parts.endpoint,
            api_key: parts.api_key,
            headers: parts.headers,
            client: parts.client,
            timeout: parts.timeout,
            redactor: Arc::new(parts.redactor),
            settings: parts.settings,
            cost: parts.cost,
            region: parts.region,
            tags: parts.tags,
        }
    }

    /// The full `/api/chat` URL every request goes to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The endpoint root.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The transport deadline configured on the builder.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// A short, non-reversible hint identifying which bearer token is
    /// configured, or `None` for the ordinary local daemon, which takes none.
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
        body: &ChatRequest,
        deadline: Duration,
    ) -> Result<reqwest::Response, ProviderError> {
        let builder = self
            .client
            .post(&self.endpoint)
            .headers(self.headers.clone())
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

impl fmt::Debug for OllamaProvider {
    /// Renders configuration, never credentials: header **names**, and a
    /// fingerprint of the token rather than the token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header_names: Vec<&str> = self
            .headers
            .keys()
            .map(reqwest::header::HeaderName::as_str)
            .collect();
        f.debug_struct("OllamaProvider")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("key_fingerprint", &self.key_fingerprint())
            .field("headers", &header_names)
            .field("timeout", &self.timeout)
            .field("settings", &self.settings)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
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

    /// Runs one call whole.
    ///
    /// `/api/chat` has no idempotency key and no request-id header, so the
    /// call's stable [`request_id`](ModelRequest::request_id) travels no
    /// further than this process. Inventing a header the daemon ignores would
    /// look like deduplication and provide none.
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        let started = Instant::now();
        let deadline = self.deadline(&request);
        let converted = build_request(
            &request,
            self.model.as_str(),
            &self.capabilities,
            &self.settings,
            false,
        )
        .map_err(|error| self.label(error))?;

        let sent = self.dispatch(&converted.body, deadline).await?;
        let raw = match tokio::time::timeout(deadline, sent.text()).await {
            Err(_elapsed) => return Err(self.label(ProviderError::timeout())),
            Ok(Err(error)) => return Err(self.label(transport(&error))),
            Ok(Ok(raw)) => raw,
        };
        if raw.trim().is_empty() {
            return Err(self.label(ProviderError::malformed("empty_body")));
        }
        let body: ChatResponse = serde_json::from_str(&raw)
            .map_err(|_| self.label(ProviderError::malformed("body_not_json")))?;
        // A daemon that fails after the headers are out reports it inside a
        // successful body rather than by changing the status.
        if let Some(reported) = &body.error {
            return Err(self.label(crate::error::classify_stream(
                reported,
                self.redactor.as_ref(),
            )));
        }

        let mut response = build_response(&body, request.request_id, &self.provider, &self.model)
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
            &self.settings,
            true,
        )
        .map_err(|error| self.label(error))?;
        let sent = self.dispatch(&converted.body, deadline).await?;
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

    use crate::declarations::baseline;

    const PLANTED: &str = "tok-planted-0123456789abcdefghijklmnop";

    fn provider() -> OllamaProvider {
        OllamaProvider::at("https://ollama.internal")
            .bearer_token(ApiKey::new(PLANTED))
            .model("qwen3:8b")
            .header("x-gateway-route", "eu")
            .cost(MicroCents::from_cents(1), MicroCents::from_cents(2))
            .region("on-premise")
            .tag("local")
            .build()
            .expect("builds")
    }

    #[test]
    fn the_token_appears_in_no_rendering_of_the_adapter() {
        let provider = provider();
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("tok-planted"), "{rendered}");
        assert!(!rendered.contains(PLANTED), "{rendered}");
        // The headers are there by name, so an operator can see what is sent.
        assert!(rendered.contains("authorization"), "{rendered}");
        assert!(rendered.contains("x-gateway-route"), "{rendered}");
        // Even the raw header map keeps its secret.
        assert!(!format!("{:?}", provider.headers).contains("tok-planted"));
    }

    #[test]
    fn a_local_profile_renders_no_credential_at_all() {
        let local = OllamaProvider::local()
            .model("qwen3:8b")
            .build()
            .expect("builds");
        let rendered = format!("{local:?}");
        assert!(rendered.contains("key_fingerprint: None"), "{rendered}");
        assert!(local.headers.is_empty());
    }

    #[test]
    fn the_routing_profile_carries_cost_region_and_tags() {
        let profile = provider().profile();
        assert_eq!(
            profile.max_cost_per_million(),
            Some(MicroCents::from_cents(2))
        );
        assert_eq!(profile.region.as_deref(), Some("on-premise"));
        assert_eq!(profile.tags, vec!["local".to_owned()]);
        assert_eq!(profile.reference().to_string(), "ollama/qwen3:8b");
    }

    #[test]
    fn the_effective_deadline_is_the_smaller_of_the_two() {
        let provider = OllamaProvider::local()
            .model("qwen3:8b")
            .timeout(Duration::from_secs(5))
            .build()
            .expect("builds");
        let patient =
            ModelRequest::new(ModelPurpose::Acknowledge).with_timeout(Duration::from_secs(30));
        assert_eq!(provider.deadline(&patient), Duration::from_secs(5));
        let hurried =
            ModelRequest::new(ModelPurpose::Acknowledge).with_timeout(Duration::from_secs(1));
        assert_eq!(provider.deadline(&hurried), Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_profile_without_streaming_refuses_rather_than_faking_it() {
        let provider = OllamaProvider::local()
            .model("qwen3:8b")
            .capabilities(baseline().with_streaming(false))
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

    #[tokio::test]
    async fn a_daemon_that_is_not_listening_is_a_transport_failure_that_names_the_cause() {
        // Bind a port and let it go, so nothing is listening on it.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("binds");
        let address = listener.local_addr().expect("an address");
        drop(listener);

        let provider = OllamaProvider::at(format!("http://{address}"))
            .model("qwen3:8b")
            .build()
            .expect("builds");
        let error = provider
            .generate(
                ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("ciao")),
            )
            .await
            .expect_err("nothing is listening");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some(crate::DAEMON_UNREACHABLE_CODE.to_owned())
        );
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Retry
        );
        // The message an operator reads names the fix rather than the socket.
        assert!(
            error.to_string().contains("is_the_daemon_running"),
            "{error}"
        );
    }

    #[test]
    fn the_declared_capabilities_are_what_supports_answers_with() {
        let measured = OllamaProvider::local()
            .model("qwen3:8b")
            .capabilities(
                baseline().with_structured_output(StructuredOutputCapability::NativeJsonSchema),
            )
            .build()
            .expect("builds");
        assert!(
            measured
                .supports(&ModelPurpose::Extract.requirements())
                .is_ok()
        );
        assert!(
            !provider()
                .capabilities()
                .structured_output
                .enforces_schema()
        );
    }
}
