//! The typed provider failure family and its retry classification (spec §24, §20.7).
//!
//! Every adapter maps its vendor errors into a [`ProviderError`]: a [`ProviderErrorKind`]
//! with its classification payload, the provider and model keys, and an optional short
//! code. Nothing from the wire appears in one: `Display` and `Debug` render the kind, the
//! keys and a code [`ErrorCode::new`] sanitized, never a body, a header, a prompt, user text
//! or a secret (spec §25.2).
//!
//! [`ProviderError::retry_class`] says what the caller may do next. Whether it may retry at
//! all is [`FallbackStage`](crate::fallback::FallbackStage)'s to decide (I17).
//!
//! | [`RetryClass`] | Meaning |
//! |----------------|---------|
//! | [`Retry`](RetryClass::Retry) | The same provider may be tried again after a backoff. |
//! | [`RetryAfter`](RetryClass::RetryAfter) | The same provider may be tried again, but only after the delay the provider asked for. |
//! | [`Fallback`](RetryClass::Fallback) | This provider will keep failing; move to the next candidate that satisfies the **same** requirements. |
//! | [`Fatal`](RetryClass::Fatal) | Neither retrying nor falling back can help; fail the stage. |
//!
//! Three kinds look alike from a status code and are not; the conformance suite has a row
//! for each:
//!
//! | Looks like | Actually | Why it matters |
//! |------------|----------|----------------|
//! | [`Authentication`](ProviderErrorKind::Authentication) | [`CredentialExpired`](ProviderErrorKind::CredentialExpired) | A wrong key stays wrong; an expired token works again after a refresh. Providers that issue short-lived credentials (Vertex AI bearer tokens, Bedrock session credentials) fail this way routinely. |
//! | [`RateLimited`](ProviderErrorKind::RateLimited) | [`QuotaExhausted`](ProviderErrorKind::QuotaExhausted) | A rate limit clears by waiting; a quota or an empty balance does not, and several providers report both with HTTP 429. |
//! | [`InvalidRequest`](ProviderErrorKind::InvalidRequest) | [`ContextOverflow`](ProviderErrorKind::ContextOverflow) | Both arrive as HTTP 400; only one is fixed by shrinking the prompt. |

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::capabilities::CapabilityMismatch;
use crate::ids::{ModelKey, ModelRef, ProviderKey};

/// Maximum length of a sanitized [`ErrorCode`], in bytes.
pub const MAX_ERROR_CODE_LEN: usize = 64;

/// Longest sanitized [`ProviderDetail`], in bytes.
pub const MAX_ERROR_DETAIL_LEN: usize = 512;

/// The endpoint's own sentence about why it refused, sanitized.
///
/// # Why this exists beside [`ErrorCode`]
///
/// A code says which family a failure belongs to. It cannot say *what was
/// wrong with the request*, and for the one family where that is the adopter's
/// own bug — a malformed request — the difference between "the provider is
/// down" and "your function schema is missing `properties`" is the difference
/// between waiting and fixing. Finding the second took putting a proxy between
/// the process and the endpoint to read a body the runtime had already read
/// and thrown away.
///
/// # What it is not
///
/// It is not shown to a user and it is not in [`ProviderError`]'s `Display`,
/// which keeps its promise that what it renders is safe to log unconditionally.
/// This is the vendor's words about a request this library sent: normally about
/// the request's shape, and in principle able to quote a value from it. An
/// adapter passes it through the same [`Redactor`](crate::secret::Redactor) the
/// code goes through, and construction here drops control characters, collapses
/// runs of whitespace and truncates on a character boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderDetail(String);

