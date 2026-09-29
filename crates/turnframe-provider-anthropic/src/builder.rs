//! Building a provider, and the things the builder refuses to do.
//!
//! [`AnthropicProviderBuilder`] assembles an [`AnthropicProvider`] from a
//! profile, a credential, a base URL, a model and whatever headers the
//! deployment needs. Every check it runs exists to keep a promise:
//!
//! * **A declaration cannot outrun what the wire can carry.** The Messages API
//!   has no `response_format`, no JSON mode and no grammar, so
//!   [`NativeJsonSchema`](StructuredOutputCapability::NativeJsonSchema),
//!   [`JsonObject`](StructuredOutputCapability::JsonObject) and
//!   [`GrammarConstrained`](StructuredOutputCapability::GrammarConstrained) are
//!   refused outright — not lowered, not warned about, refused. There is no
//!   other constructor, so *no* `AnthropicProvider` can exist claiming a
//!   transport this adapter does not send (spec §0 rule 9, §20.3).
//! * **A declaration cannot outrun its profile.** A transport stronger than
//!   [`EndpointProfile::max_structured_output`] is refused as well, so a
//!   profile written for an endpoint nobody has measured cannot be talked into
//!   claiming schema enforcement.
//! * **The forced-tool transport needs tools.**
//!   [`NativeFunctionSchema`](StructuredOutputCapability::NativeFunctionSchema)
//!   *is* a tool call, so declaring it alongside
//!   [`ToolCallingCapability::None`](turnframe_provider::capabilities::ToolCallingCapability::None)
//!   is a contradiction the builder will not assemble.
//! * **A credential travels in the credential slot.** The base URL may not
//!   carry user info, and an extra header may not be the authentication header
//!   or the version header, so a key cannot be smuggled into a place that gets
//!   logged (spec §25.2).
//! * **A misconfiguration fails at build time**, as a [`ConfigError`] naming
//!   the field at fault, rather than as a 400 in production.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use turnframe_provider::capabilities::{
    MicroCents, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::{ErrorCode, ProviderError};
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::request::DEFAULT_TIMEOUT;
use turnframe_provider::secret::{ApiKey, DefaultRedactor};

use crate::profile::{BETA_HEADER, EndpointProfile, Quirks, VERSION_HEADER};
use crate::provider::AnthropicProvider;

/// Header carrying the call's stable request id (spec §20.7).
pub const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Header names an extra header may never claim.
///
/// The first three are how a credential reaches the endpoint; the last two are
/// set from the profile, and an override would silently change what the
/// endpoint does with a request the capability declaration describes.
pub const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    VERSION_HEADER,
    BETA_HEADER,
];

/// A provider could not be configured.
///
/// `Display` names the field at fault and never its value: a rejected header
/// value may well *be* the credential that made it invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// No base URL was given and the profile has no default.
    #[error("no base URL: this profile has no default, so one must be configured")]
    MissingBaseUrl,
    /// The base URL is not an absolute HTTP URL.
    #[error("the base URL is not usable: {reason}")]
    InvalidBaseUrl {
        /// What is wrong with it. Never the URL itself.
        reason: &'static str,
    },
    /// The base URL carries `user:password@`, which would put a credential in
    /// every log line and trace attribute.
    #[error("the base URL carries user info: pass the credential as an ApiKey instead")]
    CredentialInBaseUrl,
    /// No model was named.
    #[error("no model: a provider instance is one provider-model pair")]
    MissingModel,
    /// The profile requires a credential and none was given.
    #[error("no API key: this profile authenticates with the {header} header")]
    MissingApiKey {
        /// The header the credential would have travelled in.
        header: String,
    },
    /// The credential cannot become a header value (a stray newline, say).
    #[error("the API key is not a valid header value")]
    InvalidApiKey,
    /// The `anthropic-version` value is empty or unusable as a header value.
    #[error("the anthropic-version value is not a valid header value")]
    InvalidApiVersion,
    /// A beta flag is not a usable header value.
    #[error("an anthropic-beta value is not a valid header value")]
    InvalidBeta,
    /// An extra header has an unusable name.
    #[error("the header name {name} is not valid")]
    InvalidHeaderName {
        /// The sanitized name.
        name: String,
    },
    /// An extra header has an unusable value.
    #[error("the value of header {name} is not a valid header value")]
    InvalidHeaderValue {
        /// The header's name. The value is deliberately absent.
        name: String,
    },
    /// An extra header would override authentication or versioning.
    #[error("the header {name} is reserved: credentials travel as an ApiKey")]
    ReservedHeader {
        /// The offending name.
        name: String,
    },
    /// The declared structured-output transport is stronger than the profile
    /// admits.
    #[error(
        "the profile admits at most {ceiling} structured output, but {declared} was declared; \
         lower the declaration or choose a profile that backs it"
    )]
    StructuredOutputAboveProfile {
        /// What was declared.
        declared: StructuredOutputCapability,
        /// What the profile admits.
        ceiling: StructuredOutputCapability,
    },
    /// The declared transport is one the Messages API does not have.
    #[error(
        "the Messages API has no response format and no grammar, so this adapter sends a forced \
         tool, a prompt description or nothing, and cannot honour {declared}"
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
    /// The HTTP client could not be built.
    #[error("the HTTP client could not be built")]
    Client,
}

