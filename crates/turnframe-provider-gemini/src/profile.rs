//! The two endpoint profiles this adapter speaks (spec §20.3, §20.5).
//!
//! Gemini reaches the same models through two surfaces that share a request
//! body and agree on nothing else:
//!
//! | | Gemini developer API | Vertex AI |
//! |---|---|---|
//! | Host | `generativelanguage.googleapis.com` | `{location}-aiplatform.googleapis.com` |
//! | Path | `/v1beta/models/{model}:generateContent` | `/v1/projects/{p}/locations/{l}/publishers/google/models/{m}:generateContent` |
//! | Credential | a long-lived API key in `x-goog-api-key` | a short-lived OAuth token in `Authorization: Bearer` |
//! | Where the credential comes from | configuration | a [`TokenSource`](crate::credential::TokenSource), refreshed per call |
//! | `labels` | not accepted | accepted |
//!
//! An [`EndpointProfile`] answers exactly those questions, plus two more:
//! what this provider-model pair can actually do
//! ([`EndpointProfile::capabilities`], which is configuration and never an
//! inference from the brand), and what the surface gets wrong ([`Quirks`]).
//!
//! # Two structured-output transports, and a profile picks one
//!
//! Both surfaces can enforce a schema two ways, and the choice is a
//! [`ProviderCapabilities::structured_output`] declaration rather than a
//! per-request flag:
//!
//! * [`NativeJsonSchema`](StructuredOutputCapability::NativeJsonSchema) — what
//!   both profiles declare by default — sends
//!   `generationConfig.responseSchema`;
//! * [`NativeFunctionSchema`](StructuredOutputCapability::NativeFunctionSchema)
//!   sends one function declaration whose `parameters` is the schema, with
//!   `toolConfig.functionCallingConfig` pinned to `ANY` and that single name.
//!
//! [`EndpointProfile::max_structured_output`] admits both on both surfaces, so
//! a profile declares the function form simply by handing
//! [`EndpointProfile::with_capabilities`] a declaration that names it — together
//! with a [`ToolCallingCapability`] other than `None`, because the transport
//! *is* a function call and the builder refuses the contradiction. Reach for it
//! on a surface whose [`Quirks::structured_output_with_tools`] is `false`, where
//! a response schema and `tools` cannot travel together but a forced function
//! can.
//!
//! # Both profiles are one adapter, and neither is a certificate
//!
//! Conformance is per provider **and model**. A passing run against
//! `gemini-2.5-flash` on the developer API says nothing about the same model
//! id on Vertex in a different region, and nothing at all about
//! `gemini-2.5-pro`. The profile gets you to a first request; the conformance
//! suite is what licenses the declaration.
//!
//! ```
//! use turnframe_provider_gemini::profile::EndpointProfile;
//!
//! let developer = EndpointProfile::gemini();
//! assert_eq!(developer.provider().as_str(), "gemini");
//! assert!(developer.capabilities().structured_output.enforces_schema());
//!
//! let vertex = EndpointProfile::vertex_ai("my-project", "europe-west4");
//! assert_eq!(vertex.provider().as_str(), "vertex-ai");
//! assert_eq!(
//!     vertex.default_base_url(),
//!     Some("https://europe-west4-aiplatform.googleapis.com/v1")
//! );
//! ```

use std::fmt;

use serde::Serialize;
use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::ids::ProviderKey;

use crate::schema::SchemaDialect;

/// Provider key of the Gemini developer API.
pub const GEMINI_PROVIDER_KEY: &str = "gemini";

/// Provider key of Vertex AI.
pub const VERTEX_PROVIDER_KEY: &str = "vertex-ai";

/// Base URL of the Gemini developer API, including its API version.
pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Header the developer API takes its API key in.
pub const API_KEY_HEADER: &str = "x-goog-api-key";

/// Publisher every first-party Google model lives under on Vertex.
pub const VERTEX_PUBLISHER: &str = "google";

/// The Vertex location that routes to the multi-region endpoint.
pub const VERTEX_GLOBAL_LOCATION: &str = "global";

/// Method suffix of a whole-answer call.
pub const GENERATE_CONTENT: &str = "generateContent";

/// Method suffix of a streamed call.
pub const STREAM_GENERATE_CONTENT: &str = "streamGenerateContent";

/// Stop sequences Gemini accepts. Extra ones are dropped with a warning.
pub const MAX_STOP_SEQUENCES: usize = 5;

