//! Where a Vertex AI access token comes from (spec §25.2).
//!
//! The developer API authenticates with a long-lived API key, which is an
//! [`ApiKey`] and nothing more. Vertex AI authenticates with a short-lived
//! OAuth 2.0 access token, and that is a different problem: the token expires,
//! so something has to mint a new one.
//!
//! # Why this crate does not mint it
//!
//! This adapter takes the token **already obtained**, through a
//! [`TokenSource`]. It embeds no Google authentication library, and that is a
//! deliberate choice rather than an omission:
//!
//! * **Acquiring a Google credential is an ambient concern of the deployment,
//!   not of the model call.** On GKE and Cloud Run it is the metadata server,
//!   in CI it is workload identity federation, on a laptop it is application
//!   default credentials, and in a regulated fleet it is a broker the security
//!   team owns. An adapter that picked one would be imposing a deployment
//!   policy from inside a translation layer.
//! * **Spec §25.2 puts external credentials outside the model layer.** Command
//!   handlers own them; a provider adapter receives a secret wrapper and never
//!   reads a service-account key, signs a JWT, or touches the filesystem or the
//!   metadata endpoint. Keeping the acquisition outside is what makes that
//!   structural.
//! * **Refresh policy belongs to the adopter.** How early to renew, whether one
//!   token is shared across a fleet of provider instances, and what happens
//!   when the broker is down are fleet decisions, invisible from here.
//! * **Nobody pays for what they do not use.** A build that only speaks the
//!   developer API would still carry a cloud authentication tree, and often a
//!   second TLS stack with it.
//!
//! # Supplying one
//!
//! [`StaticToken`] wraps a token you already hold. [`TokenFn`] wraps an async
//! closure, which is the shape most adopters want: call your own credential
//! provider, hand back an [`ApiKey`]. The source is consulted **once per
//! request**, immediately before dispatch, so a cached token that has just
//! expired is renewed on the next call rather than on the next restart.
//!
//! ```
//! use turnframe_provider::secret::ApiKey;
//! use turnframe_provider_gemini::credential::{TokenFn, TokenSource};
//!
//! // In a real deployment this asks your credential provider.
//! let source = TokenFn::new(|| async { Ok(ApiKey::new("ya29.not-a-real-token")) });
//!
//! futures::executor::block_on(async {
//!     let token = source.access_token().await.expect("a token");
//!     assert_eq!(token.fingerprint().len(), 8);
//! });
//! ```

use std::fmt;
use std::future::Future;

use async_trait::async_trait;
use turnframe_provider::error::{ErrorCode, ProviderError};
use turnframe_provider::secret::ApiKey;

/// A token source could not produce a usable access token.
///
/// `Display` names a short sanitized code and never the token, the broker's
/// response body or a URL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TokenError {
    /// The source has no credential to offer right now.
    #[error("no Vertex AI access token is available")]
    Unavailable,
    /// The source failed. `code` is the adopter's own short label.
    #[error("the Vertex AI token source failed: {code}")]
    Failed {
        /// A sanitized label for the failure, e.g. `"metadata_server_timeout"`.
        code: ErrorCode,
    },
}

impl TokenError {
    /// A failure carrying a short label.
    ///
    /// The label is sanitized by [`ErrorCode::new`], so a broker response
    /// pasted in by mistake cannot survive readable.
    #[must_use]
    pub fn failed(code: impl AsRef<str>) -> Self {
        Self::Failed {
            code: ErrorCode::new(code),
        }
    }

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unavailable => "token_unavailable",
            Self::Failed { .. } => "token_source_failed",
        }
    }
}

impl From<TokenError> for ProviderError {
    /// A token that cannot be obtained is an authentication failure, whose
    /// retry class is `Fallback`: waiting will not help, and another configured
    /// candidate may well have a working credential.
    fn from(value: TokenError) -> Self {
        let error = Self::authentication();
        match value {
            TokenError::Unavailable => error.with_code("token_unavailable"),
            TokenError::Failed { code } => error.with_code(code.as_str()),
        }
    }
}

/// Supplies the OAuth 2.0 access token Vertex AI authenticates with.
///
/// Implement it over whatever already holds your Google credentials. The
/// adapter calls it once per request and puts the result straight into an
/// `Authorization: Bearer` header marked sensitive; it never stores the token,
/// never logs it and never renders it.
///
/// `Debug` is a supertrait because the adapter's own `Debug` includes the
/// source. Render configuration, never a token: [`StaticToken`] shows a
/// fingerprint, and that is the pattern to copy.
#[async_trait]
pub trait TokenSource: Send + Sync + fmt::Debug {
    /// Returns a token that is valid now.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError`] when no token can be produced.
    async fn access_token(&self) -> Result<ApiKey, TokenError>;
}