impl ProviderDetail {
    /// Sanitizes and truncates `raw` into a detail.
    ///
    /// ```
    /// use turnframe_provider::error::ProviderDetail;
    ///
    /// let detail = ProviderDetail::new("Invalid schema for function 'x':\n  missing properties");
    /// assert_eq!(
    ///     detail.as_str(),
    ///     "Invalid schema for function 'x': missing properties"
    /// );
    /// ```
    #[must_use]
    pub fn new(raw: impl AsRef<str>) -> Self {
        let mut out = String::with_capacity(MAX_ERROR_DETAIL_LEN);
        let mut spaced = false;
        for ch in raw.as_ref().chars() {
            if out.len() + ch.len_utf8() > MAX_ERROR_DETAIL_LEN {
                break;
            }
            if ch.is_whitespace() {
                if !out.is_empty() && !spaced {
                    out.push(' ');
                    spaced = true;
                }
                continue;
            }
            if ch.is_control() {
                continue;
            }
            out.push(ch);
            spaced = false;
        }
        Self(out.trim_end().to_owned())
    }

    /// The sanitized text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether anything survived sanitizing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ProviderDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A short, sanitized machine code an adapter attaches to a failure.
///
/// Construction is the redaction point: only `[A-Za-z0-9_.:-]` survives, every
/// other character becomes `_`, and the value is truncated to
/// [`MAX_ERROR_CODE_LEN`]. A code is meant to be a stable label such as
/// `"invalid_api_key"` or `"model_overloaded"`, never a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ErrorCode(String);

impl ErrorCode {
    /// Sanitizes and truncates `raw` into a code.
    ///
    /// ```
    /// use turnframe_provider::error::ErrorCode;
    ///
    /// assert_eq!(ErrorCode::new("invalid_api_key").as_str(), "invalid_api_key");
    /// // A leaked body cannot survive readable.
    /// assert_eq!(ErrorCode::new("{\"key\":\"sk-abc\"}").as_str(), "__key_:_sk-abc__");
    /// ```
    #[must_use]
    pub fn new(raw: impl AsRef<str>) -> Self {
        let mut out = String::with_capacity(MAX_ERROR_CODE_LEN);
        for ch in raw.as_ref().chars() {
            if out.len() >= MAX_ERROR_CODE_LEN {
                break;
            }
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-') {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
        Self(out)
    }

    /// Borrows the sanitized code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the caller may do after a failure (spec §20.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    /// Retry the same provider after the policy's backoff.
    Retry,
    /// Retry the same provider, but not before the delay the provider asked
    /// for (see [`ProviderErrorKind::retry_after`]).
    RetryAfter,
    /// Do not retry this provider; move to the next candidate.
    Fallback,
    /// Fail the stage.
    Fatal,
}

impl RetryClass {
    /// Every class, in decreasing order of hope.
    pub const ALL: [Self; 4] = [Self::Retry, Self::RetryAfter, Self::Fallback, Self::Fatal];

    /// Stable snake-case label for metrics and replay records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::RetryAfter => "retry_after",
            Self::Fallback => "fallback",
            Self::Fatal => "fatal",
        }
    }

    /// Returns `true` when the same provider may be called again.
    #[must_use]
    pub const fn allows_same_provider(self) -> bool {
        matches!(self, Self::Retry | Self::RetryAfter)
    }

    /// Returns `true` when another candidate may be tried.
    #[must_use]
    pub const fn allows_another_candidate(self) -> bool {
        matches!(self, Self::Retry | Self::RetryAfter | Self::Fallback)
    }
}