/// How the credential is presented.
///
/// The value itself never leaves an
/// [`ApiKey`](turnframe_provider::secret::ApiKey) except inside the header this
/// scheme builds, which is why the method that renders that value is
/// crate-private and [`AuthScheme::header_name`] is not (spec §25.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthScheme {
    /// `x-goog-api-key: <key>` — the developer API's long-lived key.
    ApiKeyHeader,
    /// `Authorization: Bearer <token>` — Vertex AI's short-lived OAuth token,
    /// fetched per call from a [`TokenSource`](crate::credential::TokenSource).
    OAuthBearer,
}

impl AuthScheme {
    /// The header this scheme writes.
    ///
    /// ```
    /// use turnframe_provider_gemini::profile::AuthScheme;
    ///
    /// assert_eq!(AuthScheme::ApiKeyHeader.header_name(), "x-goog-api-key");
    /// assert_eq!(AuthScheme::OAuthBearer.header_name(), "authorization");
    /// ```
    #[must_use]
    pub const fn header_name(self) -> &'static str {
        match self {
            Self::ApiKeyHeader => API_KEY_HEADER,
            Self::OAuthBearer => "authorization",
        }
    }

    /// Returns `true` when the credential is fetched per call rather than
    /// configured once.
    #[must_use]
    pub const fn is_oauth(self) -> bool {
        matches!(self, Self::OAuthBearer)
    }

    /// Builds the header value. Crate-private: the returned `String` holds the
    /// credential and exists only long enough to become a header.
    pub(crate) fn header_value(self, secret: &str) -> String {
        match self {
            Self::ApiKeyHeader => secret.to_owned(),
            Self::OAuthBearer => format!("Bearer {secret}"),
        }
    }

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKeyHeader => "api_key_header",
            Self::OAuthBearer => "oauth_bearer",
        }
    }
}

impl fmt::Display for AuthScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the request URL is built.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RouteShape {
    /// `{base}/models/{model}:{method}` — the developer API.
    GenerativeLanguage,
    /// `{base}/projects/{project}/locations/{location}/publishers/{publisher}/models/{model}:{method}`.
    ///
    /// Vertex routes by project and location, which is why one adapter
    /// instance is one project, one region and one model.
    Vertex {
        /// Google Cloud project id.
        project: String,
        /// Region, e.g. `"europe-west4"`, or [`VERTEX_GLOBAL_LOCATION`].
        location: String,
        /// Model publisher; [`VERTEX_PUBLISHER`] for Google's own models.
        publisher: String,
    },
}

impl RouteShape {
    /// A Vertex route for Google's own models.
    #[must_use]
    pub fn vertex(project: impl Into<String>, location: impl Into<String>) -> Self {
        Self::Vertex {
            project: project.into(),
            location: location.into(),
            publisher: VERTEX_PUBLISHER.to_owned(),
        }
    }

