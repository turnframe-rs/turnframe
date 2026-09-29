//! Endpoint profiles: the shapes this adapter speaks (spec §20.3, §20.5).
//!
//! One [`EndpointProfile`] answers four questions and nothing else:
//!
//! 1. **Where does the request go?** Always `{base}/v1/messages`. Unlike the
//!    OpenAI family there is no second route shape: Anthropic's own API and the
//!    endpoints that reimplement it all put the model in the body.
//! 2. **How is the credential presented?** [`AuthScheme::ApiKeyHeader`] writes
//!    the raw key into `x-api-key`, which is what Anthropic itself accepts;
//!    [`AuthScheme::Bearer`] exists for the proxies in front of it that speak
//!    OAuth. Either way the [`anthropic-version`](VERSION_HEADER) header is
//!    sent on every request, because the Messages API pins behaviour to a
//!    dated version and an unversioned request is rejected.
//! 3. **What does this provider-model pair actually do?**
//!    [`EndpointProfile::capabilities`] is the declaration everything
//!    downstream trusts. It is configuration, never an inference from the brand
//!    (spec §20.3).
//! 4. **What does this endpoint get wrong?** [`Quirks`] holds the handful of
//!    field-level differences that are real.
//!
//! # The ceiling
//!
//! [`EndpointProfile::max_structured_output`] is the strongest structured-output
//! transport a profile may declare, and the builder refuses anything above it.
//! For this adapter the ceiling can never exceed
//! [`StructuredOutputCapability::NativeFunctionSchema`], because that is the
//! only schema-enforcing transport the Messages API has: there is no
//! `response_format`, no JSON mode and no grammar. See the crate documentation
//! for why the forced tool is the honest answer rather than a workaround.
//!
//! ```
//! use turnframe_provider_anthropic::profile::EndpointProfile;
//!
//! let profile = EndpointProfile::anthropic();
//! assert_eq!(profile.provider().as_str(), "anthropic");
//! // Schema enforcement, but through a tool — never through a response format.
//! assert!(profile.capabilities().structured_output.enforces_schema());
//! ```

use std::fmt;

use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::ids::ProviderKey;
use turnframe_provider::secret::ApiKey;

/// Public base URL of the Anthropic API.
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// Path appended to the base URL to reach the Messages API.
pub const MESSAGES_PATH: &str = "/v1/messages";

/// Header carrying the dated API version.
///
/// The Messages API pins behaviour to a version string rather than to a URL
/// path, and a request without this header is refused, so the adapter sends it
/// on every call.
pub const VERSION_HEADER: &str = "anthropic-version";

/// Header carrying opt-in beta features, comma-separated.
pub const BETA_HEADER: &str = "anthropic-beta";

/// Header Anthropic authenticates with.
pub const API_KEY_HEADER: &str = "x-api-key";

/// The API version a profile uses when the caller names none.
///
/// `2023-06-01` is the first and, so far, only stable Messages API version;
/// newer capabilities arrive as additive fields and beta headers rather than as
/// a new version string.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// The output cap sent when the caller sets none.
///
/// `max_tokens` is **required** by the Messages API — a request without it is a
/// 400 — so unlike every other adapter this one cannot simply omit the field.
/// A default that is too small silently truncates answers, so it is a
/// configured number rather than a constant buried in the conversion.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4096;

/// Context window of the Claude models this profile is written against.
pub const ANTHROPIC_MAX_CONTEXT_TOKENS: u64 = 200_000;

/// Stop sequences the Messages API accepts in one request.
pub const DEFAULT_MAX_STOP_SEQUENCES: usize = 8;

/// How the credential is presented to the endpoint.
///
/// The value itself never leaves [`ApiKey`] except inside the header this
/// scheme builds, which is why the method that renders that value is
/// crate-private and [`AuthScheme::header_name`] is not (spec §25.2).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthScheme {
    /// The raw key in a named header — `x-api-key` for Anthropic itself.
    ApiKeyHeader {
        /// Lower-case header name.
        name: String,
    },
    /// `Authorization: Bearer <key>`, for the gateways and OAuth proxies that
    /// front the Messages API.
    Bearer,
}

impl AuthScheme {
    /// The scheme Anthropic's own API uses.
    #[must_use]
    pub fn anthropic() -> Self {
        Self::header(API_KEY_HEADER)
    }

    /// A scheme that puts the raw key in `name`.
    #[must_use]
    pub fn header(name: impl Into<String>) -> Self {
        Self::ApiKeyHeader {
            name: name.into().to_ascii_lowercase(),
        }
    }