impl fmt::Display for RetryClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The failure families an adapter maps its vendor errors into.
///
/// Growable: match with a `_` arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderErrorKind {
    /// The call exceeded [`ModelRequest::timeout`](crate::request::ModelRequest::timeout)
    /// or the transport deadline. The model call itself has no effect, so a
    /// timeout here is safe to retry before commit.
    Timeout,
    /// The provider rate-limited the request.
    RateLimited {
        /// The delay the provider asked for, when it supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after: Option<Duration>,
    },
    /// The credentials were rejected as invalid — a wrong or revoked key.
    ///
    /// A key that was valid and has *expired* is
    /// [`CredentialExpired`](Self::CredentialExpired) instead, because the two
    /// call for opposite reactions.
    Authentication,
    /// The credentials are valid but not entitled to this model or endpoint.
    Authorization,
    /// A credential that was valid has expired.
    ///
    /// Deliberately not [`Authentication`](Self::Authentication). A wrong key
    /// stays wrong; an expired one becomes valid again the moment it is
    /// refreshed, and several providers issue short-lived credentials as a
    /// matter of design — Vertex AI bearer tokens and Bedrock session
    /// credentials both expire on a schedule. Folding the two together forces
    /// one of two wrong behaviours: a caller that could refresh and continue
    /// gives up, or a caller that cannot refresh retries a credential that
    /// will never work again.
    ///
    /// Its class is [`Fallback`](RetryClass::Fallback), which reads as "not
    /// with this credential as it stands". A caller **holding a refresher** may
    /// refresh and retry the same profile; a caller **without one** must treat
    /// it as fatal for that profile and move on. It is never
    /// [`Retry`](RetryClass::Retry): retrying the same expired token is a busy
    /// loop.
    CredentialExpired,
    /// The account's quota or credit balance is exhausted.
    ///
    /// Deliberately not [`RateLimited`](Self::RateLimited), **even when the
    /// provider reports it with HTTP 429**, which several do. A rate limit
    /// means "wait and it will work"; an exhausted quota or an empty credit
    /// balance means it will not work until a window resets or a human tops the
    /// account up — hours or days, not seconds. Sleeping on it burns the turn's
    /// deadline for nothing, which is why its class is
    /// [`Fallback`](RetryClass::Fallback): the router moves to another
    /// candidate instead of waiting.
    QuotaExhausted {
        /// The quota the provider named, when it named one, as a short
        /// sanitized code: `"tokens_per_day"`, `"credit_balance"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<ErrorCode>,
    },
    /// The provider rejected the request as malformed. Our request is wrong;
    /// another provider will reject it too.
    InvalidRequest,
    /// The prompt exceeded the model's context window.
    ContextOverflow {
        /// Tokens the provider says the request needed, when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        needed_tokens: Option<u64>,
        /// The model's limit, when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit_tokens: Option<u64>,
    },
    /// The configured model does not exist at this provider.
    ModelNotFound,
    /// The model declined to answer. A semantic outcome, not a transport fault.
    Refusal,
    /// The provider's safety filter blocked the prompt or the completion.
    ContentFilter,
    /// The response could not be normalized: unparseable body, a tool-call
    /// fragment that is not JSON, a stream that ended without a finish event.
    Malformed,
    /// The request never got a complete answer from the network.
    Transport,
    /// The provider returned a server-side error.
    Server {
        /// HTTP status, when there was one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
    },
    /// The caller cancelled the call.
    Cancelled,
    /// The provider-model pair does not satisfy the requirements of the stage
    /// (spec §0 rule 9). Never resolved by weakening the requirements.
    CapabilityMismatch {
        /// Exactly which requirements were unmet.
        mismatch: CapabilityMismatch,
    },
    /// The adapter cannot serve a feature the request asked for (streaming, a
    /// content part, a tool-choice mode).
    Unsupported {
        /// Name of the feature, e.g. `"streaming"`.
        feature: ErrorCode,
    },
    /// Anything the adapter could not place. Fails closed.
    Other,
}