    /// Sets the publisher, for a partner model served through Vertex.
    #[must_use]
    pub fn with_publisher(mut self, publisher: impl Into<String>) -> Self {
        if let Self::Vertex {
            publisher: current, ..
        } = &mut self
        {
            *current = publisher.into();
        }
        self
    }

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::GenerativeLanguage => "generative_language",
            Self::Vertex { .. } => "vertex",
        }
    }

    /// The regional base URL Vertex serves `location` from.
    ///
    /// ```
    /// use turnframe_provider_gemini::profile::RouteShape;
    ///
    /// assert_eq!(
    ///     RouteShape::vertex_base_url("europe-west4"),
    ///     "https://europe-west4-aiplatform.googleapis.com/v1"
    /// );
    /// // The multi-region endpoint has no prefix.
    /// assert_eq!(
    ///     RouteShape::vertex_base_url("global"),
    ///     "https://aiplatform.googleapis.com/v1"
    /// );
    /// ```
    #[must_use]
    pub fn vertex_base_url(location: &str) -> String {
        if location == VERTEX_GLOBAL_LOCATION {
            "https://aiplatform.googleapis.com/v1".to_owned()
        } else {
            format!("https://{location}-aiplatform.googleapis.com/v1")
        }
    }

    /// The base URL this route defaults to, when it has one.
    #[must_use]
    pub fn default_base_url(&self) -> Option<String> {
        match self {
            Self::GenerativeLanguage => Some(GEMINI_BASE_URL.to_owned()),
            Self::Vertex { location, .. } => Some(Self::vertex_base_url(location)),
        }
    }

    /// The resource path of the model, without the method suffix.
    ///
    /// ```
    /// use turnframe_provider_gemini::profile::RouteShape;
    ///
    /// assert_eq!(
    ///     RouteShape::GenerativeLanguage.model_path("gemini-2.5-flash"),
    ///     "models/gemini-2.5-flash"
    /// );
    /// assert_eq!(
    ///     RouteShape::vertex("aurora", "europe-west4").model_path("gemini-2.5-flash"),
    ///     "projects/aurora/locations/europe-west4/publishers/google/models/gemini-2.5-flash"
    /// );
    /// ```
    #[must_use]
    pub fn model_path(&self, model: &str) -> String {
        match self {
            Self::GenerativeLanguage => format!("models/{model}"),
            Self::Vertex {
                project,
                location,
                publisher,
            } => format!(
                "projects/{project}/locations/{location}/publishers/{publisher}/models/{model}"
            ),
        }
    }

    /// The full URL of one call.
    ///
    /// `base_url` has no trailing slash (the builder normalizes it). A streamed
    /// call asks for server-sent events explicitly: without `alt=sse` the
    /// endpoint answers with a streamed JSON *array*, which is a different
    /// framing altogether.
    ///
    /// ```
    /// use turnframe_provider_gemini::profile::RouteShape;
    ///
    /// let route = RouteShape::GenerativeLanguage;
    /// let base = "https://generativelanguage.googleapis.com/v1beta";
    /// assert_eq!(
    ///     route.endpoint_url(base, "gemini-2.5-flash", false),
    ///     format!("{base}/models/gemini-2.5-flash:generateContent")
    /// );
    /// assert_eq!(
    ///     route.endpoint_url(base, "gemini-2.5-flash", true),
    ///     format!("{base}/models/gemini-2.5-flash:streamGenerateContent?alt=sse")
    /// );
    /// ```
    #[must_use]
    pub fn endpoint_url(&self, base_url: &str, model: &str, streaming: bool) -> String {
        let path = self.model_path(model);
        if streaming {
            format!("{base_url}/{path}:{STREAM_GENERATE_CONTENT}?alt=sse")
        } else {
            format!("{base_url}/{path}:{GENERATE_CONTENT}")
        }
    }
}

/// A category Google's safety filters classify content under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum HarmCategory {
    /// Harassment.
    HarmCategoryHarassment,
    /// Hate speech.
    HarmCategoryHateSpeech,
    /// Sexually explicit content.
    HarmCategorySexuallyExplicit,
    /// Dangerous content.
    HarmCategoryDangerousContent,
    /// Civic integrity.
    HarmCategoryCivicIntegrity,
}

impl HarmCategory {
    /// Every category, for exhaustive configuration.
    pub const ALL: [Self; 5] = [
        Self::HarmCategoryHarassment,
        Self::HarmCategoryHateSpeech,
        Self::HarmCategorySexuallyExplicit,
        Self::HarmCategoryDangerousContent,
        Self::HarmCategoryCivicIntegrity,
    ];

    /// The name Google's API uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HarmCategoryHarassment => "HARM_CATEGORY_HARASSMENT",
            Self::HarmCategoryHateSpeech => "HARM_CATEGORY_HATE_SPEECH",
            Self::HarmCategorySexuallyExplicit => "HARM_CATEGORY_SEXUALLY_EXPLICIT",
            Self::HarmCategoryDangerousContent => "HARM_CATEGORY_DANGEROUS_CONTENT",
            Self::HarmCategoryCivicIntegrity => "HARM_CATEGORY_CIVIC_INTEGRITY",
        }
    }
}

impl fmt::Display for HarmCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How aggressively a [`HarmCategory`] is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum HarmBlockThreshold {
    /// Block low and above.
    BlockLowAndAbove,
    /// Block medium and above.
    BlockMediumAndAbove,
    /// Block only high.
    BlockOnlyHigh,
    /// Block nothing, but still report ratings.
    BlockNone,
    /// Turn the filter off entirely, where the deployment allows it.
    Off,
}

impl HarmBlockThreshold {
    /// The name Google's API uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlockLowAndAbove => "BLOCK_LOW_AND_ABOVE",
            Self::BlockMediumAndAbove => "BLOCK_MEDIUM_AND_ABOVE",
            Self::BlockOnlyHigh => "BLOCK_ONLY_HIGH",
            Self::BlockNone => "BLOCK_NONE",
            Self::Off => "OFF",
        }
    }
}