impl From<ConfigError> for ProviderError {
    /// A configuration fault is an invalid request the adapter makes of itself,
    /// and it will not get better on a retry.
    fn from(value: ConfigError) -> Self {
        Self::invalid_request(match value {
            ConfigError::MissingBaseUrl => "missing_base_url",
            ConfigError::InvalidBaseUrl { .. } => "invalid_base_url",
            ConfigError::CredentialInBaseUrl => "credential_in_base_url",
            ConfigError::MissingModel => "missing_model",
            ConfigError::MissingApiKey { .. } => "missing_api_key",
            ConfigError::InvalidApiKey => "invalid_api_key",
            ConfigError::InvalidApiVersion => "invalid_api_version",
            ConfigError::InvalidBeta => "invalid_beta",
            ConfigError::InvalidHeaderName { .. } => "invalid_header_name",
            ConfigError::InvalidHeaderValue { .. } => "invalid_header_value",
            ConfigError::ReservedHeader { .. } => "reserved_header",
            ConfigError::StructuredOutputAboveProfile { .. } => "structured_output_above_profile",
            ConfigError::UnsupportedTransport { .. } => "unsupported_transport",
            ConfigError::TransportNeedsToolCalling => "transport_needs_tool_calling",
            ConfigError::Client => "client_build_failed",
        })
    }
}

