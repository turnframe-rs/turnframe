//! A prompt source backed by a Langfuse project, over the **Langfuse v4 public
//! API**.
//!
//! **Read this before enabling the feature.** It introduces a runtime dependency
//! on a remote service: every prompt is an HTTP round trip inside a user's turn,
//! and when Langfuse is unreachable the instructions come from whatever
//! [`CachedPromptSource`] still holds — or, on a cold process, from nowhere at
//! all. [`FilePromptSource`](crate::FilePromptSource) stays the recommended
//! default, where the repository is the source of truth: a turn's meaning cannot
//! change because somebody edited a registry entry while the system was running.
//!
//! **Pin a version.** [`PromptSelector::Version`] is immutable, so an edit
//! cannot silently change what a running system does, and it is what the replay
//! record cites — an audit then names something that cannot have been rewritten
//! underneath it. This adapter re-checks the version the registry returned
//! against the pin and refuses a mismatch. Tracking `production` instead is
//! legitimate and should be deliberate.
//!
//! ```text
//! GET {base}/api/public/v2/prompts/{name}?label={label}
//! GET {base}/api/public/v2/prompts/{name}?version={version}
//! ```
//!
//! authenticated with HTTP Basic, the public key as user and the secret as
//! password. The `v2` there is the **resource's** version, not the product's:
//! Langfuse v4 versions each resource independently, the prompts resource is at
//! `v2`, and it is absent from Langfuse's own list of endpoints being removed.
//! There is no pre-v4 path in this file and no fallback to one.
//!
//! The secret is an [`ApiKey`] with no `Display` and no `Serialize`, and no
//! [`PromptError`] variant has a field a credential could reach — a test in this
//! crate asserts that against a planted key.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use turnframe_core::prompt::{
    LoadedPrompt, PromptError, PromptName, PromptSelector, PromptSource, PromptVersion,
};
use turnframe_provider::secret::ApiKey;

use crate::cache::CachedPromptSource;

/// Langfuse Cloud in the European Union.
pub const CLOUD_EU: &str = "https://cloud.langfuse.com";

/// Langfuse Cloud in the United States.
pub const CLOUD_US: &str = "https://us.cloud.langfuse.com";

/// The path of the Langfuse v4 prompt resource, appended to the base URL.
///
/// `v2` is the resource's version, not the product's; see the module
/// documentation.
pub const PROMPTS_PATH: &str = "api/public/v2/prompts";

/// How long one fetch may take before it is a [`PromptError::Transport`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A Langfuse source could not be configured.
///
/// `Display` names the field at fault and never its value, because a rejected
/// value may well be the credential that made it invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LangfuseConfigError {
    /// The base URL is not an absolute HTTP or HTTPS URL.
    #[error("the base URL is not usable: {reason}")]
    InvalidBaseUrl {
        /// What is wrong with it. Never the URL itself.
        reason: &'static str,
    },
    /// The base URL carries `user:password@`, which would put a credential in
    /// every log line and trace attribute.
    #[error("the base URL carries user info: pass the keys as public_key and secret_key instead")]
    CredentialInBaseUrl,
    /// No public key was given.
    #[error("no public key: Langfuse authenticates with a public and a secret key")]
    MissingPublicKey,
    /// No secret key was given.
    #[error("no secret key: Langfuse authenticates with a public and a secret key")]
    MissingSecretKey,
    /// The HTTP client could not be built.
    #[error("the HTTP client could not be built")]
    Client,
}

/// Builds a [`LangfusePromptSource`].
///
/// ```
/// use turnframe_prompt::PromptSource;
/// use turnframe_prompt::langfuse::{CLOUD_EU, LangfusePromptSource};
///
/// let source = LangfusePromptSource::builder()
///     .base_url(CLOUD_EU)
///     .public_key("pk-lf-0000")
///     .secret_key("sk-lf-0000")
///     .build()
///     .expect("a complete configuration");
/// assert_eq!(source.describe(), "langfuse");
/// ```
#[derive(Debug, Default)]
pub struct LangfusePromptSourceBuilder {
    base_url: Option<String>,
    public_key: Option<String>,
    secret_key: Option<ApiKey>,
    timeout: Option<Duration>,
}