impl fmt::Display for HarmBlockThreshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry of Gemini's `safetySettings`.
///
/// Loosening a filter is a deployment decision with legal weight, so it is
/// explicit configuration on the builder and never a default. Whatever is
/// configured, a blocked answer still surfaces as
/// [`ContentFilter`](turnframe_provider::error::ProviderErrorKind::ContentFilter),
/// which is `Fatal`: retrying elsewhere until a model complies is a safety
/// bypass, not a recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySetting {
    /// What is being classified.
    pub category: HarmCategory,
    /// How aggressively it is blocked.
    pub threshold: HarmBlockThreshold,
}

impl SafetySetting {
    /// One setting.
    #[must_use]
    pub const fn new(category: HarmCategory, threshold: HarmBlockThreshold) -> Self {
        Self {
            category,
            threshold,
        }
    }
}

/// The field-level differences between the two surfaces.
///
/// Every flag here exists because one surface returns a 400 without it, or with
/// it. A flag that only changes performance does not belong; a flag that
/// changes *meaning* is a capability, not a quirk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Quirks {
    /// Send [`RequestMetadata`](turnframe_provider::request::RequestMetadata)
    /// as Vertex's `labels` object. The developer API rejects the field.
    pub send_labels: bool,
    /// Send `generationConfig.thinkingConfig`. Only models with reasoning
    /// controls accept it.
    pub send_thinking_config: bool,
    /// The endpoint accepts a JSON response schema and `tools` in the same
    /// request.
    ///
    /// Gemini 1.x refused the combination outright. When this is `false` and a
    /// call asks for both, the adapter refuses rather than dropping one of
    /// them: dropping the schema would be a silent capability downgrade, and
    /// dropping the tools would change what the model can do.
    pub structured_output_with_tools: bool,
    /// How many stop sequences the endpoint accepts.
    pub max_stop_sequences: usize,
    /// Which flavour of the schema dialect to emit.
    pub schema_dialect: SchemaDialect,
}

impl Quirks {
    /// What the Gemini developer API accepts.
    #[must_use]
    pub const fn gemini() -> Self {
        Self {
            send_labels: false,
            send_thinking_config: true,
            structured_output_with_tools: true,
            max_stop_sequences: MAX_STOP_SEQUENCES,
            schema_dialect: SchemaDialect::gemini(),
        }
    }

    /// What Vertex AI accepts: the same, plus `labels`.
    #[must_use]
    pub const fn vertex() -> Self {
        Self {
            send_labels: true,
            ..Self::gemini()
        }
    }

    /// The conservative set: nothing optional is sent.
    ///
    /// The right starting point for an older model or an API version nobody has
    /// measured — every field that could provoke a 400 is off.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            send_labels: false,
            send_thinking_config: false,
            structured_output_with_tools: false,
            max_stop_sequences: MAX_STOP_SEQUENCES,
            schema_dialect: SchemaDialect::conservative(),
        }
    }

    /// Switches Vertex's `labels`.
    #[must_use]
    pub const fn with_labels(mut self, enabled: bool) -> Self {
        self.send_labels = enabled;
        self
    }

    /// Switches `thinkingConfig`.
    #[must_use]
    pub const fn with_thinking_config(mut self, enabled: bool) -> Self {
        self.send_thinking_config = enabled;
        self
    }

    /// Switches whether a schema and tools may travel together.
    #[must_use]
    pub const fn with_structured_output_with_tools(mut self, enabled: bool) -> Self {
        self.structured_output_with_tools = enabled;
        self
    }

    /// Sets the stop-sequence limit.
    #[must_use]
    pub const fn with_max_stop_sequences(mut self, limit: usize) -> Self {
        self.max_stop_sequences = limit;
        self
    }

    /// Sets the schema dialect.
    #[must_use]
    pub const fn with_schema_dialect(mut self, dialect: SchemaDialect) -> Self {
        self.schema_dialect = dialect;
        self
    }
}

impl Default for Quirks {
    fn default() -> Self {
        Self::gemini()
    }
}

/// One endpoint shape: where the request goes, how it authenticates, what it
/// can do and what it gets wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointProfile {
    provider: ProviderKey,
    route: RouteShape,
    auth: AuthScheme,
    default_base_url: Option<String>,
    capabilities: ProviderCapabilities,
    max_structured_output: StructuredOutputCapability,
    quirks: Quirks,
}

