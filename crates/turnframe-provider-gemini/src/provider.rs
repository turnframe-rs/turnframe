//! The adapter itself.
//!
//! One [`GeminiProvider`] is one endpoint, one credential, one model and one
//! honest capability declaration (spec §20.1). It translates and nothing else:
//! it holds no workflow policy, never sees a workflow view, has no opinion
//! about which acts are safe, and receives no command handler and no external
//! credential (spec §21.3).
//!
//! # The deadline
//!
//! Every call runs under `min(builder timeout, request timeout)`, enforced both
//! by the HTTP client and by an outer deadline, so a caller can always ask for
//! less time than the deployment allows and never for more. The Vertex token
//! fetch runs inside that budget too, because a credential broker that hangs is
//! a call that hangs. Dropping the future cancels everything; nothing is left
//! running behind it.
//!
//! # What never leaves
//!
//! The developer API's key lives in an [`ApiKey`] and reaches the wire as a
//! header value marked sensitive. Vertex's bearer token is never stored at all:
//! it is fetched from the [`TokenSource`] immediately before dispatch, put into
//! one request, and dropped. [`Debug`] renders a fingerprint of whichever
//! credential is configured — a digest prefix identifying *which* one without
//! revealing it — plus the header names, never their values. Every string this
//! adapter lifts off the wire into an error passes through a [`Redactor`]
//! seeded with the credential first, including the token that was only alive
//! for one call.
//!
//! # No idempotency key
//!
//! [`ModelRequest::request_id`] is meant to travel as an idempotency hint
//! "where the vendor supports one" (spec §20.7). Neither surface does:
//! `generateContent` has no such header and no request-id field. The id still
//! labels every attempt record on this side, so a replay can correlate them; it
//! simply cannot help the service deduplicate. Since a model call commits no
//! effect, that costs a duplicate generation at worst.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use turnframe_provider::capabilities::{MicroCents, ModelProfile, ProviderCapabilities};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::request::ModelRequest;
use turnframe_provider::response::ModelResponse;
use turnframe_provider::secret::{ApiKey, DefaultRedactor, Redactor};
use turnframe_provider::stream::ModelStream;

use crate::builder::GeminiProviderBuilder;
use crate::credential::TokenSource;
use crate::error::{ApiErrorEnvelope, ResponseHints, classify, transport};
use crate::profile::{AuthScheme, EndpointProfile, SafetySetting};
use crate::wire::request::{GenerateContentRequest, RequestOptions, build_request};
use crate::wire::response::{GenerateContentResponse, build_response};
use crate::wire::stream::model_stream;

/// Everything [`GeminiProviderBuilder::build`] assembled, handed over in one
/// piece so the provider has no public constructor of its own.
pub(crate) struct Parts {
    pub(crate) provider: ProviderKey,
    pub(crate) model: ModelKey,
    pub(crate) capabilities: ProviderCapabilities,
    pub(crate) profile: EndpointProfile,
    pub(crate) base_url: String,
    pub(crate) endpoint: String,
    pub(crate) stream_endpoint: String,
    pub(crate) api_key: Option<ApiKey>,
    pub(crate) token_source: Option<Arc<dyn TokenSource>>,
    pub(crate) headers: HeaderMap,
    pub(crate) client: reqwest::Client,
    pub(crate) timeout: Duration,
    pub(crate) redactor: DefaultRedactor,
    pub(crate) safety_settings: Vec<SafetySetting>,
    pub(crate) thinking_budget: Option<i32>,
    pub(crate) cost: Option<(MicroCents, MicroCents)>,
    pub(crate) region: Option<String>,
    pub(crate) tags: Vec<String>,
}

/// A Gemini developer API or Vertex AI endpoint.
///
/// Build it with [`GeminiProvider::gemini`] or
/// [`GeminiProvider::vertex_ai`].
pub struct GeminiProvider {
    provider: ProviderKey,
    model: ModelKey,
    capabilities: ProviderCapabilities,
    profile: EndpointProfile,
    base_url: String,
    endpoint: String,
    stream_endpoint: String,
    api_key: Option<ApiKey>,
    token_source: Option<Arc<dyn TokenSource>>,
    headers: HeaderMap,
    client: reqwest::Client,
    timeout: Duration,
    redactor: Arc<DefaultRedactor>,
    safety_settings: Vec<SafetySetting>,
    thinking_budget: Option<i32>,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl GeminiProvider {
    /// A builder for `profile`.
    #[must_use]
    pub fn builder(profile: EndpointProfile) -> GeminiProviderBuilder {
        GeminiProviderBuilder::new(profile)
    }

    /// A builder for the Gemini developer API.
    #[must_use]
    pub fn gemini() -> GeminiProviderBuilder {
        GeminiProviderBuilder::gemini()
    }

    /// A builder for Vertex AI, in one project and one region.
    #[must_use]
    pub fn vertex_ai(
        project: impl Into<String>,
        location: impl Into<String>,
    ) -> GeminiProviderBuilder {
        GeminiProviderBuilder::vertex_ai(project, location)
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
            stream_endpoint: parts.stream_endpoint,
            api_key: parts.api_key,
            token_source: parts.token_source,
            headers: parts.headers,
            client: parts.client,
            timeout: parts.timeout,
            redactor: Arc::new(parts.redactor),
            safety_settings: parts.safety_settings,
            thinking_budget: parts.thinking_budget,
            cost: parts.cost,
            region: parts.region,
            tags: parts.tags,
        }
    }