/// Assembles an [`AnthropicProvider`].
///
/// ```
/// use turnframe_provider::provider::ModelProvider;
/// use turnframe_provider::secret::ApiKey;
/// use turnframe_provider_anthropic::AnthropicProvider;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let provider = AnthropicProvider::anthropic()
///     .api_key(ApiKey::new("sk-ant-not-a-real-key"))
///     .model("claude-sonnet-4-5-20250929")
///     .build()?;
/// assert_eq!(provider.endpoint(), "https://api.anthropic.com/v1/messages");
/// assert!(provider.capabilities().structured_output.enforces_schema());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct AnthropicProviderBuilder {
    profile: EndpointProfile,
    provider: Option<ProviderKey>,
    base_url: Option<String>,
    model: Option<ModelKey>,
    api_key: Option<ApiKey>,
    api_version: Option<String>,
    betas: Vec<String>,
    headers: Vec<(String, String)>,
    timeout: Duration,
    capabilities: Option<ProviderCapabilities>,
    quirks: Option<Quirks>,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl AnthropicProviderBuilder {
    /// A builder for `profile`.
    #[must_use]
    pub fn new(profile: EndpointProfile) -> Self {
        Self {
            profile,
            provider: None,
            base_url: None,
            model: None,
            api_key: None,
            api_version: None,
            betas: Vec::new(),
            headers: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            capabilities: None,
            quirks: None,
            cost: None,
            region: None,
            tags: Vec::new(),
        }
    }

    /// A builder for Anthropic's own API.
    #[must_use]
    pub fn anthropic() -> Self {
        Self::new(EndpointProfile::anthropic())
    }

    /// A builder for another endpoint that reimplements the Messages API.
    #[must_use]
    pub fn compatible(provider: impl Into<ProviderKey>) -> Self {
        Self::new(EndpointProfile::compatible(provider))
    }

    /// The credential, as the provider crate's redacting wrapper.
    #[must_use]
    pub fn api_key(mut self, api_key: ApiKey) -> Self {
        self.api_key = Some(api_key);
        self
    }

    /// The endpoint root, without a trailing slash. Overrides the profile's
    /// default.
    #[must_use]
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// The model. One provider instance is one provider-model pair.
    #[must_use]
    pub fn model(mut self, model: impl Into<ModelKey>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Overrides the `anthropic-version` header.
    #[must_use]
    pub fn api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = Some(api_version.into());
        self
    }

    /// Adds an `anthropic-beta` feature flag, on top of the profile's own.
    #[must_use]
    pub fn beta(mut self, beta: impl Into<String>) -> Self {
        self.betas.push(beta.into());
        self
    }

    /// An extra header, for gateways that want one (a route hint, a tenant id).
    ///
    /// Reserved names are refused at [`build`](Self::build): a credential
    /// belongs in [`api_key`](Self::api_key), where it is redacted everywhere.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
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

    /// The capability declaration for this provider-model pair.
    ///
    /// This is the honest place to record what a conformance run measured.
    /// [`build`](Self::build) refuses a declaration the adapter or the profile
    /// does not back.
    #[must_use]
    pub fn capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// Overrides the profile's wire quirks.
    #[must_use]
    pub fn quirks(mut self, quirks: Quirks) -> Self {
        self.quirks = Some(quirks);
        self
    }

    /// Overrides the provider key metrics and replay records are labelled with.
    #[must_use]
    pub fn provider_key(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Per-million-token prices, for cost-aware routing.
    #[must_use]
    pub const fn cost(mut self, input: MicroCents, output: MicroCents) -> Self {
        self.cost = Some((input, output));
        self
    }

    /// The data-residency label, for region-aware routing.
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
    pub fn build(self) -> Result<AnthropicProvider, ConfigError> {
        let mut profile = self.profile;
        if let Some(quirks) = self.quirks {
            profile = profile.with_quirks(quirks);
        }
        if let Some(api_version) = self.api_version {
            profile = profile.with_api_version(api_version);
        }
        for beta in self.betas {
            profile = profile.with_beta(beta);
        }
        let capabilities = self
            .capabilities
            .unwrap_or_else(|| profile.capabilities().clone());
        check_transport(&capabilities, profile.max_structured_output())?;

        let base_url = normalize_base_url(
            self.base_url
                .as_deref()
                .or_else(|| profile.default_base_url())
                .ok_or(ConfigError::MissingBaseUrl)?,
        )?;
        let model = self.model.ok_or(ConfigError::MissingModel)?;
        if model.is_empty() {
            return Err(ConfigError::MissingModel);
        }
        let endpoint = profile.endpoint_url(&base_url);

        if self.api_key.is_none() && profile.credential_required() {
            return Err(ConfigError::MissingApiKey {
                header: profile.auth().header_name().to_owned(),
            });
        }
        let headers = build_headers(&profile, self.api_key.as_ref(), &self.headers)?;

        let client = reqwest::Client::builder()
            .build()
            .map_err(|_| ConfigError::Client)?;
        let mut redactor = DefaultRedactor::new();
        if let Some(key) = &self.api_key {
            redactor = redactor.with_secret(key);
        }

        Ok(AnthropicProvider::assemble(crate::provider::Parts {
            provider: self.provider.unwrap_or_else(|| profile.provider().clone()),
            model,
            capabilities,
            profile,
            base_url,
            endpoint,
            api_key: self.api_key,
            headers,
            client,
            timeout: self.timeout,
            redactor,
            cost: self.cost,
            region: self.region,
            tags: self.tags,
        }))
    }
}

/// Refuses a declaration the adapter or the profile cannot back.
fn check_transport(
    capabilities: &ProviderCapabilities,
    ceiling: StructuredOutputCapability,
) -> Result<(), ConfigError> {
    let declared = capabilities.structured_output;
    match declared {
        StructuredOutputCapability::NativeFunctionSchema
        | StructuredOutputCapability::PromptOnly
        | StructuredOutputCapability::None => {}
        other => return Err(ConfigError::UnsupportedTransport { declared: other }),
    }
    // The capability enum is ordered strongest first, so "stronger than the
    // ceiling" is "sorts before it".
    if declared < ceiling {
        return Err(ConfigError::StructuredOutputAboveProfile { declared, ceiling });
    }
    if declared == StructuredOutputCapability::NativeFunctionSchema
        && !capabilities.supports_tools()
    {
        return Err(ConfigError::TransportNeedsToolCalling);
    }
    Ok(())
}

/// Normalizes and vets a base URL.
fn normalize_base_url(raw: &str) -> Result<String, ConfigError> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(ConfigError::MissingBaseUrl);
    }
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or(ConfigError::InvalidBaseUrl {
            reason: "it must start with http:// or https://",
        })?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(ConfigError::InvalidBaseUrl {
            reason: "it has no host",
        });
    }
    if authority.contains('@') {
        return Err(ConfigError::CredentialInBaseUrl);
    }
    Ok(trimmed.to_owned())
}