impl EndpointProfile {
    /// The Gemini developer API.
    #[must_use]
    pub fn gemini() -> Self {
        Self {
            provider: ProviderKey::from(GEMINI_PROVIDER_KEY),
            route: RouteShape::GenerativeLanguage,
            auth: AuthScheme::ApiKeyHeader,
            default_base_url: Some(GEMINI_BASE_URL.to_owned()),
            capabilities: default_capabilities(),
            max_structured_output: StructuredOutputCapability::NativeJsonSchema,
            quirks: Quirks::gemini(),
        }
    }

    /// Vertex AI, in one project and one region.
    ///
    /// The base URL defaults to the region's own host; override it for a
    /// private endpoint or a proxy.
    #[must_use]
    pub fn vertex_ai(project: impl Into<String>, location: impl Into<String>) -> Self {
        let route = RouteShape::vertex(project, location);
        Self {
            provider: ProviderKey::from(VERTEX_PROVIDER_KEY),
            default_base_url: route.default_base_url(),
            route,
            auth: AuthScheme::OAuthBearer,
            capabilities: default_capabilities(),
            max_structured_output: StructuredOutputCapability::NativeJsonSchema,
            quirks: Quirks::vertex(),
        }
    }

    /// Overrides the provider key metrics and replay records are labelled with.
    #[must_use]
    pub fn with_provider_key(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = provider.into();
        self
    }

    /// Overrides the default base URL.
    #[must_use]
    pub fn with_default_base_url(mut self, base_url: Option<impl Into<String>>) -> Self {
        self.default_base_url = base_url.map(Into::into);
        self
    }

    /// Sets the profile's default capability declaration.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Lowers the strongest structured-output transport the profile admits.
    ///
    /// The ceiling only ever lowers: a ceiling a caller can lift is not one.
    #[must_use]
    pub fn with_max_structured_output(mut self, ceiling: StructuredOutputCapability) -> Self {
        if ceiling > self.max_structured_output {
            self.max_structured_output = ceiling;
        }
        self
    }

    /// Replaces the wire quirks.
    #[must_use]
    pub const fn with_quirks(mut self, quirks: Quirks) -> Self {
        self.quirks = quirks;
        self
    }

    /// Sets the publisher, for a partner model served through Vertex.
    #[must_use]
    pub fn with_publisher(mut self, publisher: impl Into<String>) -> Self {
        self.route = self.route.with_publisher(publisher);
        self
    }

    /// The provider key.
    #[must_use]
    pub const fn provider(&self) -> &ProviderKey {
        &self.provider
    }

    /// The route shape.
    #[must_use]
    pub const fn route(&self) -> &RouteShape {
        &self.route
    }

    /// The authentication scheme.
    #[must_use]
    pub const fn auth(&self) -> AuthScheme {
        self.auth
    }

    /// The default base URL, when the profile has one.
    #[must_use]
    pub fn default_base_url(&self) -> Option<&str> {
        self.default_base_url.as_deref()
    }

    /// The profile's default capability declaration.
    #[must_use]
    pub const fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// The strongest structured-output transport a declaration may name.
    #[must_use]
    pub const fn max_structured_output(&self) -> StructuredOutputCapability {
        self.max_structured_output
    }

    /// The wire quirks.
    #[must_use]
    pub const fn quirks(&self) -> Quirks {
        self.quirks
    }
}