impl ProviderErrorKind {
    /// Stable snake-case label for metrics, replay records and reports.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::RateLimited { .. } => "rate_limited",
            Self::Authentication => "authentication",
            Self::Authorization => "authorization",
            Self::CredentialExpired => "credential_expired",
            Self::QuotaExhausted { .. } => "quota_exhausted",
            Self::InvalidRequest => "invalid_request",
            Self::ContextOverflow { .. } => "context_overflow",
            Self::ModelNotFound => "model_not_found",
            Self::Refusal => "refusal",
            Self::ContentFilter => "content_filter",
            Self::Malformed => "malformed",
            Self::Transport => "transport",
            Self::Server { .. } => "server",
            Self::Cancelled => "cancelled",
            Self::CapabilityMismatch { .. } => "capability_mismatch",
            Self::Unsupported { .. } => "unsupported",
            Self::Other => "other",
        }
    }

    /// The delay the provider asked for, when this kind carries one.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }

    /// How the caller may proceed.
    ///
    /// The reasoning behind the less obvious rows:
    ///
    /// * `ContextOverflow` is [`Fatal`](RetryClass::Fatal): the same prompt is
    ///   too long for the candidate that was already judged large enough, so the
    ///   runtime must shrink the context rather than shop for a bigger window
    ///   mid-flight.
    /// * `Refusal` and `ContentFilter` are [`Fatal`](RetryClass::Fatal):
    ///   retrying elsewhere until a model complies is a safety bypass, not a
    ///   recovery.
    /// * `Authentication`, `Authorization` and `ModelNotFound` are
    ///   [`Fallback`](RetryClass::Fallback): they are configuration faults of
    ///   one profile, and another candidate may be configured correctly.
    /// * `CredentialExpired` is [`Fallback`](RetryClass::Fallback) rather than
    ///   [`Retry`](RetryClass::Retry): the same credential will keep failing
    ///   until something outside this call refreshes it. A caller that owns a
    ///   refresher may refresh and call the same profile again; a caller that
    ///   does not must treat it as fatal for that profile.
    /// * `QuotaExhausted` is [`Fallback`](RetryClass::Fallback) and never
    ///   [`RetryAfter`](RetryClass::RetryAfter), whatever status it arrived on:
    ///   waiting does not refill a quota.
    /// * `CapabilityMismatch` is [`Fallback`](RetryClass::Fallback) because the
    ///   router only ever offers candidates that satisfy the same requirements;
    ///   moving on is not a downgrade.
    /// * `Malformed` is [`Retry`](RetryClass::Retry): a re-roll of the same
    ///   request is the standard remedy for a decode that went off the rails.
    #[must_use]
    pub const fn retry_class(&self) -> RetryClass {
        match self {
            Self::Timeout | Self::Malformed | Self::Transport | Self::Server { .. } => {
                RetryClass::Retry
            }
            Self::RateLimited { .. } => RetryClass::RetryAfter,
            Self::Authentication
            | Self::Authorization
            | Self::CredentialExpired
            | Self::QuotaExhausted { .. }
            | Self::ModelNotFound
            | Self::CapabilityMismatch { .. }
            | Self::Unsupported { .. } => RetryClass::Fallback,
            Self::InvalidRequest
            | Self::ContextOverflow { .. }
            | Self::Refusal
            | Self::ContentFilter
            | Self::Cancelled
            | Self::Other => RetryClass::Fatal,
        }
    }

    /// Maps onto the normalized code the core error family stores.
    #[must_use]
    pub const fn core_code(&self) -> turnframe_core::error::ProviderFailureCode {
        use turnframe_core::error::ProviderFailureCode as Code;
        match self {
            Self::Timeout => Code::Timeout,
            Self::RateLimited { .. } => Code::RateLimited,
            Self::Authentication | Self::Authorization => Code::Authentication,
            Self::CredentialExpired => Code::CredentialExpired,
            Self::QuotaExhausted { .. } => Code::QuotaExhausted,
            Self::ContextOverflow { .. } => Code::ContextOverflow,
            Self::Malformed => Code::Malformed,
            Self::Refusal => Code::Refusal,
            Self::CapabilityMismatch { .. } => Code::CapabilityMismatch,
            Self::Cancelled => Code::Cancelled,
            Self::Server { .. } | Self::Transport => Code::ServerError,
            Self::InvalidRequest
            | Self::ModelNotFound
            | Self::ContentFilter
            | Self::Unsupported { .. }
            | Self::Other => Code::Other,
        }
    }
}

impl fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())?;
        match self {
            Self::RateLimited {
                retry_after: Some(delay),
            } => write!(f, "(retry_after={}s)", delay.as_secs()),
            Self::ContextOverflow {
                needed_tokens: Some(needed),
                limit_tokens: Some(limit),
            } => write!(f, "(needed={needed}, limit={limit})"),
            Self::Server {
                status: Some(status),
            } => write!(f, "(status={status})"),
            Self::QuotaExhausted { scope: Some(scope) } => write!(f, "({scope})"),
            Self::CapabilityMismatch { mismatch } => write!(f, "({mismatch})"),
            Self::Unsupported { feature } => write!(f, "({feature})"),
            _ => Ok(()),
        }
    }
}