    /// The header this scheme writes.
    ///
    /// ```
    /// use turnframe_provider_anthropic::profile::AuthScheme;
    ///
    /// assert_eq!(AuthScheme::anthropic().header_name(), "x-api-key");
    /// assert_eq!(AuthScheme::Bearer.header_name(), "authorization");
    /// ```
    #[must_use]
    pub fn header_name(&self) -> &str {
        match self {
            Self::ApiKeyHeader { name } => name,
            Self::Bearer => "authorization",
        }
    }

    /// Builds the header value. Crate-private: the returned `String` holds the
    /// credential and exists only long enough to become a header.
    pub(crate) fn header_value(&self, key: &ApiKey) -> String {
        match self {
            Self::ApiKeyHeader { .. } => key.expose().to_owned(),
            Self::Bearer => format!("Bearer {}", key.expose()),
        }
    }
}

/// The field-level differences between endpoints that all claim to speak the
/// Messages API.
///
/// Every flag here exists because some endpoint returns a 400 without it, or
/// with it. A quirk that changes *meaning* is a capability, not a quirk, and
/// lives in [`ProviderCapabilities`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Quirks {
    /// The `max_tokens` value sent when the request names none. Required by
    /// the API, so there is no "omit it" option.
    pub default_max_output_tokens: u32,
    /// How many `stop_sequences` the endpoint accepts. Extra sequences are
    /// dropped with a
    /// [`FeatureDropped`](turnframe_provider::response::ResponseWarning::FeatureDropped)
    /// warning rather than silently.
    pub max_stop_sequences: usize,
    /// Whether `metadata.user_id` is understood. It is the only metadata field
    /// the Messages API has, so every other label is dropped with a warning
    /// whatever this says.
    pub send_metadata_user_id: bool,
    /// Whether `tool_choice.disable_parallel_tool_use` is understood. Sent only
    /// when the profile declares no parallel tool calling, so that a
    /// `Sequential` declaration is enforced on the wire and not merely stated.
    pub send_disable_parallel_tool_use: bool,
}

impl Quirks {
    /// What Anthropic's own API accepts.
    #[must_use]
    pub const fn anthropic() -> Self {
        Self {
            default_max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_stop_sequences: DEFAULT_MAX_STOP_SEQUENCES,
            send_metadata_user_id: true,
            send_disable_parallel_tool_use: true,
        }
    }

    /// The safest set for an endpoint nobody has measured: only the fields the
    /// Messages API has always had.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            default_max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_stop_sequences: DEFAULT_MAX_STOP_SEQUENCES,
            send_metadata_user_id: false,
            send_disable_parallel_tool_use: false,
        }
    }

    /// Sets the default output cap.
    #[must_use]
    pub const fn with_default_max_output_tokens(mut self, tokens: u32) -> Self {
        self.default_max_output_tokens = tokens;
        self
    }

    /// Sets how many stop sequences fit.
    #[must_use]
    pub const fn with_max_stop_sequences(mut self, limit: usize) -> Self {
        self.max_stop_sequences = limit;
        self
    }

    /// Sets whether `metadata.user_id` is sent.
    #[must_use]
    pub const fn with_metadata_user_id(mut self, send: bool) -> Self {
        self.send_metadata_user_id = send;
        self
    }

    /// Sets whether `disable_parallel_tool_use` is sent.
    #[must_use]
    pub const fn with_disable_parallel_tool_use(mut self, send: bool) -> Self {
        self.send_disable_parallel_tool_use = send;
        self
    }
}

impl Default for Quirks {
    fn default() -> Self {
        Self::anthropic()
    }
}

/// One configured Messages API endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointProfile {
    provider: ProviderKey,
    default_base_url: Option<String>,
    auth: AuthScheme,
    api_version: String,
    betas: Vec<String>,
    credential_required: bool,
    capabilities: ProviderCapabilities,
    max_structured_output: StructuredOutputCapability,
    quirks: Quirks,
}

impl EndpointProfile {
    /// Anthropic's own API.
    ///
    /// The declaration describes the Claude models this crate was written
    /// against: a forced tool as the structured-output transport, parallel tool
    /// calling, vision, document input, streaming, prompt caching, ids that
    /// round-trip, and a
    /// 200 000-token window. It is still a **default**, not a certificate: a
    /// model with a smaller window or without tool support is configured with
    /// [`capabilities`](crate::AnthropicProviderBuilder::capabilities), and the
    /// conformance suite is what licenses whatever you write there.
    #[must_use]
    pub fn anthropic() -> Self {
        Self {
            provider: ProviderKey::from("anthropic"),
            default_base_url: Some(ANTHROPIC_BASE_URL.to_owned()),
            auth: AuthScheme::anthropic(),
            api_version: DEFAULT_ANTHROPIC_VERSION.to_owned(),
            betas: Vec::new(),
            credential_required: true,
            capabilities: ProviderCapabilities::minimal()
                .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
                .with_tool_calling(ToolCallingCapability::Parallel)
                .with_vision(true)
                .with_documents(true)
                .with_streaming(true)
                .with_prompt_caching(true)
                .with_preserves_call_ids(true)
                .with_max_context_tokens(ANTHROPIC_MAX_CONTEXT_TOKENS)
                .with_temperature(true),
            max_structured_output: StructuredOutputCapability::NativeFunctionSchema,
            quirks: Quirks::anthropic(),
        }
    }

