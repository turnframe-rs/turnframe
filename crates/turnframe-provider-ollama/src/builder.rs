//! Building a provider, and the four things the builder refuses to do.
//!
//! [`OllamaProviderBuilder`] assembles an [`OllamaProvider`] from a base URL, a
//! model and — optionally — a credential. Every check it runs exists to keep a
//! promise:
//!
//! * **A credential is optional, because a local daemon has none.** This is the
//!   one place this adapter differs structurally from every other in the
//!   workspace: `ollama serve` listens on `127.0.0.1:11434` and authenticates
//!   nothing, so requiring a key would force adopters to invent one. A bearer
//!   token is accepted all the same, for the instance behind an authenticating
//!   proxy or a hosted Ollama-compatible runtime.
//! * **A declaration cannot outrun what this adapter puts on the wire.**
//!   [`NativeFunctionSchema`](StructuredOutputCapability::NativeFunctionSchema)
//!   and [`GrammarConstrained`](StructuredOutputCapability::GrammarConstrained)
//!   are refused: this adapter sends a JSON Schema in `format`, or `"json"`, or
//!   nothing, and no profile here may claim a transport it does not send.
//! * **`preserves_call_ids` cannot be claimed.** Ollama's chat format has no
//!   call id, so an adapter here always synthesizes them. A profile declaring
//!   otherwise would be lying about the one field a caller uses to correlate a
//!   tool result, and there is no constructor that lets it.
//! * **A credential travels in the credential slot.** The base URL may not
//!   carry user info, and an extra header may not be the authentication header,
//!   so a token cannot be smuggled into a place that gets logged (spec §25.2).

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use turnframe_provider::capabilities::{
    MicroCents, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::{ErrorCode, ProviderError};
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::request::DEFAULT_TIMEOUT;
use turnframe_provider::secret::{ApiKey, DefaultRedactor};

use crate::declarations::baseline;
use crate::provider::OllamaProvider;
use crate::wire::request::WireSettings;

/// Where `ollama serve` listens by default.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";

/// The native chat endpoint, appended to the base URL.
pub const CHAT_PATH: &str = "/api/chat";

/// The provider key a profile is labelled with unless one is configured.
pub const DEFAULT_PROVIDER_KEY: &str = "ollama";

/// Header names an extra header may never claim, because they are how a
/// credential reaches a proxied endpoint.
pub const RESERVED_HEADERS: &[&str] = &["authorization", "api-key", "x-api-key"];

/// A provider could not be configured.
///
/// `Display` names the field at fault and never its value: a rejected header
/// value may well *be* the credential that made it invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
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
    /// The credential cannot become a header value (a stray newline, say).
    #[error("the bearer token is not a valid header value")]
    InvalidApiKey,
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
    /// The declared transport is one this adapter cannot put on the wire.
    #[error(
        "this adapter sends a JSON Schema in `format`, `\"json\"`, or nothing, \
         so it cannot honour {declared}"
    )]
    UnsupportedTransport {
        /// What was declared.
        declared: StructuredOutputCapability,
    },
    /// The declaration claims tool-call ids survive, which they cannot.
    #[error(
        "Ollama's chat format carries no tool-call id, so preserves_call_ids \
         cannot be true: this adapter synthesizes every id it returns"
    )]
    CallIdsCannotBePreserved,
    /// The HTTP client could not be built.
    #[error("the HTTP client could not be built")]
    Client,
}

impl From<ConfigError> for ProviderError {
    /// A configuration fault is an invalid request the adapter makes of itself,
    /// and it will not get better on a retry.
    fn from(value: ConfigError) -> Self {
        Self::invalid_request(match value {
            ConfigError::InvalidBaseUrl { .. } => "invalid_base_url",
            ConfigError::CredentialInBaseUrl => "credential_in_base_url",
            ConfigError::MissingModel => "missing_model",
            ConfigError::InvalidApiKey => "invalid_api_key",
            ConfigError::InvalidHeaderName { .. } => "invalid_header_name",
            ConfigError::InvalidHeaderValue { .. } => "invalid_header_value",
            ConfigError::ReservedHeader { .. } => "reserved_header",
            ConfigError::UnsupportedTransport { .. } => "unsupported_transport",
            ConfigError::CallIdsCannotBePreserved => "call_ids_cannot_be_preserved",
            ConfigError::Client => "client_build_failed",
        })
    }
}