impl LangfusePromptSourceBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the Langfuse base URL. Defaults to [`CLOUD_EU`].
    ///
    /// A self-hosted deployment passes its own origin here.
    #[must_use]
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Sets the project's public key, which travels as the Basic auth user.
    #[must_use]
    pub fn public_key(mut self, public_key: impl Into<String>) -> Self {
        self.public_key = Some(public_key.into());
        self
    }

    /// Sets the project's secret key, which travels as the Basic auth password.
    #[must_use]
    pub fn secret_key(mut self, secret_key: impl Into<ApiKey>) -> Self {
        self.secret_key = Some(secret_key.into());
        self
    }

    /// Sets the per-request deadline. Defaults to [`DEFAULT_TIMEOUT`].
    ///
    /// It is a deadline inside a user's turn, so it should be short: a prompt
    /// that has not arrived in a few seconds is not going to save the turn.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Builds the source.
    ///
    /// # Errors
    ///
    /// [`LangfuseConfigError`] when the base URL is unusable or a key is
    /// missing. Everything that can be checked without a network is checked
    /// here, so a misconfiguration is a startup failure rather than a failed
    /// turn.
    pub fn build(self) -> Result<LangfusePromptSource, LangfuseConfigError> {
        let base = self.base_url.as_deref().unwrap_or(CLOUD_EU);
        let mut url =
            reqwest::Url::parse(base).map_err(|_| LangfuseConfigError::InvalidBaseUrl {
                reason: "not an absolute URL",
            })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(LangfuseConfigError::InvalidBaseUrl {
                reason: "the scheme is not http or https",
            });
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(LangfuseConfigError::CredentialInBaseUrl);
        }
        {
            let mut segments =
                url.path_segments_mut()
                    .map_err(|()| LangfuseConfigError::InvalidBaseUrl {
                        reason: "the URL cannot carry a path",
                    })?;
            // Drops a trailing empty segment from a base URL written with a
            // trailing slash, so the path does not come out doubled.
            segments.pop_if_empty();
            for segment in PROMPTS_PATH.split('/') {
                segments.push(segment);
            }
        }

        let public_key = self
            .public_key
            .filter(|key| !key.is_empty())
            .ok_or(LangfuseConfigError::MissingPublicKey)?;
        let secret_key = self
            .secret_key
            .filter(|key| !key.is_empty())
            .ok_or(LangfuseConfigError::MissingSecretKey)?;

        let client = reqwest::Client::builder()
            .build()
            .map_err(|_| LangfuseConfigError::Client)?;

        Ok(LangfusePromptSource {
            client,
            prompts_url: url,
            public_key,
            secret_key,
            timeout: self.timeout.unwrap_or(DEFAULT_TIMEOUT),
        })
    }
}

/// A [`PromptSource`] backed by a Langfuse project.
///
/// Compose it with [`CachedPromptSource`] — through
/// [`into_cached`](Self::into_cached) or by hand — so a turn is not one HTTP
/// round trip per prompt, and so a Langfuse outage is survived by whatever
/// version is already held. Read the module documentation before deciding to
/// use it at all.
pub struct LangfusePromptSource {
    client: reqwest::Client,
    /// The URL up to and including `PROMPTS_PATH`; the name is pushed onto it.
    prompts_url: reqwest::Url,
    public_key: String,
    secret_key: ApiKey,
    timeout: Duration,
}

impl fmt::Debug for LangfusePromptSource {
    /// Renders the origin, the public key and a non-reversible fingerprint of
    /// the secret key — never the secret key itself.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LangfusePromptSource")
            .field("origin", &self.prompts_url.origin().ascii_serialization())
            .field("public_key", &self.public_key)
            .field("secret_key", &self.secret_key)
            .field("secret_key_fingerprint", &self.secret_key.fingerprint())
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl LangfusePromptSource {
    /// Starts a builder.
    #[must_use]
    pub fn builder() -> LangfusePromptSourceBuilder {
        LangfusePromptSourceBuilder::new()
    }

    /// The per-request deadline in force.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// A short, non-reversible hint identifying *which* secret key is
    /// configured, for a startup log line. Useless for authenticating.
    #[must_use]
    pub fn credential_fingerprint(&self) -> String {
        self.secret_key.fingerprint()
    }

    /// Wraps this source in a [`CachedPromptSource`] with its default window
    /// and capacity.
    ///
    /// The recommended way to use this adapter: without a cache, every prompt
    /// of every turn is a network round trip, and a Langfuse outage is an
    /// outage here too.
    #[must_use]
    pub fn into_cached(self) -> CachedPromptSource {
        CachedPromptSource::new(Arc::new(self))
    }

    fn request_url(&self, name: &PromptName, selector: &PromptSelector) -> reqwest::Url {
        let mut url = self.prompts_url.clone();
        // `push` percent-encodes, so a name with a slash or a space cannot
        // escape its path segment. The builder already proved this URL has a
        // path, so the `Err` arm is unreachable and leaves the URL as it was.
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.push(name.as_str());
        }
        match selector {
            PromptSelector::Latest => {}
            PromptSelector::Label(label) => {
                url.query_pairs_mut().append_pair("label", label);
            }
            PromptSelector::Version(version) => {
                url.query_pairs_mut()
                    .append_pair("version", version.as_str());
            }
        }
        url
    }
}