    /// Any other endpoint that reimplements the Messages API.
    ///
    /// It has no default base URL and a deliberately modest declaration:
    /// [`PromptOnly`](StructuredOutputCapability::PromptOnly), no tools, no
    /// streaming, no id preservation. Raise it to what a conformance run
    /// against *that* endpoint and *that* model measured, and no further.
    #[must_use]
    pub fn compatible(provider: impl Into<ProviderKey>) -> Self {
        Self {
            provider: provider.into(),
            default_base_url: None,
            auth: AuthScheme::anthropic(),
            api_version: DEFAULT_ANTHROPIC_VERSION.to_owned(),
            betas: Vec::new(),
            credential_required: true,
            capabilities: ProviderCapabilities::minimal()
                .with_structured_output(StructuredOutputCapability::PromptOnly)
                .with_temperature(true),
            // The ceiling is what this *adapter* can put on the wire; what the
            // endpoint honours is what a conformance run measures.
            max_structured_output: StructuredOutputCapability::NativeFunctionSchema,
            quirks: Quirks::conservative(),
        }
    }

    /// Overrides the provider key.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = provider.into();
        self
    }

    /// Sets the default base URL.
    #[must_use]
    pub fn with_default_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.default_base_url = Some(base_url.into());
        self
    }

    /// Sets the authentication scheme.
    #[must_use]
    pub fn with_auth(mut self, auth: AuthScheme) -> Self {
        self.auth = auth;
        self
    }

    /// Pins the `anthropic-version` header.
    #[must_use]
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    /// Adds an `anthropic-beta` feature flag.
    #[must_use]
    pub fn with_beta(mut self, beta: impl Into<String>) -> Self {
        self.betas.push(beta.into());
        self
    }

    /// Sets whether a credential is required to build.
    #[must_use]
    pub const fn with_credential_required(mut self, required: bool) -> Self {
        self.credential_required = required;
        self
    }

    /// Replaces the default capability declaration.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Lowers the structured-output ceiling.
    ///
    /// Raising it is ignored, because a ceiling a caller can lift is not a
    /// ceiling. The strongest value any profile of this adapter may hold is
    /// [`NativeFunctionSchema`](StructuredOutputCapability::NativeFunctionSchema).
    #[must_use]
    pub fn with_max_structured_output(mut self, ceiling: StructuredOutputCapability) -> Self {
        // The enum sorts strongest first, so "stronger" is "sorts before".
        if ceiling > self.max_structured_output {
            self.max_structured_output = ceiling;
        }
        self
    }

    /// Replaces the wire quirks.
    #[must_use]
    pub fn with_quirks(mut self, quirks: Quirks) -> Self {
        self.quirks = quirks;
        self
    }

    /// The provider key metrics and replay records are labelled with.
    #[must_use]
    pub const fn provider(&self) -> &ProviderKey {
        &self.provider
    }

    /// The default base URL, when the profile has one.
    #[must_use]
    pub fn default_base_url(&self) -> Option<&str> {
        self.default_base_url.as_deref()
    }

    /// The authentication scheme.
    #[must_use]
    pub const fn auth(&self) -> &AuthScheme {
        &self.auth
    }

    /// The `anthropic-version` value sent on every request.
    #[must_use]
    pub fn api_version(&self) -> &str {
        &self.api_version
    }

    /// The `anthropic-beta` features, in the order they were added.
    #[must_use]
    pub fn betas(&self) -> &[String] {
        &self.betas
    }

    /// Whether a credential must be configured.
    #[must_use]
    pub const fn credential_required(&self) -> bool {
        self.credential_required
    }

    /// The default capability declaration.
    #[must_use]
    pub const fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// The strongest structured-output transport this profile admits.
    #[must_use]
    pub const fn max_structured_output(&self) -> StructuredOutputCapability {
        self.max_structured_output
    }

    /// The wire quirks in force.
    #[must_use]
    pub const fn quirks(&self) -> &Quirks {
        &self.quirks
    }

    /// The full Messages URL for `base_url`, which carries no trailing slash.
    ///
    /// ```
    /// use turnframe_provider_anthropic::profile::EndpointProfile;
    ///
    /// assert_eq!(
    ///     EndpointProfile::anthropic().endpoint_url("https://api.anthropic.com"),
    ///     "https://api.anthropic.com/v1/messages"
    /// );
    /// ```
    #[must_use]
    pub fn endpoint_url(&self, base_url: &str) -> String {
        format!("{base_url}{MESSAGES_PATH}")
    }
}