/// Assembles an [`OllamaProvider`].
///
/// ```
/// use turnframe_provider::capabilities::StructuredOutputCapability;
/// use turnframe_provider::provider::ModelProvider;
/// use turnframe_provider::secret::ApiKey;
/// use turnframe_provider_ollama::{OllamaProvider, declarations::baseline};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // The ordinary case: a daemon on this machine, and no credential anywhere.
/// let local = OllamaProvider::local().model("qwen3:8b").build()?;
/// assert_eq!(local.endpoint(), "http://127.0.0.1:11434/api/chat");
/// assert!(local.key_fingerprint().is_none());
///
/// // The same adapter behind an authenticating proxy.
/// let remote = OllamaProvider::at("https://ollama.internal")
///     .bearer_token(ApiKey::new("not-a-real-token"))
///     .model("qwen3:8b")
///     .capabilities(baseline().with_structured_output(StructuredOutputCapability::NativeJsonSchema))
///     .context_tokens(32_768)
///     .build()?;
/// assert!(remote.key_fingerprint().is_some());
/// assert!(remote.capabilities().structured_output.enforces_schema());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct OllamaProviderBuilder {
    provider: Option<ProviderKey>,
    base_url: String,
    model: Option<ModelKey>,
    api_key: Option<ApiKey>,
    headers: Vec<(String, String)>,
    timeout: Duration,
    capabilities: Option<ProviderCapabilities>,
    keep_alive: Option<String>,
    context_tokens: Option<u64>,
    cost: Option<(MicroCents, MicroCents)>,
    region: Option<String>,
    tags: Vec<String>,
}

