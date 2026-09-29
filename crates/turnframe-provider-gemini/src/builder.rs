//! Building a provider, and the things the builder refuses to do.
//!
//! [`GeminiProviderBuilder`] assembles a [`GeminiProvider`] from a profile, a
//! credential, a base URL, a model and whatever the deployment needs. Every
//! check it runs exists to keep a promise:
//!
//! * **A declaration cannot outrun its profile.** A capability set whose
//!   structured-output transport is stronger than
//!   [`EndpointProfile::max_structured_output`] is refused, and so is one
//!   naming a transport this adapter does not put on the wire. There is no
//!   other constructor, so *no* [`GeminiProvider`] can exist claiming schema
//!   enforcement its profile does not back (spec §0 rule 9, §20.3).
//! * **A credential travels in the credential slot.** The base URL may not
//!   carry user info, an extra header may not claim the authentication header,
//!   and neither surface accepts the key as a query parameter — the developer
//!   API's `?key=` form is deliberately not implemented, because a URL is the
//!   one part of a request that reliably ends up in an access log (spec §25.2).
//! * **The two profiles want different credentials, and neither substitutes.**
//!   The developer API needs an [`ApiKey`]; Vertex AI needs a
//!   [`TokenSource`]. Configuring the wrong one is a [`ConfigError`] at build
//!   time rather than a 401 in production.
//! * **A misconfiguration fails at build time.** A project id that would not
//!   survive a URL path, a bad header value, a missing model: all of them are
//!   named before the first request.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use turnframe_provider::capabilities::{
    MicroCents, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::{ErrorCode, ProviderError};
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::request::DEFAULT_TIMEOUT;
use turnframe_provider::secret::{ApiKey, DefaultRedactor};

use crate::credential::{StaticToken, TokenSource};
use crate::profile::{AuthScheme, EndpointProfile, Quirks, RouteShape, SafetySetting};
use crate::provider::GeminiProvider;

/// Header names an extra header may never claim, because they are how a
/// credential reaches the endpoint.
pub const RESERVED_HEADERS: &[&str] = &["authorization", "x-goog-api-key", "x-goog-user-project"];

/// Query parameters a base URL may never carry, for the same reason.
pub const RESERVED_QUERY_PARAMS: &[&str] = &["key", "access_token"];

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
    /// The base URL carries `user:password@` or a credential query parameter.
    #[error("the base URL carries a credential: pass it as an ApiKey or a TokenSource instead")]
    CredentialInBaseUrl,
    /// No model was named.
    #[error("no model: a provider instance is one provider-model pair")]
    MissingModel,
    /// A model name would not survive being put in a URL path.
    #[error("the model name {name} is not URL-safe")]
    InvalidModel {
        /// The sanitized name.
        name: String,
    },
    /// A Vertex project or location is empty or not URL-safe.
    #[error("the Vertex {field} {value} is empty or not URL-safe")]
    InvalidVertexPath {
        /// Which part: `"project"`, `"location"` or `"publisher"`.
        field: &'static str,
        /// The sanitized value.
        value: String,
    },
    /// The developer API needs an API key and none was configured.
    #[error("no API key: the Gemini developer API authenticates with the {header} header")]
    MissingApiKey {
        /// The header the credential would have travelled in.
        header: &'static str,
    },
    /// Vertex AI needs a token source and none was configured.
    #[error(
        "no access token: Vertex AI authenticates with a bearer token, so configure a \
         TokenSource (or access_token for a fixed one)"
    )]
    MissingTokenSource,
    /// An API key was configured on a profile that authenticates with OAuth, or
    /// a token source on one that authenticates with an API key.
    #[error("this profile authenticates with {scheme}, so the {supplied} credential is unusable")]
    CredentialSchemeMismatch {
        /// What the profile expects.
        scheme: AuthScheme,
        /// What was configured: `"api_key"` or `"token_source"`.
        supplied: &'static str,
    },
    /// The credential cannot become a header value (a stray newline, say).
    #[error("the credential is not a valid header value")]
    InvalidCredential,
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
    #[error("the header {name} is reserved: credentials travel as a typed credential")]
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
    /// The declared transport is one this adapter cannot put on the wire.
    #[error(
        "this adapter sends a response schema, a forced function, a JSON mime type or nothing, \
         so it cannot honour {declared}"
    )]
    UnsupportedTransport {
        /// What was declared.
        declared: StructuredOutputCapability,
    },
    /// `NativeFunctionSchema` was declared without tool calling.
    #[error(
        "native_function_schema is a forced function call, so it cannot be declared with \
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
            ConfigError::InvalidModel { .. } => "invalid_model",
            ConfigError::InvalidVertexPath { .. } => "invalid_vertex_path",
            ConfigError::MissingApiKey { .. } => "missing_api_key",
            ConfigError::MissingTokenSource => "missing_token_source",
            ConfigError::CredentialSchemeMismatch { .. } => "credential_scheme_mismatch",
            ConfigError::InvalidCredential => "invalid_credential",
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