/// A normalized provider failure.
///
/// Built with the constructors below and enriched with
/// [`with_model`](Self::with_model) once the caller knows which candidate
/// produced it. `Display` **and** `Debug` are safe to log: the endpoint's own
/// sentence is carried but rendered by neither, and is read deliberately
/// through [`detail`](Self::detail). See [`ProviderDetail`].
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderError {
    kind: ProviderErrorKind,
    provider: Option<ProviderKey>,
    model: Option<ModelKey>,
    code: Option<ErrorCode>,
    /// Boxed because a `ProviderError` travels in the `Err` half of every
    /// provider call, so its size is the size of every one of those returns —
    /// and this is the largest thing on it and the least often read.
    detail: Option<Box<ProviderDetail>>,
}

impl ProviderError {
    /// Builds an error from a kind.
    #[must_use]
    pub const fn new(kind: ProviderErrorKind) -> Self {
        Self {
            kind,
            provider: None,
            model: None,
            code: None,
            detail: None,
        }
    }

    /// The call did not complete in time.
    #[must_use]
    pub const fn timeout() -> Self {
        Self::new(ProviderErrorKind::Timeout)
    }

    /// The provider rate-limited the request.
    #[must_use]
    pub const fn rate_limited(retry_after: Option<Duration>) -> Self {
        Self::new(ProviderErrorKind::RateLimited { retry_after })
    }

    /// The credentials were rejected.
    #[must_use]
    pub const fn authentication() -> Self {
        Self::new(ProviderErrorKind::Authentication)
    }

    /// The credentials are not entitled to this model.
    #[must_use]
    pub const fn authorization() -> Self {
        Self::new(ProviderErrorKind::Authorization)
    }

    /// A credential that was valid has expired and must be refreshed.
    ///
    /// Use this, not [`authentication`](Self::authentication), whenever the
    /// provider distinguishes the two — a rejected key and an expired token
    /// call for opposite reactions from whatever holds the credential.
    #[must_use]
    pub const fn credential_expired() -> Self {
        Self::new(ProviderErrorKind::CredentialExpired)
    }

    /// The account's quota or credit balance is exhausted.
    ///
    /// `scope` names the quota when the provider names one. Use this, not
    /// [`rate_limited`](Self::rate_limited), even when the provider reports it
    /// with HTTP 429.
    #[must_use]
    pub fn quota_exhausted(scope: Option<&str>) -> Self {
        Self::new(ProviderErrorKind::QuotaExhausted {
            scope: scope.map(ErrorCode::new),
        })
    }

    /// The provider rejected our request shape.
    #[must_use]
    pub fn invalid_request(code: impl AsRef<str>) -> Self {
        Self::new(ProviderErrorKind::InvalidRequest).with_code(code)
    }

    /// The prompt did not fit the context window.
    #[must_use]
    pub const fn context_overflow(needed_tokens: Option<u64>, limit_tokens: Option<u64>) -> Self {
        Self::new(ProviderErrorKind::ContextOverflow {
            needed_tokens,
            limit_tokens,
        })
    }

    /// The configured model does not exist at this provider.
    #[must_use]
    pub const fn model_not_found() -> Self {
        Self::new(ProviderErrorKind::ModelNotFound)
    }

    /// The model declined to answer.
    #[must_use]
    pub const fn refusal() -> Self {
        Self::new(ProviderErrorKind::Refusal)
    }

    /// The provider's safety filter blocked the exchange.
    #[must_use]
    pub const fn content_filter() -> Self {
        Self::new(ProviderErrorKind::ContentFilter)
    }

    /// The response could not be normalized. `code` says which step failed.
    #[must_use]
    pub fn malformed(code: impl AsRef<str>) -> Self {
        Self::new(ProviderErrorKind::Malformed).with_code(code)
    }

    /// The network call did not complete.
    #[must_use]
    pub fn transport(code: impl AsRef<str>) -> Self {
        Self::new(ProviderErrorKind::Transport).with_code(code)
    }

    /// The provider failed on its side.
    #[must_use]
    pub const fn server(status: Option<u16>) -> Self {
        Self::new(ProviderErrorKind::Server { status })
    }