#[async_trait::async_trait]
impl PromptSource for LangfusePromptSource {
    async fn load(
        &self,
        name: &PromptName,
        selector: &PromptSelector,
    ) -> Result<LoadedPrompt, PromptError> {
        let response = self
            .client
            .get(self.request_url(name, selector))
            .basic_auth(&self.public_key, Some(self.secret_key.expose()))
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| transport_error(&error))?;

        let status = response.status();
        if !status.is_success() {
            return Err(status_error(status, name, selector));
        }

        let body = response
            .bytes()
            .await
            .map_err(|_| PromptError::Transport { code: "body" })?;
        let payload: PromptPayload =
            serde_json::from_slice(&body).map_err(|_| PromptError::Malformed {
                code: "not_json_prompt",
            })?;
        payload.into_loaded(name, selector)
    }

    fn describe(&self) -> &'static str {
        "langfuse"
    }
}

/// The subset of the Langfuse v4 prompt document this adapter reads.
///
/// Deliberately **not** `deny_unknown_fields`: this is a remote service whose
/// document grows on its own schedule, and refusing a prompt because Langfuse
/// added a field would be an outage caused by a release note. The fields that
/// are read are read strictly.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptPayload {
    /// Ordinal version Langfuse assigned. Missing on no documented response,
    /// but treated as a malformed answer rather than guessed at.
    version: Option<u64>,
    /// `"text"` or `"chat"`; absent on older text prompts.
    #[serde(rename = "type")]
    prompt_type: Option<String>,
    /// A string for a text prompt, an array of role/content objects for a chat
    /// prompt.
    prompt: Option<serde_json::Value>,
}

impl PromptPayload {
    fn into_loaded(
        self,
        name: &PromptName,
        selector: &PromptSelector,
    ) -> Result<LoadedPrompt, PromptError> {
        if self.prompt_type.as_deref() == Some("chat") {
            return Err(PromptError::Unsupported {
                reason: "chat_prompt",
            });
        }
        let version = self.version.ok_or(PromptError::Malformed {
            code: "missing_version",
        })?;
        let text = match self.prompt {
            Some(serde_json::Value::String(text)) => text,
            // An array is a chat prompt whatever `type` said. Flattening it
            // into one string would change its meaning and make the hash the
            // hash of something the registry does not hold.
            Some(serde_json::Value::Array(_)) => {
                return Err(PromptError::Unsupported {
                    reason: "chat_prompt",
                });
            }
            Some(_) => {
                return Err(PromptError::Malformed {
                    code: "prompt_not_text",
                });
            }
            None => {
                return Err(PromptError::Malformed {
                    code: "missing_prompt",
                });
            }
        };

        let version = PromptVersion::new(version.to_string());
        // A pin the registry did not honour is refused rather than served: the
        // whole point of pinning is that nothing else may come back.
        if let PromptSelector::Version(pinned) = selector
            && &version != pinned
        {
            return Err(PromptError::VersionNotFound {
                name: name.clone(),
                version: pinned.clone(),
            });
        }
        Ok(LoadedPrompt::new(name.clone(), version, text))
    }
}

/// Maps a transport failure onto the error family, with a stable code and
/// nothing borrowed from the request.
fn transport_error(error: &reqwest::Error) -> PromptError {
    if error.is_timeout() {
        return PromptError::Transport { code: "timeout" };
    }
    if error.is_connect() {
        return PromptError::Transport { code: "connect" };
    }
    if error.is_decode() {
        return PromptError::Malformed { code: "decode" };
    }
    PromptError::Transport { code: "request" }
}

