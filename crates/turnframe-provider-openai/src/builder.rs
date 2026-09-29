//! Building a provider, and the one thing the builder refuses to do.
//!
//! [`OpenAiProviderBuilder`] assembles an [`OpenAiProvider`] from a profile, a
//! credential, a base URL, a model and whatever headers the deployment needs.
//! Every check it runs exists to keep a promise:
//!
//! * **A declaration cannot outrun its profile.** A capability set whose
//!   structured-output transport is stronger than
//!   [`EndpointProfile::max_structured_output`] is refused, and so is one this
//!   *particular* profile has no way to carry: `grammar_constrained` without a
//!   [`GrammarDialect`](crate::profile::GrammarDialect) to travel in,
//!   `native_function_schema` without a function slot to force. There is no
//!   other constructor, so *no* `OpenAiProvider` can exist claiming schema
//!   enforcement that its profile does not back (spec §0 rule 9, §20.3).
//! * **A credential travels in the credential slot.** The base URL may not
//!   carry user info, and an extra header may not be the authentication header,
//!   so a key cannot be smuggled into a place that gets logged (spec §25.2).
//! * **A misconfiguration fails at build time.** A bad header value, a
//!   deployment name that would not survive a URL, a missing model: all of them
//!   are [`ConfigError`]s before the first request rather than a 400 in
//!   production.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use turnframe_provider::capabilities::{
    MicroCents, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::{ErrorCode, ProviderError};
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::request::DEFAULT_TIMEOUT;
use turnframe_provider::secret::{ApiKey, DefaultRedactor};

use crate::profile::{AuthScheme, EndpointProfile, Preset, Quirks};
use crate::provider::OpenAiProvider;

/// Header carrying the organization on OpenAI.
pub const ORGANIZATION_HEADER: &str = "openai-organization";

/// Header carrying the project on OpenAI.
pub const PROJECT_HEADER: &str = "openai-project";

/// Header carrying the call's stable request id (spec §20.7).
pub const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Header names an extra header may never claim, because they are how a
/// credential reaches the endpoint.
pub const RESERVED_HEADERS: &[&str] = &["authorization", "api-key", "x-api-key"];

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
    /// A deployment name would not survive being put in a URL path.
    #[error("the deployment name {name} is not URL-safe")]
    InvalidDeployment {
        /// The sanitized name.
        name: String,
    },
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
    /// An extra header would override the authentication header.
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
    /// `grammar_constrained` was declared and the profile names no dialect.
    ///
    /// The capability says decoding is constrained; it does not say through
    /// which field, and vLLM's `guided_json` and `llama.cpp`'s `grammar` are
    /// not interchangeable. Guessing would send a field the endpoint ignores
    /// while the declaration went on claiming enforcement.
    #[error(
        "grammar_constrained was declared but this profile names no grammar dialect; \
         set Quirks::with_grammar_dialect, or declare a transport the profile backs"
    )]
    GrammarDialectUnset,
    /// `native_function_schema` was declared on a profile with no tool calling.
    ///
    /// The transport *is* a forced function call, so an endpoint with no
    /// function slot cannot serve it.
    #[error(
        "native_function_schema is a forced function call, so it needs tool calling; \
         this declaration has none"
    )]
    FunctionTransportWithoutTools,
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
            ConfigError::InvalidDeployment { .. } => "invalid_deployment",
            ConfigError::InvalidHeaderName { .. } => "invalid_header_name",
            ConfigError::InvalidHeaderValue { .. } => "invalid_header_value",
            ConfigError::ReservedHeader { .. } => "reserved_header",
            ConfigError::StructuredOutputAboveProfile { .. } => "structured_output_above_profile",
            ConfigError::GrammarDialectUnset => "grammar_dialect_unset",
            ConfigError::FunctionTransportWithoutTools => "function_transport_without_tools",
            ConfigError::Client => "client_build_failed",
        })
    }
}