    /// The caller cancelled the call.
    #[must_use]
    pub const fn cancelled() -> Self {
        Self::new(ProviderErrorKind::Cancelled)
    }

    /// The provider-model pair does not satisfy the stage's requirements.
    #[must_use]
    pub fn capability_mismatch(mismatch: CapabilityMismatch) -> Self {
        Self::new(ProviderErrorKind::CapabilityMismatch { mismatch })
    }

    /// The adapter cannot serve a requested feature.
    #[must_use]
    pub fn unsupported(feature: impl AsRef<str>) -> Self {
        Self::new(ProviderErrorKind::Unsupported {
            feature: ErrorCode::new(feature),
        })
    }

    /// An unclassified failure. Treated as [`RetryClass::Fatal`].
    #[must_use]
    pub fn other(code: impl AsRef<str>) -> Self {
        Self::new(ProviderErrorKind::Other).with_code(code)
    }

    /// Attaches the endpoint's own sentence, sanitized.
    ///
    /// Pass it through the adapter's [`Redactor`](crate::secret::Redactor)
    /// first, the way a code is. Nothing is attached when nothing survives
    /// sanitizing.
    #[must_use]
    pub fn with_detail(mut self, detail: impl AsRef<str>) -> Self {
        let detail = ProviderDetail::new(detail);
        if !detail.is_empty() {
            self.detail = Some(Box::new(detail));
        }
        self
    }

    /// The endpoint's own sentence, when the adapter had one.
    ///
    /// Deliberately not in `Display`: see [`ProviderDetail`].
    #[must_use]
    pub fn detail(&self) -> Option<&ProviderDetail> {
        self.detail.as_deref()
    }

    /// Attaches a sanitized adapter code.
    #[must_use]
    pub fn with_code(mut self, code: impl AsRef<str>) -> Self {
        self.code = Some(ErrorCode::new(code));
        self
    }

    /// Attaches the provider key.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Attaches provider and model keys at once.
    #[must_use]
    pub fn with_model(mut self, model: &ModelRef) -> Self {
        self.provider = Some(model.provider.clone());
        self.model = Some(model.model.clone());
        self
    }

    /// The failure family.
    #[must_use]
    pub fn kind(&self) -> &ProviderErrorKind {
        &self.kind
    }

    /// The provider key, when known.
    #[must_use]
    pub fn provider(&self) -> Option<&ProviderKey> {
        self.provider.as_ref()
    }

    /// The model key, when known.
    #[must_use]
    pub fn model(&self) -> Option<&ModelKey> {
        self.model.as_ref()
    }

    /// The adapter's sanitized code, when it supplied one.
    #[must_use]
    pub fn code(&self) -> Option<&ErrorCode> {
        self.code.as_ref()
    }

    /// How the caller may proceed.
    #[must_use]
    pub const fn retry_class(&self) -> RetryClass {
        self.kind.retry_class()
    }

    /// The delay the provider asked for, when it supplied one.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.kind.retry_after()
    }

    /// Returns `true` when the same provider may be called again.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.retry_class().allows_same_provider()
    }

    /// Converts into the normalized failure the core error family stores.
    #[must_use]
    pub fn to_core_failure(&self) -> turnframe_core::error::ProviderFailure {
        turnframe_core::error::ProviderFailure {
            provider_key: self
                .provider
                .clone()
                .unwrap_or_else(|| ProviderKey::from("unknown")),
            model_key: self.model.clone(),
            code: self.kind.core_code(),
            retryable: self.is_retryable(),
            detail: self
                .detail
                .as_ref()
                .map(|detail| detail.as_str().to_owned()),
        }
    }
}

impl fmt::Debug for ProviderError {
    /// Renders everything `Display` does, and says only *whether* an endpoint
    /// sentence is attached.
    ///
    /// Hand-written rather than derived because a derived one would print the
    /// sentence, and `{:?}` on an error is the most common way a body reaches
    /// a log by accident. The value is still there for a caller that asks for
    /// it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderError")
            .field("kind", &self.kind)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("code", &self.code)
            .field("detail", &self.detail.is_some())
            .finish()
    }
}