/// The declaration both profiles start from.
///
/// It is a *starting point for a model family*, not a measurement:
/// `preserves_call_ids` is `false` because the REST `functionCall` part carries
/// no id of its own, and `max_context_tokens` is unset because it is a
/// per-model number the adopter knows and this crate does not.
fn default_capabilities() -> ProviderCapabilities {
    ProviderCapabilities::minimal()
        .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
        .with_tool_calling(ToolCallingCapability::Parallel)
        .with_vision(true)
        .with_documents(true)
        .with_streaming(true)
        .with_prompt_caching(true)
        .with_preserves_call_ids(false)
        .with_temperature(true)
        .with_seed(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_surfaces_route_and_authenticate_differently() {
        let developer = EndpointProfile::gemini();
        assert_eq!(developer.provider().as_str(), "gemini");
        assert_eq!(developer.auth(), AuthScheme::ApiKeyHeader);
        assert!(!developer.auth().is_oauth());
        assert_eq!(developer.route().as_str(), "generative_language");
        assert_eq!(developer.default_base_url(), Some(GEMINI_BASE_URL));
        assert!(!developer.quirks().send_labels);

        let vertex = EndpointProfile::vertex_ai("aurora", "us-central1");
        assert_eq!(vertex.provider().as_str(), "vertex-ai");
        assert_eq!(vertex.auth(), AuthScheme::OAuthBearer);
        assert!(vertex.auth().is_oauth());
        assert_eq!(vertex.route().as_str(), "vertex");
        assert_eq!(
            vertex.default_base_url(),
            Some("https://us-central1-aiplatform.googleapis.com/v1")
        );
        assert!(vertex.quirks().send_labels);
    }

    #[test]
    fn the_vertex_path_carries_project_location_publisher_and_model() {
        let route = RouteShape::vertex("aurora", "europe-west4").with_publisher("anthropic");
        assert_eq!(
            route.endpoint_url(
                "https://europe-west4-aiplatform.googleapis.com/v1",
                "claude-x",
                false
            ),
            "https://europe-west4-aiplatform.googleapis.com/v1/projects/aurora/locations/\
             europe-west4/publishers/anthropic/models/claude-x:generateContent"
        );
        // `with_publisher` on the developer route is a no-op, not a panic.
        assert_eq!(
            RouteShape::GenerativeLanguage.with_publisher("x"),
            RouteShape::GenerativeLanguage
        );
    }

    #[test]
    fn a_streamed_call_asks_for_server_sent_events_explicitly() {
        let url = RouteShape::vertex("aurora", "global").endpoint_url(
            "https://aiplatform.googleapis.com/v1",
            "gemini-2.5-flash",
            true,
        );
        assert!(url.ends_with(":streamGenerateContent?alt=sse"), "{url}");
    }

    #[test]
    fn the_headers_are_named_but_their_values_are_built_privately() {
        assert_eq!(AuthScheme::ApiKeyHeader.header_value("k"), "k");
        assert_eq!(AuthScheme::OAuthBearer.header_value("t"), "Bearer t");
        assert_eq!(AuthScheme::OAuthBearer.to_string(), "oauth_bearer");
    }

    #[test]
    fn the_ceiling_only_ever_lowers() {
        let profile = EndpointProfile::gemini()
            .with_max_structured_output(StructuredOutputCapability::JsonObject);
        assert_eq!(
            profile.max_structured_output(),
            StructuredOutputCapability::JsonObject
        );
        // Trying to raise it back does nothing.
        let raised =
            profile.with_max_structured_output(StructuredOutputCapability::NativeJsonSchema);
        assert_eq!(
            raised.max_structured_output(),
            StructuredOutputCapability::JsonObject
        );
    }

    #[test]
    fn the_default_declaration_is_honest_about_call_ids() {
        let capabilities = default_capabilities();
        assert!(!capabilities.preserves_call_ids);
        assert!(capabilities.structured_output.enforces_schema());
        assert!(capabilities.supports_tools());
        assert!(capabilities.max_context_tokens.is_none());
    }

    #[test]
    fn the_conservative_quirks_send_nothing_optional() {
        let quirks = Quirks::conservative();
        assert!(!quirks.send_labels);
        assert!(!quirks.send_thinking_config);
        assert!(!quirks.structured_output_with_tools);
        assert!(!quirks.schema_dialect.property_ordering);
        assert_eq!(Quirks::default(), Quirks::gemini());
        assert!(
            Quirks::gemini()
                .with_labels(true)
                .with_max_stop_sequences(2)
                .with_thinking_config(false)
                .with_structured_output_with_tools(false)
                .send_labels
        );
    }

    #[test]
    fn safety_settings_serialize_with_googles_own_spelling() {
        let setting = SafetySetting::new(
            HarmCategory::HarmCategoryDangerousContent,
            HarmBlockThreshold::BlockOnlyHigh,
        );
        assert_eq!(
            serde_json::to_value(setting).unwrap(),
            serde_json::json!({
                "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                "threshold": "BLOCK_ONLY_HIGH"
            })
        );
        for category in HarmCategory::ALL {
            assert!(category.as_str().starts_with("HARM_CATEGORY_"));
            assert_eq!(
                serde_json::to_value(category).unwrap(),
                serde_json::json!(category.as_str())
            );
        }
        assert_eq!(HarmBlockThreshold::Off.to_string(), "OFF");
        assert_eq!(
            HarmCategory::HarmCategoryHarassment.to_string(),
            "HARM_CATEGORY_HARASSMENT"
        );
    }
}