    /// The URL a whole-answer call goes to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The URL a streamed call goes to.
    #[must_use]
    pub fn stream_endpoint(&self) -> &str {
        &self.stream_endpoint
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

    /// A short, non-reversible hint identifying which credential is configured.
    ///
    /// For the developer API this is the API key's fingerprint. For Vertex it
    /// is `None` while no token has been fetched: the adapter holds a token
    /// *source*, not a token.
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

    /// The conversion options this adapter's configuration supplies.
    fn options(&self) -> RequestOptions<'_> {
        RequestOptions {
            capabilities: &self.capabilities,
            quirks: self.profile.quirks(),
            safety_settings: &self.safety_settings,
            thinking_budget: self.thinking_budget,
        }
    }

    /// Fetches the per-call credential, when the profile needs one.
    ///
    /// Returns the header to add and a redactor that also masks the token, so a
    /// token that lives for one request is still masked in anything that
    /// request produces.
    async fn per_call_auth(
        &self,
        deadline: Duration,
    ) -> Result<(Option<HeaderValue>, Arc<dyn Redactor>), ProviderError> {
        let Some(source) = self.token_source.as_ref() else {
            return Ok((None, self.redactor.clone()));
        };
        let token = match tokio::time::timeout(deadline, source.access_token()).await {
            Err(_elapsed) => return Err(self.label(ProviderError::timeout())),
            Ok(Ok(token)) => token,
            Ok(Err(error)) => return Err(self.label(ProviderError::from(error))),
        };
        let mut value = HeaderValue::try_from(AuthScheme::OAuthBearer.header_value(token.expose()))
            .map_err(|_| self.label(ProviderError::authentication().with_code("invalid_token")))?;
        value.set_sensitive(true);
        let redactor = self.redactor.as_ref().clone().with_secret(&token);
        Ok((Some(value), Arc::new(redactor)))
    }

    /// Sends one request and returns the response, or the classified failure.
    ///
    /// Success means a status below 400; everything else is read, classified
    /// and discarded here, so no body travels further into the process.
    async fn dispatch(
        &self,
        url: &str,
        body: &GenerateContentRequest,
        deadline: Duration,
        auth: Option<HeaderValue>,
        redactor: &dyn Redactor,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut builder = self
            .client
            .post(url)
            .headers(self.headers.clone())
            .timeout(deadline)
            .json(body);
        if let Some(auth) = auth {
            builder = builder.header(AuthScheme::OAuthBearer.header_name(), auth);
        }
        let sent = match tokio::time::timeout(deadline, builder.send()).await {
            Err(_elapsed) => return Err(self.label(ProviderError::timeout())),
            Ok(Err(error)) => return Err(self.label(transport(&error))),
            Ok(Ok(sent)) => sent,
        };
        let status = sent.status().as_u16();
        if status < 400 {
            return Ok(sent);
        }
        // The hints are copied out of the headers before the body is read,
        // because reading it consumes the response.
        let borrowed = ResponseHints::from_headers(sent.headers());
        let retry_after = borrowed.retry_after;
        let challenge = borrowed.www_authenticate.map(str::to_owned);
        let envelope = match tokio::time::timeout(deadline, sent.text()).await {
            Ok(Ok(text)) => ApiErrorEnvelope::decode(&text),
            // The status is the classification; a body we could not read only
            // ever refines it.
            Ok(Err(_)) | Err(_) => ApiErrorEnvelope::default(),
        };
        let hints = ResponseHints {
            retry_after,
            www_authenticate: challenge.as_deref(),
        };
        Err(self.label(classify(status, &hints, &envelope, redactor)))
    }
}