/// Assembles an [`OpenAiProvider`].
///
/// ```
/// use turnframe_provider::provider::ModelProvider;
/// use turnframe_provider::secret::ApiKey;
/// use turnframe_provider_openai::{OpenAiProvider, profile::Preset};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let provider = OpenAiProvider::openai()
///     .api_key(ApiKey::new("sk-not-a-real-key"))
///     .model("gpt-4o-2024-08-06")
///     .build()?;
/// assert_eq!(provider.endpoint(), "https://api.openai.com/v1/chat/completions");
///
/// // A gateway preset needs its own model, and declares far less by default.
/// let groq = OpenAiProvider::preset(Preset::Groq)
///     .api_key(ApiKey::new("gsk-not-a-real-key"))
///     .model("llama-3.3-70b-versatile")
///     .build()?;
/// assert!(!groq.capabilities().structured_output.enforces_schema());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct OpenAiProviderBuilder {
    profile: EndpointProfile,
    provider: Option<ProviderKey>,
    base_url: Option<String>,
    model: Option<ModelKey>,
    deployment: Option<String>,
    api_key: Option<ApiKey>,
    organization: Option<String>,
    project: Option<String>,
    headers: Vec<(String, String)>,
    timeout: Duration,
    capabilities: Option<ProviderCapabilities>,
    quirks: Option<Quirks>,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl OpenAiProviderBuilder {
    /// A builder for `profile`.
    #[must_use]
    pub fn new(profile: EndpointProfile) -> Self {
        Self {
            profile,
            provider: None,
            base_url: None,
            model: None,
            deployment: None,
            api_key: None,
            organization: None,
            project: None,
            headers: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            capabilities: None,
            quirks: None,
            cost: None,
            region: None,
            tags: Vec::new(),
        }
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

    /// The Azure deployment name, when it differs from the model key.
    #[must_use]
    pub fn deployment(mut self, deployment: impl Into<String>) -> Self {
        self.deployment = Some(deployment.into());
        self
    }

    /// The organization, sent as `OpenAI-Organization`.
    #[must_use]
    pub fn organization(mut self, organization: impl Into<String>) -> Self {
        self.organization = Some(organization.into());
        self
    }

    /// The project, sent as `OpenAI-Project`.
    #[must_use]
    pub fn project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    /// An extra header, for gateways that want one (a referer, a route hint).
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
    /// [`build`](Self::build) refuses a declaration the profile does not back.
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
    pub fn build(self) -> Result<OpenAiProvider, ConfigError> {
        let model = self.model.ok_or(ConfigError::MissingModel)?;
        if model.is_empty() {
            return Err(ConfigError::MissingModel);
        }
        // OpenAI's own endpoints know which families reason; a gateway's models are unknown.
        let openai_family = matches!(self.profile.provider().as_str(), "openai" | "azure-openai");
        let reasoning = openai_family && crate::profile::is_reasoning_model(model.as_str());
        let profile = match self.quirks {
            Some(quirks) => self.profile.with_quirks(quirks),
            // A reasoning model refuses `max_tokens` with a 400.
            None if reasoning => {
                let quirks = self
                    .profile
                    .quirks()
                    .clone()
                    .with_max_completion_tokens(true);
                self.profile.with_quirks(quirks)
            }
            None => self.profile,
        };
        let capabilities = self.capabilities.unwrap_or_else(|| {
            let declared = profile.capabilities().clone();
            if openai_family {
                crate::profile::declared_for_model(declared, model.as_str())
            } else {
                declared
            }
        });
        check_transport(&capabilities, &profile)?;

        let base_url = normalize_base_url(
            self.base_url
                .as_deref()
                .or_else(|| profile.default_base_url())
                .ok_or(ConfigError::MissingBaseUrl)?,
        )?;
        let deployment = self.deployment.unwrap_or_else(|| model.as_str().to_owned());
        if profile.route().routes_by_deployment() {
            check_deployment(&deployment)?;
        }
        let endpoint = profile.route().endpoint_url(&base_url, &deployment);

        if self.api_key.is_none() && profile.credential_required() {
            return Err(ConfigError::MissingApiKey {
                header: profile.auth().header_name().to_owned(),
            });
        }
        let headers = build_headers(
            profile.auth(),
            self.api_key.as_ref(),
            self.organization.as_deref(),
            self.project.as_deref(),
            &self.headers,
        )?;

        let client = reqwest::Client::builder()
            .build()
            .map_err(|_| ConfigError::Client)?;
        let mut redactor = DefaultRedactor::new();
        if let Some(key) = &self.api_key {
            redactor = redactor.with_secret(key);
        }

        Ok(OpenAiProvider::assemble(crate::provider::Parts {
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

impl OpenAiProviderBuilder {
    /// A builder for OpenAI itself.
    #[must_use]
    pub fn openai() -> Self {
        Self::new(EndpointProfile::openai())
    }

    /// A builder for Azure OpenAI, pinned to `api_version`.
    #[must_use]
    pub fn azure_openai(api_version: impl Into<String>) -> Self {
        Self::new(EndpointProfile::azure_openai(api_version))
    }

    /// A builder for a generic OpenAI-compatible endpoint.
    #[must_use]
    pub fn compatible(provider: impl Into<ProviderKey>) -> Self {
        Self::new(EndpointProfile::compatible(provider))
    }

    /// A builder for a named preset.
    #[must_use]
    pub fn preset(preset: Preset) -> Self {
        Self::new(EndpointProfile::preset(preset))
    }
}

/// Refuses a declaration the adapter or the profile cannot back.
///
/// All five transports are expressible now, so what is left to check is
/// whether *this* profile can carry the one declared: a grammar needs a
/// dialect to travel in, and a forced function needs a function slot.
fn check_transport(
    capabilities: &ProviderCapabilities,
    profile: &EndpointProfile,
) -> Result<(), ConfigError> {
    let declared = capabilities.structured_output;
    match declared {
        StructuredOutputCapability::GrammarConstrained
            if profile.quirks().grammar_dialect.is_none() =>
        {
            return Err(ConfigError::GrammarDialectUnset);
        }
        StructuredOutputCapability::NativeFunctionSchema if !capabilities.supports_tools() => {
            return Err(ConfigError::FunctionTransportWithoutTools);
        }
        _ => {}
    }
    // The capability enum is ordered strongest first, so "stronger than the
    // ceiling" is "sorts before it".
    let ceiling = profile.max_structured_output();
    if declared < ceiling {
        return Err(ConfigError::StructuredOutputAboveProfile { declared, ceiling });
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

/// A deployment name travels in a URL path, so it must be path-safe.
fn check_deployment(name: &str) -> Result<(), ConfigError> {
    if name.is_empty() {
        return Err(ConfigError::MissingModel);
    }
    let safe = name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if safe {
        Ok(())
    } else {
        Err(ConfigError::InvalidDeployment {
            name: ErrorCode::new(name).as_str().to_owned(),
        })
    }
}

/// Builds the header map sent with every request.
///
/// The authentication value is marked sensitive, so even a `Debug` of the map
/// renders it as `Sensitive` rather than as the key.
fn build_headers(
    auth: &AuthScheme,
    api_key: Option<&ApiKey>,
    organization: Option<&str>,
    project: Option<&str>,
    extra: &[(String, String)],
) -> Result<HeaderMap, ConfigError> {
    let mut headers = HeaderMap::new();
    if let Some(key) = api_key {
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
    for (name, value) in [
        (ORGANIZATION_HEADER, organization),
        (PROJECT_HEADER, project),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            let parsed =
                HeaderValue::try_from(value).map_err(|_| ConfigError::InvalidHeaderValue {
                    name: name.to_owned(),
                })?;
            let parsed_name =
                HeaderName::try_from(name).map_err(|_| ConfigError::InvalidHeaderName {
                    name: name.to_owned(),
                })?;
            headers.insert(parsed_name, parsed);
        }
    }
    for (name, value) in extra {
        let lowered = name.trim().to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lowered.as_str()) || lowered == auth.header_name() {
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

    fn key() -> ApiKey {
        ApiKey::new("sk-conformance-not-a-real-key")
    }

    #[test]
    fn the_openai_defaults_produce_the_published_endpoint() {
        let provider = OpenAiProviderBuilder::openai()
            .api_key(key())
            .model("gpt-4o-2024-08-06")
            .build()
            .expect("builds");
        assert_eq!(
            provider.endpoint(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(provider.provider_key().as_str(), "openai");
        assert_eq!(provider.model_key().as_str(), "gpt-4o-2024-08-06");
    }

    #[test]
    fn azure_uses_the_deployment_and_the_api_version() {
        let provider = OpenAiProviderBuilder::azure_openai("2024-10-21")
            .api_key(key())
            .base_url("https://contoso.openai.azure.com/")
            .model("gpt-4o")
            .deployment("prod-4o")
            .build()
            .expect("builds");
        assert_eq!(
            provider.endpoint(),
            "https://contoso.openai.azure.com/openai/deployments/prod-4o/chat/completions\
             ?api-version=2024-10-21"
        );
        // Without an explicit deployment the model key is used.
        let defaulted = OpenAiProviderBuilder::azure_openai("2024-10-21")
            .api_key(key())
            .base_url("https://contoso.openai.azure.com")
            .model("gpt-4o")
            .build()
            .expect("builds");
        assert!(defaulted.endpoint().contains("/deployments/gpt-4o/"));
    }

    #[test]
    fn a_declaration_cannot_outrun_its_profile() {
        // `llama.cpp` enforces a schema through a grammar and never through a
        // `json_schema` response format, so its ceiling stops one notch below.
        let lying = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeJsonSchema);
        let error = OpenAiProviderBuilder::preset(Preset::LlamaCpp)
            .base_url("http://127.0.0.1:8080/v1")
            .model("local")
            .capabilities(lying)
            .build()
            .expect_err("refused");
        assert_eq!(
            error,
            ConfigError::StructuredOutputAboveProfile {
                declared: StructuredOutputCapability::NativeJsonSchema,
                ceiling: StructuredOutputCapability::GrammarConstrained
            }
        );
        assert!(error.to_string().contains("lower the declaration"));
    }

    #[test]
    fn a_grammar_declaration_needs_a_dialect_to_travel_in() {
        // OpenAI has no grammar field at all, so the capability has nowhere to
        // go and the provider is refused rather than built half-honest.
        let error = OpenAiProviderBuilder::openai()
            .api_key(key())
            .model("gpt-4o")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::GrammarConstrained),
            )
            .build()
            .expect_err("refused");
        assert_eq!(error, ConfigError::GrammarDialectUnset);
        assert!(error.to_string().contains("names no grammar dialect"));

        // The self-hosted presets name one, so the same declaration builds.
        for preset in [Preset::Vllm, Preset::LlamaCpp] {
            OpenAiProviderBuilder::preset(preset)
                .base_url("http://127.0.0.1:8000/v1")
                .model("local")
                .capabilities(
                    ProviderCapabilities::minimal()
                        .with_structured_output(StructuredOutputCapability::GrammarConstrained),
                )
                .build()
                .unwrap_or_else(|error| panic!("{preset} backs a grammar: {error}"));
        }
    }

    #[test]
    fn a_forced_function_transport_needs_a_function_slot() {
        let error = OpenAiProviderBuilder::openai()
            .api_key(key())
            .model("gpt-4o")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeFunctionSchema),
            )
            .build()
            .expect_err("refused");
        assert_eq!(error, ConfigError::FunctionTransportWithoutTools);

        // With tool calling declared it is a legitimate transport (spec §20.4).
        let provider = OpenAiProviderBuilder::openai()
            .api_key(key())
            .model("gpt-4o")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
                    .with_tool_calling(ToolCallingCapability::Parallel),
            )
            .build()
            .expect("builds");
        assert!(provider.capabilities().structured_output.enforces_schema());
    }

    #[test]
    fn a_weaker_declaration_is_always_allowed() {
        let provider = OpenAiProviderBuilder::openai()
            .api_key(key())
            .model("gpt-3.5-turbo")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::JsonObject)
                    .with_tool_calling(ToolCallingCapability::Sequential),
            )
            .build()
            .expect("builds");
        assert_eq!(
            provider.capabilities().structured_output,
            StructuredOutputCapability::JsonObject
        );
    }

    #[test]
    fn a_credential_may_not_ride_in_the_url_or_in_a_header() {
        let error = OpenAiProviderBuilder::compatible("aurora")
            .api_key(key())
            .base_url("https://user:secret@gateway.test/v1")
            .model("m")
            .build()
            .expect_err("refused");
        assert_eq!(error, ConfigError::CredentialInBaseUrl);

        let error = OpenAiProviderBuilder::openai()
            .api_key(key())
            .model("m")
            .header("Authorization", "Bearer sneaky")
            .build()
            .expect_err("refused");
        assert_eq!(
            error,
            ConfigError::ReservedHeader {
                name: "authorization".to_owned()
            }
        );
    }

    #[test]
    fn missing_pieces_are_named_precisely() {
        assert_eq!(
            OpenAiProviderBuilder::openai()
                .api_key(key())
                .build()
                .expect_err("refused"),
            ConfigError::MissingModel
        );
        assert_eq!(
            OpenAiProviderBuilder::compatible("aurora")
                .api_key(key())
                .model("m")
                .build()
                .expect_err("refused"),
            ConfigError::MissingBaseUrl
        );
        assert_eq!(
            OpenAiProviderBuilder::openai()
                .model("m")
                .build()
                .expect_err("refused"),
            ConfigError::MissingApiKey {
                header: "authorization".to_owned()
            }
        );
        assert!(matches!(
            OpenAiProviderBuilder::compatible("aurora")
                .api_key(key())
                .base_url("gateway.test/v1")
                .model("m")
                .build(),
            Err(ConfigError::InvalidBaseUrl { .. })
        ));
    }

    #[test]
    fn a_self_hosted_preset_builds_without_a_credential() {
        let provider = OpenAiProviderBuilder::preset(Preset::Vllm)
            .base_url("http://gpu-01.internal:8000/v1")
            .model("Qwen/Qwen3-32B")
            .build()
            .expect("builds");
        assert_eq!(
            provider.endpoint(),
            "http://gpu-01.internal:8000/v1/chat/completions"
        );
    }

    #[test]
    fn an_azure_deployment_name_must_survive_a_url() {
        let error = OpenAiProviderBuilder::azure_openai("2024-10-21")
            .api_key(key())
            .base_url("https://contoso.openai.azure.com")
            .model("meta/llama?x=1")
            .build()
            .expect_err("refused");
        assert!(matches!(error, ConfigError::InvalidDeployment { .. }));
        // The same model name is fine where the model travels in the body.
        assert!(
            OpenAiProviderBuilder::preset(Preset::Together)
                .api_key(key())
                .model("meta-llama/Llama-3.3-70B-Instruct-Turbo")
                .build()
                .is_ok()
        );
    }

    #[test]
    fn configuration_errors_become_non_retryable_provider_errors() {
        let error: ProviderError = ConfigError::MissingModel.into();
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fatal
        );
        assert!(error.to_string().contains("missing_model"));
    }

    #[test]
    fn the_authentication_header_is_marked_sensitive() {
        let headers =
            build_headers(&AuthScheme::Bearer, Some(&key()), None, None, &[]).expect("builds");
        let value = headers.get("authorization").expect("the auth header");
        assert!(value.is_sensitive());
        assert_eq!(format!("{value:?}"), "Sensitive");
        assert!(!format!("{headers:?}").contains("sk-conformance"));
    }
}