/// Maps an HTTP status onto the error family.
fn status_error(status: StatusCode, name: &PromptName, selector: &PromptSelector) -> PromptError {
    match status {
        StatusCode::UNAUTHORIZED => PromptError::Unauthorized,
        StatusCode::FORBIDDEN => PromptError::Forbidden { name: name.clone() },
        StatusCode::TOO_MANY_REQUESTS => PromptError::RateLimited,
        StatusCode::NOT_FOUND => match selector {
            PromptSelector::Latest => PromptError::NotFound { name: name.clone() },
            PromptSelector::Label(label) => PromptError::LabelNotFound {
                name: name.clone(),
                label: label.clone(),
            },
            PromptSelector::Version(version) => PromptError::VersionNotFound {
                name: name.clone(),
                version: version.clone(),
            },
        },
        other => PromptError::Transport {
            code: status_label(other),
        },
    }
}

/// A stable code per status, so a metric can group them without a formatted
/// string.
const fn status_label(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "status_400",
        408 => "status_408",
        409 => "status_409",
        500 => "status_500",
        502 => "status_502",
        503 => "status_503",
        504 => "status_504",
        code if code >= 500 => "status_5xx",
        _ => "status_other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built() -> LangfusePromptSource {
        LangfusePromptSource::builder()
            .base_url("https://example.test/")
            .public_key("pk-lf-1")
            .secret_key("sk-lf-planted-0123456789")
            .build()
            .unwrap()
    }

    #[test]
    fn the_url_is_the_v4_prompt_resource_and_the_selector_is_a_query() {
        let source = built();
        let name = PromptName::from("interpret.system");

        let latest = source.request_url(&name, &PromptSelector::Latest);
        assert_eq!(
            latest.as_str(),
            "https://example.test/api/public/v2/prompts/interpret.system"
        );

        let labelled = source.request_url(&name, &PromptSelector::label("production"));
        assert_eq!(
            labelled.as_str(),
            "https://example.test/api/public/v2/prompts/interpret.system?label=production"
        );

        let pinned = source.request_url(&name, &PromptSelector::version("7"));
        assert_eq!(
            pinned.as_str(),
            "https://example.test/api/public/v2/prompts/interpret.system?version=7"
        );
    }

    #[test]
    fn a_name_that_could_escape_its_path_segment_is_encoded() {
        let source = built();
        let url = source.request_url(&PromptName::from("a/b c"), &PromptSelector::Latest);
        assert_eq!(
            url.as_str(),
            "https://example.test/api/public/v2/prompts/a%2Fb%20c"
        );
    }

    #[test]
    fn the_configuration_refuses_what_it_cannot_check_later() {
        assert_eq!(
            LangfusePromptSource::builder()
                .base_url("ftp://example.test")
                .public_key("pk")
                .secret_key("sk")
                .build()
                .unwrap_err(),
            LangfuseConfigError::InvalidBaseUrl {
                reason: "the scheme is not http or https",
            }
        );
        assert_eq!(
            LangfusePromptSource::builder()
                .base_url("not a url")
                .public_key("pk")
                .secret_key("sk")
                .build()
                .unwrap_err(),
            LangfuseConfigError::InvalidBaseUrl {
                reason: "not an absolute URL",
            }
        );
        assert_eq!(
            LangfusePromptSource::builder()
                .base_url("https://pk:sk@example.test")
                .public_key("pk")
                .secret_key("sk")
                .build()
                .unwrap_err(),
            LangfuseConfigError::CredentialInBaseUrl
        );
        assert_eq!(
            LangfusePromptSource::builder()
                .secret_key("sk")
                .build()
                .unwrap_err(),
            LangfuseConfigError::MissingPublicKey
        );
        assert_eq!(
            LangfusePromptSource::builder()
                .public_key("pk")
                .build()
                .unwrap_err(),
            LangfuseConfigError::MissingSecretKey
        );
        assert_eq!(
            LangfusePromptSource::builder()
                .public_key("pk")
                .secret_key("")
                .build()
                .unwrap_err(),
            LangfuseConfigError::MissingSecretKey
        );
    }

    #[test]
    fn debug_output_carries_a_fingerprint_and_never_the_secret() {
        let source = built();
        let rendered = format!("{source:?}");
        assert!(!rendered.contains("sk-lf-planted"), "{rendered}");
        assert!(rendered.contains("REDACTED"));
        assert!(rendered.contains(&source.credential_fingerprint()));
        assert_eq!(source.timeout(), DEFAULT_TIMEOUT);
    }

    #[test]
    fn a_chat_prompt_is_refused_rather_than_flattened() {
        let name = PromptName::from("n");
        let chat = PromptPayload {
            version: Some(1),
            prompt_type: Some("chat".to_owned()),
            prompt: Some(serde_json::json!([{"role": "system", "content": "hi"}])),
        };
        assert_eq!(
            chat.into_loaded(&name, &PromptSelector::Latest)
                .unwrap_err(),
            PromptError::Unsupported {
                reason: "chat_prompt",
            }
        );

        // Even when `type` does not say so.
        let untyped = PromptPayload {
            version: Some(1),
            prompt_type: None,
            prompt: Some(serde_json::json!([{"role": "system", "content": "hi"}])),
        };
        assert_eq!(
            untyped
                .into_loaded(&name, &PromptSelector::Latest)
                .unwrap_err(),
            PromptError::Unsupported {
                reason: "chat_prompt",
            }
        );
    }

    #[test]
    fn a_pin_the_registry_did_not_honour_is_refused() {
        let name = PromptName::from("n");
        let payload = PromptPayload {
            version: Some(9),
            prompt_type: Some("text".to_owned()),
            prompt: Some(serde_json::json!("the text")),
        };
        assert_eq!(
            payload
                .into_loaded(&name, &PromptSelector::version("7"))
                .unwrap_err(),
            PromptError::VersionNotFound {
                name: name.clone(),
                version: PromptVersion::from("7"),
            }
        );

        let honoured = PromptPayload {
            version: Some(7),
            prompt_type: Some("text".to_owned()),
            prompt: Some(serde_json::json!("the text")),
        };
        let loaded = honoured
            .into_loaded(&name, &PromptSelector::version("7"))
            .unwrap();
        assert_eq!(loaded.version().as_str(), "7");
        assert!(loaded.reference().matches("the text"));
    }

    #[test]
    fn a_document_missing_what_is_needed_is_malformed() {
        let name = PromptName::from("n");
        for (payload, code) in [
            (
                PromptPayload {
                    version: None,
                    prompt_type: Some("text".to_owned()),
                    prompt: Some(serde_json::json!("t")),
                },
                "missing_version",
            ),
            (
                PromptPayload {
                    version: Some(1),
                    prompt_type: Some("text".to_owned()),
                    prompt: None,
                },
                "missing_prompt",
            ),
            (
                PromptPayload {
                    version: Some(1),
                    prompt_type: Some("text".to_owned()),
                    prompt: Some(serde_json::json!(42)),
                },
                "prompt_not_text",
            ),
        ] {
            assert_eq!(
                payload
                    .into_loaded(&name, &PromptSelector::Latest)
                    .unwrap_err(),
                PromptError::Malformed { code }
            );
        }
    }

    #[test]
    fn statuses_map_onto_the_family_with_stable_codes() {
        let name = PromptName::from("n");
        assert_eq!(
            status_error(StatusCode::UNAUTHORIZED, &name, &PromptSelector::Latest),
            PromptError::Unauthorized
        );
        assert_eq!(
            status_error(StatusCode::FORBIDDEN, &name, &PromptSelector::Latest),
            PromptError::Forbidden { name: name.clone() }
        );
        assert_eq!(
            status_error(
                StatusCode::TOO_MANY_REQUESTS,
                &name,
                &PromptSelector::Latest
            ),
            PromptError::RateLimited
        );
        assert_eq!(
            status_error(StatusCode::NOT_FOUND, &name, &PromptSelector::Latest),
            PromptError::NotFound { name: name.clone() }
        );
        assert_eq!(
            status_error(
                StatusCode::NOT_FOUND,
                &name,
                &PromptSelector::label("production")
            ),
            PromptError::LabelNotFound {
                name: name.clone(),
                label: "production".to_owned(),
            }
        );
        assert_eq!(
            status_error(StatusCode::NOT_FOUND, &name, &PromptSelector::version("7")),
            PromptError::VersionNotFound {
                name: name.clone(),
                version: PromptVersion::from("7"),
            }
        );
        assert_eq!(
            status_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &name,
                &PromptSelector::Latest
            ),
            PromptError::Transport { code: "status_500" }
        );
        assert_eq!(
            status_label(StatusCode::from_u16(507).unwrap()),
            "status_5xx"
        );
        assert_eq!(
            status_label(StatusCode::from_u16(418).unwrap()),
            "status_other"
        );
    }
}