impl fmt::Display for ProviderError {
    /// Renders kind, keys and code — never a body, a header or user text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("provider call failed: ")?;
        fmt::Display::fmt(&self.kind, f)?;
        match (&self.provider, &self.model) {
            (Some(provider), Some(model)) => write!(f, " [{provider}/{model}]")?,
            (Some(provider), None) => write!(f, " [{provider}]")?,
            _ => {}
        }
        if let Some(code) = &self.code {
            write!(f, " code={code}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

impl From<ProviderError> for turnframe_core::error::ProviderFailure {
    fn from(value: ProviderError) -> Self {
        value.to_core_failure()
    }
}

impl From<CapabilityMismatch> for ProviderError {
    fn from(value: CapabilityMismatch) -> Self {
        Self::capability_mismatch(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{MissingCapability, StructuredOutputCapability};

    #[test]
    fn every_kind_has_a_class_and_a_unique_label() {
        let kinds = [
            ProviderErrorKind::Timeout,
            ProviderErrorKind::RateLimited { retry_after: None },
            ProviderErrorKind::Authentication,
            ProviderErrorKind::Authorization,
            ProviderErrorKind::CredentialExpired,
            ProviderErrorKind::QuotaExhausted { scope: None },
            ProviderErrorKind::InvalidRequest,
            ProviderErrorKind::ContextOverflow {
                needed_tokens: None,
                limit_tokens: None,
            },
            ProviderErrorKind::ModelNotFound,
            ProviderErrorKind::Refusal,
            ProviderErrorKind::ContentFilter,
            ProviderErrorKind::Malformed,
            ProviderErrorKind::Transport,
            ProviderErrorKind::Server { status: None },
            ProviderErrorKind::Cancelled,
            ProviderErrorKind::CapabilityMismatch {
                mismatch: CapabilityMismatch {
                    missing: vec![MissingCapability::Streaming],
                },
            },
            ProviderErrorKind::Unsupported {
                feature: ErrorCode::new("streaming"),
            },
            ProviderErrorKind::Other,
        ];
        let mut labels: Vec<&str> = kinds.iter().map(ProviderErrorKind::as_str).collect();
        assert_eq!(labels.len(), 18);
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 18, "labels must be unique");
        for kind in &kinds {
            assert!(RetryClass::ALL.contains(&kind.retry_class()));
        }
    }

    #[test]
    fn classification_matches_the_documented_table() {
        assert_eq!(ProviderError::timeout().retry_class(), RetryClass::Retry);
        assert_eq!(
            ProviderError::malformed("bad_json").retry_class(),
            RetryClass::Retry
        );
        assert_eq!(
            ProviderError::server(Some(503)).retry_class(),
            RetryClass::Retry
        );
        assert_eq!(
            ProviderError::rate_limited(Some(Duration::from_secs(3))).retry_class(),
            RetryClass::RetryAfter
        );
        assert_eq!(
            ProviderError::authentication().retry_class(),
            RetryClass::Fallback
        );
        assert_eq!(
            ProviderError::model_not_found().retry_class(),
            RetryClass::Fallback
        );
        assert_eq!(
            ProviderError::unsupported("streaming").retry_class(),
            RetryClass::Fallback
        );
        assert_eq!(
            ProviderError::context_overflow(Some(9), Some(8)).retry_class(),
            RetryClass::Fatal
        );
        assert_eq!(ProviderError::refusal().retry_class(), RetryClass::Fatal);
        assert_eq!(
            ProviderError::content_filter().retry_class(),
            RetryClass::Fatal
        );
        assert_eq!(ProviderError::cancelled().retry_class(), RetryClass::Fatal);
        assert_eq!(
            ProviderError::other("weird").retry_class(),
            RetryClass::Fatal
        );
    }

    #[test]
    fn an_expired_credential_is_not_a_bad_key() {
        let expired = ProviderError::credential_expired();
        assert!(matches!(
            expired.kind(),
            ProviderErrorKind::CredentialExpired
        ));
        assert_ne!(
            expired.kind().as_str(),
            ProviderError::authentication().kind().as_str(),
            "the two must stay tellable apart in metrics and replay records"
        );
        // Never a plain retry: the same token would fail again immediately.
        assert_ne!(expired.retry_class(), RetryClass::Retry);
        assert_eq!(expired.retry_class(), RetryClass::Fallback);
        assert!(!expired.is_retryable());
        assert!(expired.retry_class().allows_another_candidate());
        assert_eq!(
            expired.to_core_failure().code,
            turnframe_core::error::ProviderFailureCode::CredentialExpired
        );
    }

    #[test]
    fn an_exhausted_quota_is_not_a_rate_limit() {
        let quota = ProviderError::quota_exhausted(Some("tokens_per_day"));
        assert_eq!(quota.kind().as_str(), "quota_exhausted");
        // The whole point: waiting does not refill a quota, so the caller must
        // move on rather than sleep on a Retry-After it was never given.
        assert_eq!(quota.retry_class(), RetryClass::Fallback);
        assert_ne!(quota.retry_class(), RetryClass::RetryAfter);
        assert_eq!(quota.retry_after(), None);
        assert!(quota.to_string().contains("tokens_per_day"), "{quota}");
        assert_eq!(
            quota.to_core_failure().code,
            turnframe_core::error::ProviderFailureCode::QuotaExhausted
        );

        // The scope is sanitized like every other code, so a body pasted in by
        // mistake cannot leak in readable form.
        let planted = ProviderError::quota_exhausted(Some("{\"error\": \"no credit\"}"));
        assert!(!planted.to_string().contains('"'), "{planted}");
        assert!(
            ProviderError::quota_exhausted(None)
                .to_string()
                .ends_with("quota_exhausted")
        );
    }

    #[test]
    fn error_codes_are_sanitized_and_truncated() {
        let planted = ErrorCode::new("{\"error\":{\"message\":\"invalid api key sk-live-1\"}}");
        assert!(!planted.as_str().contains('"'));
        assert!(!planted.as_str().contains(' '));
        let long = ErrorCode::new("x".repeat(500));
        assert_eq!(long.as_str().len(), MAX_ERROR_CODE_LEN);
    }

    #[test]
    fn display_carries_codes_and_keys_only() {
        let error = ProviderError::rate_limited(Some(Duration::from_secs(30)))
            .with_model(&ModelRef::new("openai", "gpt-4o"))
            .with_code("requests_per_minute");
        let text = error.to_string();
        assert_eq!(
            text,
            "provider call failed: rate_limited(retry_after=30s) [openai/gpt-4o] code=requests_per_minute"
        );
        assert_eq!(error.retry_after(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn capability_mismatch_names_the_missing_transport() {
        let mismatch = CapabilityMismatch {
            missing: vec![MissingCapability::StructuredOutput {
                required: vec![StructuredOutputCapability::NativeJsonSchema],
                declared: StructuredOutputCapability::PromptOnly,
            }],
        };
        let error = ProviderError::from(mismatch);
        assert_eq!(error.retry_class(), RetryClass::Fallback);
        let text = error.to_string();
        assert!(text.contains("native_json_schema"), "{text}");
        assert!(text.contains("prompt_only"), "{text}");
    }

    #[test]
    fn core_failure_bridge_keeps_keys_and_retryability() {
        let failure = ProviderError::timeout()
            .with_model(&ModelRef::new("anthropic", "claude"))
            .to_core_failure();
        assert_eq!(failure.provider_key.as_str(), "anthropic");
        assert_eq!(
            failure.model_key.as_ref().map(ModelKey::as_str),
            Some("claude")
        );
        assert!(failure.retryable);
        assert_eq!(
            failure.code,
            turnframe_core::error::ProviderFailureCode::Timeout
        );
        let fatal = ProviderError::refusal().to_core_failure();
        assert!(!fatal.retryable);
        assert_eq!(fatal.provider_key.as_str(), "unknown");
    }

    #[test]
    fn kinds_round_trip_through_serde() {
        let kind = ProviderErrorKind::RateLimited {
            retry_after: Some(Duration::from_millis(1500)),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"rate_limited\""));
        let back: ProviderErrorKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }
}