/// Builds the header map sent with every request.
///
/// The authentication value is marked sensitive, so even a `Debug` of the map
/// renders it as `Sensitive` rather than as the key.
fn build_headers(
    profile: &EndpointProfile,
    api_key: Option<&ApiKey>,
    extra: &[(String, String)],
) -> Result<HeaderMap, ConfigError> {
    let mut headers = HeaderMap::new();
    if let Some(key) = api_key {
        let auth = profile.auth();
        let mut value = HeaderValue::try_from(auth.header_value(key))
            .map_err(|_| ConfigError::InvalidApiKey)?;
        value.set_sensitive(true);
        let name = HeaderName::try_from(auth.header_name()).map_err(|_| {
            ConfigError::InvalidHeaderName {
                name: ErrorCode::new(auth.header_name()).as_str().to_owned(),
            }
        })?;
        headers.insert(name, value);
    }

    // The version header is not optional: an unversioned request is refused.
    let version = profile.api_version().trim();
    if version.is_empty() {
        return Err(ConfigError::InvalidApiVersion);
    }
    headers.insert(
        HeaderName::try_from(VERSION_HEADER).map_err(|_| ConfigError::InvalidApiVersion)?,
        HeaderValue::try_from(version).map_err(|_| ConfigError::InvalidApiVersion)?,
    );
    if !profile.betas().is_empty() {
        let joined = profile.betas().join(",");
        headers.insert(
            HeaderName::try_from(BETA_HEADER).map_err(|_| ConfigError::InvalidBeta)?,
            HeaderValue::try_from(joined).map_err(|_| ConfigError::InvalidBeta)?,
        );
    }

    for (name, value) in extra {
        let lowered = name.trim().to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lowered.as_str()) || lowered == profile.auth().header_name() {
            return Err(ConfigError::ReservedHeader { name: lowered });
        }
        let parsed_name =
            HeaderName::try_from(lowered.as_str()).map_err(|_| ConfigError::InvalidHeaderName {
                name: ErrorCode::new(&lowered).as_str().to_owned(),
            })?;
        let mut parsed_value =
            HeaderValue::try_from(value.as_str()).map_err(|_| ConfigError::InvalidHeaderValue {
                name: lowered.clone(),
            })?;
        // An adopter's extra header may itself be a token for a gateway; it is
        // never printed.
        parsed_value.set_sensitive(true);
        headers.insert(parsed_name, parsed_value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_provider::capabilities::ToolCallingCapability;
    use turnframe_provider::provider::ModelProvider;

    use crate::profile::{ANTHROPIC_BASE_URL, API_KEY_HEADER, DEFAULT_ANTHROPIC_VERSION};

    const PLANTED: &str = "sk-ant-planted-0123456789abcdef";

    fn key() -> ApiKey {
        ApiKey::new(PLANTED)
    }

    #[test]
    fn the_anthropic_defaults_produce_the_published_endpoint() {
        let provider = AnthropicProviderBuilder::anthropic()
            .api_key(key())
            .model("claude-sonnet-4-5-20250929")
            .build()
            .expect("builds");
        assert_eq!(
            provider.endpoint(),
            format!("{ANTHROPIC_BASE_URL}/v1/messages")
        );
        assert_eq!(provider.base_url(), ANTHROPIC_BASE_URL);
        assert_eq!(provider.provider_key().as_str(), "anthropic");
        assert_eq!(provider.model_key().as_str(), "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn a_transport_the_messages_api_does_not_have_is_refused_outright() {
        for declared in [
            StructuredOutputCapability::NativeJsonSchema,
            StructuredOutputCapability::JsonObject,
            StructuredOutputCapability::GrammarConstrained,
        ] {
            let error = AnthropicProviderBuilder::anthropic()
                .api_key(key())
                .model("claude-x")
                .capabilities(
                    ProviderCapabilities::minimal()
                        .with_structured_output(declared)
                        .with_tool_calling(ToolCallingCapability::Parallel),
                )
                .build()
                .expect_err("refused");
            assert_eq!(error, ConfigError::UnsupportedTransport { declared });
            assert!(error.to_string().contains("forced tool"), "{error}");
        }
    }

    #[test]
    fn a_declaration_cannot_outrun_its_profile() {
        let lying = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
            .with_tool_calling(ToolCallingCapability::Parallel);
        let error = AnthropicProviderBuilder::compatible("aurora")
            .api_key(key())
            .base_url("https://claude.aurora.test")
            .model("m")
            .capabilities(lying)
            .build();
        // The generic profile's ceiling is the adapter's own, so this is allowed…
        assert!(error.is_ok());

        // …but a profile whose ceiling was lowered refuses it.
        let measured = crate::profile::EndpointProfile::compatible("aurora")
            .with_default_base_url("https://claude.aurora.test")
            .with_max_structured_output(StructuredOutputCapability::PromptOnly);
        let error = AnthropicProviderBuilder::new(measured)
            .api_key(key())
            .model("m")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
                    .with_tool_calling(ToolCallingCapability::Parallel),
            )
            .build()
            .expect_err("refused");
        assert_eq!(
            error,
            ConfigError::StructuredOutputAboveProfile {
                declared: StructuredOutputCapability::NativeFunctionSchema,
                ceiling: StructuredOutputCapability::PromptOnly
            }
        );
        assert!(error.to_string().contains("lower the declaration"));
    }

    #[test]
    fn the_forced_tool_transport_cannot_be_declared_without_tools() {
        let error = AnthropicProviderBuilder::anthropic()
            .api_key(key())
            .model("claude-x")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
                    .with_tool_calling(ToolCallingCapability::None),
            )
            .build()
            .expect_err("refused");
        assert_eq!(error, ConfigError::TransportNeedsToolCalling);
    }

    #[test]
    fn a_weaker_declaration_is_always_allowed() {
        let provider = AnthropicProviderBuilder::anthropic()
            .api_key(key())
            .model("claude-haiku")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::PromptOnly),
            )
            .build()
            .expect("builds");
        assert_eq!(
            provider.capabilities().structured_output,
            StructuredOutputCapability::PromptOnly
        );
        assert!(
            provider
                .supports(&turnframe_provider::purpose::ModelPurpose::Extract.requirements())
                .is_err(),
            "prompt_only may not plan mutations"
        );
    }

    #[test]
    fn a_credential_may_not_ride_in_the_url_or_in_a_header() {
        let error = AnthropicProviderBuilder::compatible("aurora")
            .api_key(key())
            .base_url("https://user:secret@gateway.test")
            .model("m")
            .build()
            .expect_err("refused");
        assert_eq!(error, ConfigError::CredentialInBaseUrl);

        for reserved in ["X-Api-Key", "Authorization", "anthropic-version"] {
            let error = AnthropicProviderBuilder::anthropic()
                .api_key(key())
                .model("m")
                .header(reserved, "sneaky")
                .build()
                .expect_err("refused");
            assert_eq!(
                error,
                ConfigError::ReservedHeader {
                    name: reserved.to_ascii_lowercase()
                }
            );
        }
    }

    #[test]
    fn missing_pieces_are_named_precisely() {
        assert_eq!(
            AnthropicProviderBuilder::anthropic()
                .api_key(key())
                .build()
                .expect_err("refused"),
            ConfigError::MissingModel
        );
        assert_eq!(
            AnthropicProviderBuilder::compatible("aurora")
                .api_key(key())
                .model("m")
                .build()
                .expect_err("refused"),
            ConfigError::MissingBaseUrl
        );
        assert_eq!(
            AnthropicProviderBuilder::anthropic()
                .model("m")
                .build()
                .expect_err("refused"),
            ConfigError::MissingApiKey {
                header: API_KEY_HEADER.to_owned()
            }
        );
        assert!(matches!(
            AnthropicProviderBuilder::compatible("aurora")
                .api_key(key())
                .base_url("gateway.test")
                .model("m")
                .build(),
            Err(ConfigError::InvalidBaseUrl { .. })
        ));
        assert!(matches!(
            AnthropicProviderBuilder::anthropic()
                .api_key(key())
                .model("m")
                .api_version("  ")
                .build(),
            Err(ConfigError::InvalidApiVersion)
        ));
    }

    #[test]
    fn a_profile_without_a_credential_requirement_builds_without_one() {
        let profile = crate::profile::EndpointProfile::compatible("local")
            .with_default_base_url("http://127.0.0.1:8787")
            .with_credential_required(false);
        let provider = AnthropicProviderBuilder::new(profile)
            .model("claude-proxy")
            .build()
            .expect("builds");
        assert_eq!(provider.endpoint(), "http://127.0.0.1:8787/v1/messages");
        assert!(provider.key_fingerprint().is_none());
    }

    #[test]
    fn every_request_carries_the_version_and_the_betas() {
        let profile = crate::profile::EndpointProfile::anthropic().with_beta("first-beta");
        let provider = AnthropicProviderBuilder::new(profile)
            .api_key(key())
            .model("claude-x")
            .beta("second-beta")
            .build()
            .expect("builds");
        let headers = provider.headers_for_test();
        assert_eq!(
            headers.get(VERSION_HEADER).and_then(|v| v.to_str().ok()),
            Some(DEFAULT_ANTHROPIC_VERSION)
        );
        assert_eq!(
            headers.get(BETA_HEADER).and_then(|v| v.to_str().ok()),
            Some("first-beta,second-beta")
        );
    }

    #[test]
    fn the_authentication_header_is_marked_sensitive() {
        let headers = build_headers(
            &crate::profile::EndpointProfile::anthropic(),
            Some(&key()),
            &[],
        )
        .expect("builds");
        let value = headers.get(API_KEY_HEADER).expect("the auth header");
        assert!(value.is_sensitive());
        assert_eq!(format!("{value:?}"), "Sensitive");
        assert!(!format!("{headers:?}").contains("sk-ant-planted"));
    }

    #[test]
    fn configuration_errors_become_non_retryable_provider_errors() {
        let error: ProviderError = ConfigError::MissingModel.into();
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fatal
        );
        assert!(error.to_string().contains("missing_model"));
        let error: ProviderError = ConfigError::TransportNeedsToolCalling.into();
        assert!(error.to_string().contains("transport_needs_tool_calling"));
    }
}