impl fmt::Debug for GeminiProvider {
    /// Renders configuration, never credentials: header **names**, the token
    /// source's own `Debug` (which is expected to redact), and a fingerprint of
    /// the API key rather than the key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header_names: Vec<&str> = self
            .headers
            .keys()
            .map(reqwest::header::HeaderName::as_str)
            .collect();
        f.debug_struct("GeminiProvider")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("auth_header", &self.profile.auth().header_name())
            .field("key_fingerprint", &self.key_fingerprint())
            .field("token_source", &self.token_source)
            .field("headers", &header_names)
            .field("timeout", &self.timeout)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModelProvider for GeminiProvider {
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
        let converted =
            build_request(&request, self.options()).map_err(|error| self.label(error))?;
        let (auth, redactor) = self.per_call_auth(deadline).await?;

        let sent = self
            .dispatch(
                &self.endpoint,
                &converted.body,
                deadline,
                auth,
                redactor.as_ref(),
            )
            .await?;
        let raw = match tokio::time::timeout(deadline, sent.text()).await {
            Err(_elapsed) => return Err(self.label(ProviderError::timeout())),
            Ok(Err(error)) => return Err(self.label(transport(&error))),
            Ok(Ok(raw)) => raw,
        };
        if raw.trim().is_empty() {
            return Err(self.label(ProviderError::malformed("empty_body")));
        }
        let body: GenerateContentResponse = serde_json::from_str(&raw)
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
        let converted =
            build_request(&request, self.options()).map_err(|error| self.label(error))?;
        let (auth, redactor) = self.per_call_auth(deadline).await?;
        let sent = self
            .dispatch(
                &self.stream_endpoint,
                &converted.body,
                deadline,
                auth,
                redactor.as_ref(),
            )
            .await?;
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

    const PLANTED_KEY: &str = "AIzaSyPlanted0123456789abcdefgh";
    const PLANTED_TOKEN: &str = "ya29.planted-0123456789abcdefghij";

    fn provider() -> GeminiProvider {
        GeminiProvider::gemini()
            .api_key(ApiKey::new(PLANTED_KEY))
            .model("gemini-2.5-flash")
            .quota_project("aurora-billing")
            .header("x-goog-request-reason", "audit")
            .cost(MicroCents::from_cents(30), MicroCents::from_cents(250))
            .tag("critical")
            .build()
            .expect("builds")
    }

    #[test]
    fn no_rendering_of_the_adapter_contains_a_credential() {
        let provider = provider();
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("AIzaSyPlanted"), "{rendered}");
        assert!(!rendered.contains(PLANTED_KEY), "{rendered}");
        // The headers are there by name, so an operator can see what is sent.
        assert!(rendered.contains("x-goog-api-key"), "{rendered}");
        assert!(rendered.contains("x-goog-user-project"), "{rendered}");
        // And the key is identifiable without being readable.
        let fingerprint = provider.key_fingerprint().expect("a key is configured");
        assert_eq!(fingerprint.len(), 8);
        assert!(rendered.contains(&fingerprint), "{rendered}");
        assert!(!PLANTED_KEY.contains(&fingerprint));
        // Even the raw header map keeps its secret.
        assert!(!format!("{:?}", provider.headers).contains("AIzaSyPlanted"));
    }

    #[test]
    fn a_vertex_adapter_holds_a_token_source_and_not_a_token() {
        let provider = GeminiProvider::vertex_ai("aurora", "europe-west4")
            .access_token(ApiKey::new(PLANTED_TOKEN))
            .model("gemini-2.5-flash")
            .build()
            .expect("builds");
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("ya29.planted"), "{rendered}");
        assert!(rendered.contains("StaticToken"), "{rendered}");
        // There is no configured key to fingerprint: the credential is fetched.
        assert!(provider.key_fingerprint().is_none());
        assert!(rendered.contains("authorization"), "{rendered}");
    }

    #[test]
    fn the_routing_profile_carries_cost_region_and_tags() {
        let profile = provider().profile();
        assert_eq!(
            profile.max_cost_per_million(),
            Some(MicroCents::from_cents(250))
        );
        assert_eq!(profile.tags, vec!["critical".to_owned()]);
        assert_eq!(profile.reference().to_string(), "gemini/gemini-2.5-flash");
        // The developer API has no region of its own to declare.
        assert!(profile.region.is_none());
    }

    #[test]
    fn the_effective_deadline_is_the_smaller_of_the_two() {
        let provider = GeminiProvider::gemini()
            .api_key(ApiKey::new(PLANTED_KEY))
            .model("gemini-2.5-flash")
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
        let provider = GeminiProvider::gemini()
            .api_key(ApiKey::new(PLANTED_KEY))
            .model("gemini-2.5-flash")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::JsonObject)
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

    #[tokio::test]
    async fn a_token_source_that_cannot_answer_fails_the_call_before_it_is_sent() {
        use crate::credential::{TokenError, TokenFn};
        let provider = GeminiProvider::vertex_ai("aurora", "global")
            .token_source(Arc::new(TokenFn::new(|| async {
                Err::<ApiKey, _>(TokenError::failed("metadata_server_timeout"))
            })))
            .model("gemini-2.5-flash")
            .build()
            .expect("builds");
        let error = provider
            .generate(
                ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("ciao")),
            )
            .await
            .expect_err("no token");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("metadata_server_timeout".to_owned())
        );
        assert_eq!(error.provider().map(ProviderKey::as_str), Some("vertex-ai"));
    }
}