/// A token that is handed over once and never refreshed.
///
/// Right for a test, a short-lived job or a process whose supervisor restarts
/// it more often than a Google access token expires. Wrong for a long-running
/// service: use [`TokenFn`] or your own [`TokenSource`] there, because this one
/// will start returning 401s an hour in.
pub struct StaticToken(ApiKey);

impl StaticToken {
    /// Wraps a token.
    #[must_use]
    pub const fn new(token: ApiKey) -> Self {
        Self(token)
    }

    /// A short, non-reversible hint identifying which token is configured.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        self.0.fingerprint()
    }
}

impl fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticToken")
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
}

#[async_trait]
impl TokenSource for StaticToken {
    async fn access_token(&self) -> Result<ApiKey, TokenError> {
        if self.0.is_empty() {
            return Err(TokenError::Unavailable);
        }
        Ok(self.0.clone())
    }
}

/// A [`TokenSource`] backed by an async closure.
///
/// The closure is called once per request, so it is the right place to consult
/// a cache and refresh only when the cached token is close to expiry.
///
/// ```
/// use turnframe_provider::secret::ApiKey;
/// use turnframe_provider_gemini::credential::{TokenError, TokenFn, TokenSource};
///
/// let source = TokenFn::new(|| async {
///     // Ask whatever already owns your Google credentials.
///     std::env::var("VERTEX_ACCESS_TOKEN")
///         .map(ApiKey::new)
///         .map_err(|_| TokenError::failed("env_var_missing"))
/// });
/// assert_eq!(format!("{source:?}"), "TokenFn(..)");
/// ```
pub struct TokenFn<F>(F);

impl<F> TokenFn<F> {
    /// Wraps the closure.
    #[must_use]
    pub const fn new(source: F) -> Self {
        Self(source)
    }
}

impl<F> fmt::Debug for TokenFn<F> {
    /// Renders nothing about the closure: it captures whatever the adopter
    /// captured, which may well be a credential.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenFn(..)")
    }
}

#[async_trait]
impl<F, Fut> TokenSource for TokenFn<F>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = Result<ApiKey, TokenError>> + Send,
{
    async fn access_token(&self) -> Result<ApiKey, TokenError> {
        (self.0)().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_provider::error::ProviderErrorKind;

    const PLANTED: &str = "ya29.planted-0123456789abcdefghij";

    #[tokio::test]
    async fn a_static_token_round_trips_and_never_renders_itself() {
        let source = StaticToken::new(ApiKey::new(PLANTED));
        let token = source.access_token().await.expect("a token");
        assert_eq!(token.expose(), PLANTED);

        let rendered = format!("{source:?}");
        assert!(!rendered.contains("ya29.planted"), "{rendered}");
        assert!(rendered.contains(&source.fingerprint()), "{rendered}");
        assert_eq!(source.fingerprint().len(), 8);
    }

    #[tokio::test]
    async fn an_empty_static_token_is_unavailable_rather_than_a_401_later() {
        let source = StaticToken::new(ApiKey::new(""));
        assert_eq!(
            source.access_token().await.expect_err("empty"),
            TokenError::Unavailable
        );
    }

    #[tokio::test]
    async fn a_closure_source_is_consulted_every_time() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let source = TokenFn::new(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(ApiKey::new(PLANTED))
            }
        });
        for _ in 0..3 {
            source.access_token().await.expect("a token");
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(format!("{source:?}"), "TokenFn(..)");
    }

    #[tokio::test]
    async fn a_failing_source_becomes_a_fallback_class_authentication_failure() {
        // A broker response pasted in where a label belongs is mangled by
        // `ErrorCode`, so it cannot reach a log line as readable JSON.
        let source = TokenFn::new(|| async {
            Err::<ApiKey, _>(TokenError::failed(
                "{\"error\": \"invalid_grant\", \"detail\": \"see log\"}",
            ))
        });
        let failure = source.access_token().await.expect_err("fails");
        assert_eq!(failure.as_str(), "token_source_failed");
        let rendered = failure.to_string();
        assert!(!rendered.contains('"'), "{rendered}");
        assert!(!rendered.contains("see log"), "{rendered}");

        let error = ProviderError::from(failure);
        assert!(matches!(error.kind(), ProviderErrorKind::Authentication));
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fallback
        );
        assert!(!error.to_string().contains("see log"), "{error}");

        let unavailable = ProviderError::from(TokenError::Unavailable);
        assert_eq!(
            unavailable.code().map(|code| code.as_str().to_owned()),
            Some("token_unavailable".to_owned())
        );
        assert_eq!(TokenError::Unavailable.as_str(), "token_unavailable");
    }

    #[tokio::test]
    async fn the_trait_is_object_safe() {
        let source: std::sync::Arc<dyn TokenSource> =
            std::sync::Arc::new(StaticToken::new(ApiKey::new(PLANTED)));
        assert_eq!(
            source.access_token().await.expect("a token").len(),
            PLANTED.len()
        );
    }
}