impl fmt::Display for EndpointProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.provider, self.api_version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_anthropic_profile_declares_the_transport_it_actually_sends() {
        let profile = EndpointProfile::anthropic();
        assert_eq!(profile.provider().as_str(), "anthropic");
        assert_eq!(profile.default_base_url(), Some(ANTHROPIC_BASE_URL));
        assert_eq!(profile.api_version(), DEFAULT_ANTHROPIC_VERSION);
        assert_eq!(profile.auth().header_name(), API_KEY_HEADER);
        assert_eq!(
            profile.capabilities().structured_output,
            StructuredOutputCapability::NativeFunctionSchema
        );
        assert_eq!(
            profile.max_structured_output(),
            StructuredOutputCapability::NativeFunctionSchema
        );
        assert!(profile.capabilities().preserves_call_ids);
        assert!(profile.betas().is_empty());
        assert!(profile.credential_required());
        assert_eq!(profile.to_string(), "anthropic (2023-06-01)");
    }

    #[test]
    fn a_compatible_profile_claims_nothing_on_anybody_s_behalf() {
        let profile = EndpointProfile::compatible("aurora");
        assert_eq!(profile.provider().as_str(), "aurora");
        assert!(profile.default_base_url().is_none());
        assert_eq!(
            profile.capabilities().structured_output,
            StructuredOutputCapability::PromptOnly
        );
        assert!(!profile.capabilities().supports_tools());
        assert!(!profile.capabilities().streaming);
        assert!(!profile.quirks().send_metadata_user_id);
    }

    #[test]
    fn the_ceiling_only_ever_lowers() {
        let profile = EndpointProfile::anthropic()
            .with_max_structured_output(StructuredOutputCapability::PromptOnly);
        assert_eq!(
            profile.max_structured_output(),
            StructuredOutputCapability::PromptOnly
        );
        let attempted =
            profile.with_max_structured_output(StructuredOutputCapability::NativeJsonSchema);
        assert_eq!(
            attempted.max_structured_output(),
            StructuredOutputCapability::PromptOnly,
            "a ceiling a caller can lift is not a ceiling"
        );
    }

    #[test]
    fn the_auth_value_is_shaped_by_the_scheme() {
        let key = ApiKey::new("sk-ant-not-a-real-key");
        assert_eq!(
            AuthScheme::anthropic().header_value(&key),
            "sk-ant-not-a-real-key"
        );
        assert_eq!(
            AuthScheme::Bearer.header_value(&key),
            "Bearer sk-ant-not-a-real-key"
        );
        assert_eq!(AuthScheme::header("X-Api-Key").header_name(), "x-api-key");
    }

    #[test]
    fn a_profile_can_be_reshaped_for_a_proxy() {
        let profile = EndpointProfile::compatible("proxy")
            .with_default_base_url("https://claude.internal")
            .with_auth(AuthScheme::Bearer)
            .with_api_version("2023-06-01")
            .with_beta("prompt-caching-2024-07-31")
            .with_credential_required(false)
            .with_provider("proxy-eu")
            .with_quirks(Quirks::anthropic().with_max_stop_sequences(2))
            .with_capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeFunctionSchema),
            );
        assert_eq!(profile.provider().as_str(), "proxy-eu");
        assert_eq!(
            profile.endpoint_url("https://claude.internal"),
            "https://claude.internal/v1/messages"
        );
        assert_eq!(profile.auth().header_name(), "authorization");
        assert_eq!(profile.betas(), ["prompt-caching-2024-07-31".to_owned()]);
        assert!(!profile.credential_required());
        assert_eq!(profile.quirks().max_stop_sequences, 2);
    }

    #[test]
    fn quirk_defaults_are_the_anthropic_ones() {
        assert_eq!(Quirks::default(), Quirks::anthropic());
        let tuned = Quirks::conservative()
            .with_default_max_output_tokens(512)
            .with_metadata_user_id(true)
            .with_disable_parallel_tool_use(true);
        assert_eq!(tuned.default_max_output_tokens, 512);
        assert!(tuned.send_metadata_user_id);
        assert!(tuned.send_disable_parallel_tool_use);
    }
}