impl OllamaProviderBuilder {
    /// A builder for the daemon at `base_url`.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            provider: None,
            base_url: base_url.into(),
            model: None,
            api_key: None,
            headers: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            capabilities: None,
            keep_alive: None,
            context_tokens: None,
            cost: None,
            region: None,
            tags: Vec::new(),
        }
    }

    /// A builder for the daemon on this machine, at [`DEFAULT_BASE_URL`].
    #[must_use]
    pub fn local() -> Self {
        Self::new(DEFAULT_BASE_URL)
    }

    /// The endpoint root, without a trailing slash and without `/api/chat`.
    #[must_use]
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// The model tag, exactly as `ollama list` prints it: `qwen3:8b`.
    ///
    /// One provider instance is one provider-model pair, because conformance
    /// is per pair and a declaration covering "Ollama" declares nothing.
    #[must_use]
    pub fn model(mut self, model: impl Into<ModelKey>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// A bearer token, for an instance behind an authenticating proxy.
    ///
    /// Leave it unset for a local daemon: it authenticates nothing, and the
    /// builder does not invent a credential it has no use for.
    #[must_use]
    pub fn bearer_token(mut self, api_key: ApiKey) -> Self {
        self.api_key = Some(api_key);
        self
    }

    /// An extra header, for a gateway that wants one.
    ///
    /// Reserved names are refused at [`build`](Self::build): a credential
    /// belongs in [`bearer_token`](Self::bearer_token), where it is redacted
    /// everywhere.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The transport deadline.
    ///
    /// The effective deadline of a call is the smaller of this and
    /// [`ModelRequest::timeout`](turnframe_provider::request::ModelRequest::timeout),
    /// so a caller can always ask for less time but never for more. A cold
    /// local model is loaded from disk on the first call, which is worth
    /// remembering when choosing this.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The capability declaration for this provider-**model** pair.
    ///
    /// The default is [`baseline`], which claims only what the daemon backs for
    /// anything it runs. This is the honest place to record what a conformance
    /// run against *this model* measured.
    #[must_use]
    pub fn capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// The context window the daemon must allocate, as `options.num_ctx`.
    ///
    /// Ollama defaults a model's window to a small value and **silently
    /// truncates** a longer prompt, so a declared
    /// [`max_context_tokens`](ProviderCapabilities::max_context_tokens) that
    /// never reached the daemon would be a declaration about nothing. Setting
    /// it here overrides the declared window; leaving it unset makes the
    /// declared window the one that travels.
    #[must_use]
    pub const fn context_tokens(mut self, tokens: u64) -> Self {
        self.context_tokens = Some(tokens);
        self
    }

    /// How long the daemon keeps the model resident after a call, as Ollama
    /// spells a duration: `"5m"`, `"1h"`, `"0"` to unload immediately.
    #[must_use]
    pub fn keep_alive(mut self, keep_alive: impl Into<String>) -> Self {
        self.keep_alive = Some(keep_alive.into());
        self
    }

    /// Overrides the provider key metrics and replay records are labelled with.
    #[must_use]
    pub fn provider_key(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Per-million-token prices, for cost-aware routing.
    ///
    /// A local daemon bills nothing, so this is normally left unset; a hosted
    /// Ollama-compatible runtime may well charge, and a router that compares
    /// candidates on cost needs the number to compare.
    #[must_use]
    pub const fn cost(mut self, input: MicroCents, output: MicroCents) -> Self {
        self.cost = Some((input, output));
        self
    }

    /// The data-residency label, for region-aware routing.
    ///
    /// A machine under a desk is the strongest data-residency story there is,
    /// and a router can only act on it if it is labelled.
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
    pub fn build(self) -> Result<OllamaProvider, ConfigError> {
        let capabilities = self.capabilities.unwrap_or_else(baseline);
        check_transport(capabilities.structured_output)?;
        if capabilities.preserves_call_ids {
            return Err(ConfigError::CallIdsCannotBePreserved);
        }

        let base_url = normalize_base_url(&self.base_url)?;
        let model = self.model.ok_or(ConfigError::MissingModel)?;
        if model.is_empty() {
            return Err(ConfigError::MissingModel);
        }
        let endpoint = format!("{base_url}{CHAT_PATH}");
        let headers = build_headers(self.api_key.as_ref(), &self.headers)?;

        let client = reqwest::Client::builder()
            .build()
            .map_err(|_| ConfigError::Client)?;
        let mut redactor = DefaultRedactor::new();
        if let Some(key) = &self.api_key {
            redactor = redactor.with_secret(key);
        }

        Ok(OllamaProvider::assemble(crate::provider::Parts {
            provider: self
                .provider
                .unwrap_or_else(|| ProviderKey::from(DEFAULT_PROVIDER_KEY)),
            model,
            capabilities,
            base_url,
            endpoint,
            api_key: self.api_key,
            headers,
            client,
            timeout: self.timeout,
            redactor,
            settings: WireSettings {
                keep_alive: self.keep_alive,
                num_ctx: self.context_tokens,
            },
            cost: self.cost,
            region: self.region,
            tags: self.tags,
        }))
    }
}

impl Default for OllamaProviderBuilder {
    fn default() -> Self {
        Self::local()
    }
}

/// Refuses a declaration this adapter cannot put on the wire.
fn check_transport(declared: StructuredOutputCapability) -> Result<(), ConfigError> {
    match declared {
        StructuredOutputCapability::NativeJsonSchema
        | StructuredOutputCapability::JsonObject
        | StructuredOutputCapability::PromptOnly
        | StructuredOutputCapability::None => Ok(()),
        other => Err(ConfigError::UnsupportedTransport { declared: other }),
    }
}

/// Normalizes and vets a base URL.
fn normalize_base_url(raw: &str) -> Result<String, ConfigError> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(ConfigError::InvalidBaseUrl {
            reason: "it is empty",
        });
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
/// renders it as `Sensitive` rather than as the token. An unauthenticated
/// daemon gets an empty map, which is exactly right: there is nothing to send.
fn build_headers(
    api_key: Option<&ApiKey>,
    extra: &[(String, String)],
) -> Result<HeaderMap, ConfigError> {
    let mut headers = HeaderMap::new();
    if let Some(key) = api_key {
        let mut value = HeaderValue::try_from(format!("Bearer {}", key.expose()))
            .map_err(|_| ConfigError::InvalidApiKey)?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    for (name, value) in extra {
        let lowered = name.trim().to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lowered.as_str()) {
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

    #[test]
    fn a_local_daemon_needs_no_credential() {
        let provider = OllamaProvider::local()
            .model("qwen3:8b")
            .build()
            .expect("builds");
        assert_eq!(provider.endpoint(), "http://127.0.0.1:11434/api/chat");
        assert_eq!(provider.provider_key().as_str(), DEFAULT_PROVIDER_KEY);
        assert!(provider.key_fingerprint().is_none());
    }

    #[test]
    fn a_bearer_token_is_accepted_for_a_proxied_instance() {
        let provider = OllamaProvider::at("https://ollama.internal/")
            .bearer_token(ApiKey::new("tok-planted-0123456789abcdef"))
            .model("qwen3:8b")
            .build()
            .expect("builds");
        // The trailing slash is normalized away rather than doubling up.
        assert_eq!(provider.endpoint(), "https://ollama.internal/api/chat");
        let fingerprint = provider.key_fingerprint().expect("a token");
        assert_eq!(fingerprint.len(), 8);
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("tok-planted"), "{rendered}");
        assert!(rendered.contains("authorization"), "{rendered}");
        assert!(rendered.contains(&fingerprint), "{rendered}");
    }

    #[test]
    fn a_model_is_required_because_a_profile_is_one_pair() {
        assert_eq!(
            OllamaProvider::local().build().expect_err("no model"),
            ConfigError::MissingModel
        );
        assert_eq!(
            OllamaProvider::local()
                .model("")
                .build()
                .expect_err("no model"),
            ConfigError::MissingModel
        );
    }

    #[test]
    fn a_transport_this_adapter_cannot_send_is_refused_at_build_time() {
        let error = OllamaProvider::local()
            .model("qwen3:8b")
            .capabilities(
                baseline().with_structured_output(StructuredOutputCapability::GrammarConstrained),
            )
            .build()
            .expect_err("no grammar goes on this wire");
        assert_eq!(
            error,
            ConfigError::UnsupportedTransport {
                declared: StructuredOutputCapability::GrammarConstrained
            }
        );
    }

    #[test]
    fn a_profile_cannot_claim_that_call_ids_survive() {
        let error = OllamaProvider::local()
            .model("qwen3:8b")
            .capabilities(
                baseline()
                    .with_tool_calling(ToolCallingCapability::Parallel)
                    .with_preserves_call_ids(true),
            )
            .build()
            .expect_err("the format has no id");
        assert_eq!(error, ConfigError::CallIdsCannotBePreserved);
    }

    #[test]
    fn a_credential_cannot_be_smuggled_into_the_url_or_a_header() {
        assert_eq!(
            OllamaProvider::at("http://user:pass@ollama.internal")
                .model("qwen3:8b")
                .build()
                .expect_err("user info"),
            ConfigError::CredentialInBaseUrl
        );
        assert_eq!(
            OllamaProvider::local()
                .model("qwen3:8b")
                .header("Authorization", "Bearer nope")
                .build()
                .expect_err("reserved"),
            ConfigError::ReservedHeader {
                name: "authorization".to_owned()
            }
        );
    }

    #[test]
    fn a_base_url_must_be_an_absolute_http_url() {
        assert!(matches!(
            OllamaProvider::at("127.0.0.1:11434")
                .model("qwen3:8b")
                .build()
                .expect_err("no scheme"),
            ConfigError::InvalidBaseUrl { .. }
        ));
        assert!(matches!(
            OllamaProvider::at("http://")
                .model("qwen3:8b")
                .build()
                .expect_err("no host"),
            ConfigError::InvalidBaseUrl { .. }
        ));
    }

    #[test]
    fn the_default_declaration_is_not_admitted_for_a_mutation_capable_stage() {
        let provider = OllamaProvider::local()
            .model("smollm2:135m")
            .build()
            .expect("builds");
        let mutation = turnframe_provider::purpose::ModelPurpose::Extract.requirements();
        let mismatch = provider
            .supports(&mutation)
            .expect_err("json_object is not enough");
        assert!(mismatch.structured_output_unmet());

        // The same daemon, a model that earned the declaration.
        let measured = OllamaProvider::local()
            .model("qwen3:8b")
            .capabilities(
                baseline().with_structured_output(StructuredOutputCapability::NativeJsonSchema),
            )
            .build()
            .expect("builds");
        assert!(measured.supports(&mutation).is_ok());
    }

    #[test]
    fn a_configuration_fault_becomes_an_invalid_request_that_no_retry_fixes() {
        let error = ProviderError::from(ConfigError::CallIdsCannotBePreserved);
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fatal
        );
        assert!(
            error.to_string().contains("call_ids_cannot_be_preserved"),
            "{error}"
        );
    }
}