/// Assembles a [`GeminiProvider`].
///
/// ```
/// use turnframe_provider::secret::ApiKey;
/// use turnframe_provider_gemini::GeminiProvider;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let gemini = GeminiProvider::gemini()
///     .api_key(ApiKey::new("AIza-not-a-real-key"))
///     .model("gemini-2.5-flash")
///     .build()?;
/// assert!(gemini.endpoint().ends_with("/models/gemini-2.5-flash:generateContent"));
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct GeminiProviderBuilder {
    profile: EndpointProfile,
    provider: Option<ProviderKey>,
    base_url: Option<String>,
    model: Option<ModelKey>,
    api_key: Option<ApiKey>,
    token_source: Option<Arc<dyn TokenSource>>,
    quota_project: Option<String>,
    headers: Vec<(String, String)>,
    timeout: Duration,
    capabilities: Option<ProviderCapabilities>,
    quirks: Option<Quirks>,
    safety_settings: Vec<SafetySetting>,
    thinking_budget: Option<i32>,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl GeminiProviderBuilder {
    /// A builder for `profile`.
    #[must_use]
    pub fn new(profile: EndpointProfile) -> Self {
        Self {
            profile,
            provider: None,
            base_url: None,
            model: None,
            api_key: None,
            token_source: None,
            quota_project: None,
            headers: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            capabilities: None,
            quirks: None,
            safety_settings: Vec::new(),
            thinking_budget: None,
            cost: None,
            region: None,
            tags: Vec::new(),
        }
    }

    /// A builder for the Gemini developer API.
    #[must_use]
    pub fn gemini() -> Self {
        Self::new(EndpointProfile::gemini())
    }

    /// A builder for Vertex AI, in one project and one region.
    #[must_use]
    pub fn vertex_ai(project: impl Into<String>, location: impl Into<String>) -> Self {
        Self::new(EndpointProfile::vertex_ai(project, location))
    }

    /// The developer API's long-lived key.
    #[must_use]
    pub fn api_key(mut self, api_key: ApiKey) -> Self {
        self.api_key = Some(api_key);
        self
    }

    /// Where Vertex AI's short-lived access token comes from.
    ///
    /// Consulted once per request, so a source that caches and refreshes is
    /// enough to keep a long-running process authenticated. See
    /// [`credential`](crate::credential) for why this adapter takes a token
    /// rather than minting one.
    #[must_use]
    pub fn token_source(mut self, source: Arc<dyn TokenSource>) -> Self {
        self.token_source = Some(source);
        self
    }

    /// A fixed Vertex AI access token.
    ///
    /// Shorthand for a [`StaticToken`]. It never refreshes, so it is right for
    /// a test or a short-lived job and wrong for a service: a Google access
    /// token expires within the hour, and after that every call fails with
    /// [`CredentialExpired`](turnframe_provider::error::ProviderErrorKind::CredentialExpired).
    #[must_use]
    pub fn access_token(self, token: ApiKey) -> Self {
        self.token_source(Arc::new(StaticToken::new(token)))
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

    /// The project quota and billing are charged to, sent as
    /// `x-goog-user-project`.
    ///
    /// Only meaningful when the credential belongs to one project and the quota
    /// to another.
    #[must_use]
    pub fn quota_project(mut self, project: impl Into<String>) -> Self {
        self.quota_project = Some(project.into());
        self
    }

    /// An extra header, for a proxy or a gateway that wants one.
    ///
    /// Reserved names are refused at [`build`](Self::build): a credential
    /// belongs in [`api_key`](Self::api_key) or [`token_source`](Self::token_source),
    /// where it is redacted everywhere.
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
    pub const fn quirks(mut self, quirks: Quirks) -> Self {
        self.quirks = Some(quirks);
        self
    }

    /// Overrides the provider key metrics and replay records are labelled with.
    #[must_use]
    pub fn provider_key(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Adds one safety setting, sent with every request.
    ///
    /// Loosening a filter is a deployment decision with legal weight, so there
    /// is no default: what is not configured is Google's own threshold.
    #[must_use]
    pub fn safety_setting(mut self, setting: SafetySetting) -> Self {
        self.safety_settings.push(setting);
        self
    }

    /// The thinking budget, in tokens, for a model with reasoning controls.
    ///
    /// `0` turns thinking off where the model allows it; `-1` asks the model to
    /// decide. The tokens it spends come back as
    /// [`TokenUsage::reasoning`](turnframe_provider::response::TokenUsage::reasoning).
    #[must_use]
    pub const fn thinking_budget(mut self, tokens: i32) -> Self {
        self.thinking_budget = Some(tokens);
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
    /// A Vertex profile fills this in from its location when the caller sets
    /// none, because the location *is* the residency.
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
    pub fn build(self) -> Result<GeminiProvider, ConfigError> {
        let profile = match self.quirks {
            Some(quirks) => self.profile.with_quirks(quirks),
            None => self.profile,
        };
        let capabilities = self
            .capabilities
            .unwrap_or_else(|| profile.capabilities().clone());
        check_transport(&capabilities, profile.max_structured_output())?;

        let model = self.model.ok_or(ConfigError::MissingModel)?;
        if model.is_empty() {
            return Err(ConfigError::MissingModel);
        }
        check_path_segment(model.as_str(), "model")
            .map_err(|value| ConfigError::InvalidModel { name: value })?;
        check_route(profile.route())?;

        let base_url = normalize_base_url(
            self.base_url
                .as_deref()
                .map(str::to_owned)
                .or_else(|| profile.default_base_url().map(str::to_owned))
                .ok_or(ConfigError::MissingBaseUrl)?
                .as_str(),
        )?;
        let endpoint = profile
            .route()
            .endpoint_url(&base_url, model.as_str(), false);
        let stream_endpoint = profile
            .route()
            .endpoint_url(&base_url, model.as_str(), true);

        let credential = check_credential(profile.auth(), self.api_key, self.token_source)?;
        let headers = build_headers(
            credential.api_key.as_ref(),
            profile.auth(),
            self.quota_project.as_deref(),
            &self.headers,
        )?;

        let client = reqwest::Client::builder()
            .build()
            .map_err(|_| ConfigError::Client)?;
        let mut redactor = DefaultRedactor::new();
        if let Some(key) = &credential.api_key {
            redactor = redactor.with_secret(key);
        }
        let region = self.region.or_else(|| match profile.route() {
            RouteShape::Vertex { location, .. } => Some(location.clone()),
            RouteShape::GenerativeLanguage => None,
        });

        Ok(GeminiProvider::assemble(crate::provider::Parts {
            provider: self.provider.unwrap_or_else(|| profile.provider().clone()),
            model,
            capabilities,
            profile,
            base_url,
            endpoint,
            stream_endpoint,
            api_key: credential.api_key,
            token_source: credential.token_source,
            headers,
            client,
            timeout: self.timeout,
            redactor,
            safety_settings: self.safety_settings,
            thinking_budget: self.thinking_budget,
            cost: self.cost,
            region,
            tags: self.tags,
        }))
    }
}

/// Whichever credential the profile's scheme calls for.
struct Credential {
    api_key: Option<ApiKey>,
    token_source: Option<Arc<dyn TokenSource>>,
}

/// Refuses a declaration the adapter or the profile cannot back.
fn check_transport(
    capabilities: &ProviderCapabilities,
    ceiling: StructuredOutputCapability,
) -> Result<(), ConfigError> {
    let declared = capabilities.structured_output;
    match declared {
        StructuredOutputCapability::NativeJsonSchema
        | StructuredOutputCapability::NativeFunctionSchema
        | StructuredOutputCapability::JsonObject
        | StructuredOutputCapability::PromptOnly
        | StructuredOutputCapability::None => {}
        other => return Err(ConfigError::UnsupportedTransport { declared: other }),
    }
    // The capability enum is ordered strongest first, so "stronger than the
    // ceiling" is "sorts before it".
    if declared < ceiling {
        return Err(ConfigError::StructuredOutputAboveProfile { declared, ceiling });
    }
    // The forced function transport *is* a function call, so a profile that
    // declares it and denies tool calling is describing two different models.
    if declared == StructuredOutputCapability::NativeFunctionSchema
        && !capabilities.supports_tools()
    {
        return Err(ConfigError::TransportNeedsToolCalling);
    }
    Ok(())
}

/// Checks that the profile got the credential it authenticates with, and only
/// that one.
fn check_credential(
    scheme: AuthScheme,
    api_key: Option<ApiKey>,
    token_source: Option<Arc<dyn TokenSource>>,
) -> Result<Credential, ConfigError> {
    match scheme {
        AuthScheme::ApiKeyHeader => {
            if token_source.is_some() {
                return Err(ConfigError::CredentialSchemeMismatch {
                    scheme,
                    supplied: "token_source",
                });
            }
            let api_key =
                api_key
                    .filter(|key| !key.is_empty())
                    .ok_or(ConfigError::MissingApiKey {
                        header: scheme.header_name(),
                    })?;
            Ok(Credential {
                api_key: Some(api_key),
                token_source: None,
            })
        }
        AuthScheme::OAuthBearer => {
            if api_key.is_some() {
                return Err(ConfigError::CredentialSchemeMismatch {
                    scheme,
                    supplied: "api_key",
                });
            }
            let token_source = token_source.ok_or(ConfigError::MissingTokenSource)?;
            Ok(Credential {
                api_key: None,
                token_source: Some(token_source),
            })
        }
    }
}

/// Checks the parts of a Vertex route that travel in a URL path.
fn check_route(route: &RouteShape) -> Result<(), ConfigError> {
    let RouteShape::Vertex {
        project,
        location,
        publisher,
    } = route
    else {
        return Ok(());
    };
    for (field, value) in [
        ("project", project),
        ("location", location),
        ("publisher", publisher),
    ] {
        check_path_segment(value, field)
            .map_err(|value| ConfigError::InvalidVertexPath { field, value })?;
    }
    Ok(())
}

/// A value that travels in a URL path must survive being put there.
///
/// On failure returns the sanitized value, so the caller can name it without
/// echoing whatever made it invalid.
fn check_path_segment(value: &str, _field: &str) -> Result<(), String> {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if safe {
        Ok(())
    } else {
        Err(ErrorCode::new(value).as_str().to_owned())
    }
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
    // The developer API also accepts `?key=…`. This adapter does not offer it:
    // a URL is the one part of a request that reliably reaches an access log.
    if let Some(query) = trimmed.split_once('?').map(|(_, query)| query) {
        let carries_credential = query.split('&').any(|pair| {
            let name = pair
                .split('=')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            RESERVED_QUERY_PARAMS.contains(&name.as_str())
        });
        if carries_credential {
            return Err(ConfigError::CredentialInBaseUrl);
        }
    }
    Ok(trimmed.to_owned())
}

/// Builds the header map sent with every request.
///
/// A Vertex bearer token is **not** here: it is fetched per call and added to
/// that one request, so no long-lived structure holds it.
///
/// The authentication value is marked sensitive, so even a `Debug` of the map
/// renders it as `Sensitive` rather than as the key.
fn build_headers(
    api_key: Option<&ApiKey>,
    scheme: AuthScheme,
    quota_project: Option<&str>,
    extra: &[(String, String)],
) -> Result<HeaderMap, ConfigError> {
    let mut headers = HeaderMap::new();
    if let Some(key) = api_key {
        let mut value = HeaderValue::try_from(scheme.header_value(key.expose()))
            .map_err(|_| ConfigError::InvalidCredential)?;
        value.set_sensitive(true);
        let name = HeaderName::try_from(scheme.header_name()).map_err(|_| {
            ConfigError::InvalidHeaderName {
                name: ErrorCode::new(scheme.header_name()).as_str().to_owned(),
            }
        })?;
        headers.insert(name, value);
    }
    if let Some(project) = quota_project.filter(|project| !project.is_empty()) {
        let value =
            HeaderValue::try_from(project).map_err(|_| ConfigError::InvalidHeaderValue {
                name: "x-goog-user-project".to_owned(),
            })?;
        headers.insert(HeaderName::from_static("x-goog-user-project"), value);
    }
    for (name, value) in extra {
        let lowered = name.trim().to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lowered.as_str()) || lowered == scheme.header_name() {
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
        // An adopter's extra header may itself be a token for a proxy; it is
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

    const KEY: &str = "AIzaSyPlanted0123456789abcdefgh";
    const TOKEN: &str = "ya29.planted-0123456789abcdefghij";

    fn gemini() -> GeminiProviderBuilder {
        GeminiProviderBuilder::gemini()
            .api_key(ApiKey::new(KEY))
            .model("gemini-2.5-flash")
    }

    fn vertex() -> GeminiProviderBuilder {
        GeminiProviderBuilder::vertex_ai("aurora-prod", "europe-west4")
            .access_token(ApiKey::new(TOKEN))
            .model("gemini-2.5-flash")
    }

    #[test]
    fn each_profile_builds_its_own_url() {
        let developer = gemini().build().expect("builds");
        assert_eq!(
            developer.endpoint(),
            "https://generativelanguage.googleapis.com/v1beta/models/\
             gemini-2.5-flash:generateContent"
        );
        assert!(
            developer
                .stream_endpoint()
                .ends_with(":streamGenerateContent?alt=sse")
        );
        assert_eq!(developer.provider_key().as_str(), "gemini");

        let cloud = vertex().build().expect("builds");
        assert_eq!(
            cloud.endpoint(),
            "https://europe-west4-aiplatform.googleapis.com/v1/projects/aurora-prod/locations/\
             europe-west4/publishers/google/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(cloud.provider_key().as_str(), "vertex-ai");
        // The location is the data residency, so routing gets it for free.
        assert_eq!(cloud.profile().region.as_deref(), Some("europe-west4"));
    }

    #[test]
    fn each_profile_insists_on_its_own_credential() {
        let no_key = GeminiProviderBuilder::gemini().model("m").build();
        assert!(matches!(
            no_key,
            Err(ConfigError::MissingApiKey {
                header: "x-goog-api-key"
            })
        ));

        let no_token = GeminiProviderBuilder::vertex_ai("aurora", "global")
            .model("m")
            .build();
        assert_eq!(no_token.err(), Some(ConfigError::MissingTokenSource));

        // And refuses the other one, rather than ignoring it.
        let wrong_way_round = GeminiProviderBuilder::vertex_ai("aurora", "global")
            .model("m")
            .api_key(ApiKey::new(KEY))
            .build();
        assert!(matches!(
            wrong_way_round,
            Err(ConfigError::CredentialSchemeMismatch {
                supplied: "api_key",
                ..
            })
        ));

        let other_way = GeminiProviderBuilder::gemini()
            .model("m")
            .access_token(ApiKey::new(TOKEN))
            .build();
        assert!(matches!(
            other_way,
            Err(ConfigError::CredentialSchemeMismatch {
                supplied: "token_source",
                ..
            })
        ));

        // An empty key is missing, not present-and-broken.
        let empty = GeminiProviderBuilder::gemini()
            .model("m")
            .api_key(ApiKey::new(""))
            .build();
        assert!(matches!(empty, Err(ConfigError::MissingApiKey { .. })));
    }

    #[test]
    fn a_declaration_cannot_outrun_the_profile_and_no_other_door_exists() {
        let ceiling = EndpointProfile::gemini()
            .with_max_structured_output(StructuredOutputCapability::JsonObject);
        let error = GeminiProviderBuilder::new(ceiling)
            .api_key(ApiKey::new(KEY))
            .model("m")
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeJsonSchema),
            )
            .build()
            .expect_err("above the ceiling");
        assert!(matches!(
            error,
            ConfigError::StructuredOutputAboveProfile {
                declared: StructuredOutputCapability::NativeJsonSchema,
                ceiling: StructuredOutputCapability::JsonObject
            }
        ));

        // And a transport this adapter never sends is refused outright.
        let error = gemini()
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::GrammarConstrained),
            )
            .build()
            .expect_err("not sent by this adapter");
        assert!(matches!(error, ConfigError::UnsupportedTransport { .. }));

        // The forced function call *is* sent, so it is admitted — but only by a
        // declaration that also admits tool calling, because it is one.
        let error = gemini()
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeFunctionSchema),
            )
            .build()
            .expect_err("a forced function needs tool calling");
        assert!(
            matches!(error, ConfigError::TransportNeedsToolCalling),
            "{error}"
        );

        gemini()
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
                    .with_tool_calling(ToolCallingCapability::Parallel),
            )
            .build()
            .expect("the function transport is one this adapter sends");
    }

    #[test]
    fn a_credential_may_not_travel_in_the_url() {
        let user_info = gemini()
            .base_url("https://user:pass@example.test/v1beta")
            .build();
        assert_eq!(user_info.err(), Some(ConfigError::CredentialInBaseUrl));

        // The developer API's `?key=` form is deliberately not offered.
        let query = gemini()
            .base_url("https://example.test/v1beta?key=AIzaLeaked")
            .build();
        assert_eq!(query.err(), Some(ConfigError::CredentialInBaseUrl));

        let scheme_less = gemini().base_url("example.test").build();
        assert!(matches!(
            scheme_less,
            Err(ConfigError::InvalidBaseUrl { .. })
        ));
        assert!(matches!(
            gemini().base_url("   ").build(),
            Err(ConfigError::MissingBaseUrl)
        ));
    }

    #[test]
    fn an_extra_header_may_not_be_the_authentication_header() {
        for reserved in RESERVED_HEADERS {
            let error = gemini()
                .header(*reserved, "Bearer smuggled")
                .build()
                .expect_err("reserved");
            assert!(matches!(error, ConfigError::ReservedHeader { .. }));
        }
        // A legitimate extra header is accepted and marked sensitive.
        let provider = gemini()
            .header("x-goog-request-reason", "audit-42")
            .quota_project("billing-project")
            .build()
            .expect("builds");
        let rendered = format!("{provider:?}");
        assert!(rendered.contains("x-goog-request-reason"), "{rendered}");
        assert!(rendered.contains("x-goog-user-project"), "{rendered}");
    }

    #[test]
    fn what_travels_in_a_url_path_is_checked_before_the_first_request() {
        let bad_model = GeminiProviderBuilder::gemini()
            .api_key(ApiKey::new(KEY))
            .model("../../admin")
            .build();
        assert!(matches!(bad_model, Err(ConfigError::InvalidModel { .. })));

        let bad_project = GeminiProviderBuilder::vertex_ai("aurora/../x", "global")
            .access_token(ApiKey::new(TOKEN))
            .model("m")
            .build();
        assert!(matches!(
            bad_project,
            Err(ConfigError::InvalidVertexPath {
                field: "project",
                ..
            })
        ));

        let no_model = GeminiProviderBuilder::gemini()
            .api_key(ApiKey::new(KEY))
            .build();
        assert_eq!(no_model.err(), Some(ConfigError::MissingModel));
    }

    #[test]
    fn a_configuration_fault_becomes_a_fatal_provider_error() {
        let error = ProviderError::from(ConfigError::MissingTokenSource);
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fatal
        );
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("missing_token_source".to_owned())
        );
        // And every variant has its own code.
        let codes = [
            ConfigError::MissingBaseUrl,
            ConfigError::InvalidBaseUrl { reason: "x" },
            ConfigError::CredentialInBaseUrl,
            ConfigError::MissingModel,
            ConfigError::InvalidModel { name: "x".into() },
            ConfigError::InvalidVertexPath {
                field: "project",
                value: "x".into(),
            },
            ConfigError::MissingApiKey { header: "h" },
            ConfigError::MissingTokenSource,
            ConfigError::CredentialSchemeMismatch {
                scheme: AuthScheme::OAuthBearer,
                supplied: "api_key",
            },
            ConfigError::InvalidCredential,
            ConfigError::InvalidHeaderName { name: "x".into() },
            ConfigError::InvalidHeaderValue { name: "x".into() },
            ConfigError::ReservedHeader { name: "x".into() },
            ConfigError::StructuredOutputAboveProfile {
                declared: StructuredOutputCapability::NativeJsonSchema,
                ceiling: StructuredOutputCapability::JsonObject,
            },
            ConfigError::UnsupportedTransport {
                declared: StructuredOutputCapability::GrammarConstrained,
            },
            ConfigError::Client,
        ];
        let mut labels: Vec<String> = codes
            .iter()
            .map(|error| {
                ProviderError::from(error.clone())
                    .code()
                    .map(|code| code.as_str().to_owned())
                    .unwrap_or_default()
            })
            .collect();
        let total = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), total, "every variant has its own code");
    }

    #[test]
    fn the_declared_capabilities_are_what_supports_answers_with() {
        let provider = gemini().build().expect("builds");
        let mutation = turnframe_provider::purpose::ModelPurpose::Extract.requirements();
        assert!(provider.supports(&mutation).is_ok());

        let weak = gemini()
            .capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::JsonObject)
                    .with_tool_calling(ToolCallingCapability::None),
            )
            .build()
            .expect("builds");
        let mismatch = weak
            .supports(&mutation)
            .expect_err("json_object is not enough");
        assert!(mismatch.structured_output_unmet());
    }
}
