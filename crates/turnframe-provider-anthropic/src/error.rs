//! Vendor failure → normalized failure (spec §20.7, §24, §25.2).
//!
//! Two rules shape this module.
//!
//! **Nothing from the wire reaches the error.** A status code and an error body
//! are read, classified and thrown away. What survives is a
//! [`ProviderErrorKind`](turnframe_provider::error::ProviderErrorKind), a short
//! [`ErrorCode`] taken from the envelope's own `error.type` — sanitized by
//! [`ErrorCode::new`] and passed through the configured [`Redactor`] first, so
//! an endpoint that echoes a credential into its error type cannot get it into
//! a log line — and, for a context overflow, the two integers the message
//! named. No header, no message, no body, ever.
//!
//! **Classification is by meaning, not by status.** Anthropic's `error.type` is
//! precise enough to lead, and the status only fills in what the body does not
//! say:
//!
//! | `error.type` | Status | Normalized | Retry class |
//! |---|---|---|---|
//! | `invalid_request_error` | 400 | `InvalidRequest` | fatal |
//! | `invalid_request_error` naming a prompt that is too long | 400 | `ContextOverflow` | fatal — shrink the prompt |
//! | `invalid_request_error` naming an exhausted credit balance | 400 | `Authorization` | fall back |
//! | `authentication_error` | 401 | `Authentication` | fall back |
//! | `permission_error` | 403 | `Authorization` | fall back |
//! | `not_found_error` | 404 | `ModelNotFound` | fall back |
//! | `request_too_large` | 413 | `ContextOverflow` | fatal — shrink the prompt |
//! | `rate_limit_error` | 429 | `RateLimited { retry_after }` | retry after the delay |
//! | `timeout_error` | 408 | `Timeout` | retry |
//! | `api_error` | 500 | `Server` | retry |
//! | `overloaded_error` | 529 | `Server` | retry |
//!
//! Two rows are worth stating out loud. **`overloaded_error` is a server
//! error**, not a rate limit: Anthropic answers 529 when the whole service is
//! saturated rather than when this key has spent its quota, so the caller
//! backs off on its own schedule instead of waiting for a delay nobody sent.
//! And **`request_too_large` is a context overflow**, because the remedy is the
//! same one the runtime already knows how to apply — send less — even though
//! the limit that was hit is a byte count rather than a token count.
//!
//! # Two failures that look like others and are not
//!
//! | Signal | Normalized | Why it is not the obvious thing |
//! |---|---|---|
//! | 401 whose message says the key is expired, revoked or disabled | [`CredentialExpired`](ProviderErrorKind::CredentialExpired) | A key that *was* valid is a rotation problem, not a typo in configuration. A caller holding a refresher can refresh and call the same profile again; a plain `Authentication` would tell it not to bother. |
//! | An exhausted balance or spend limit, on a **400 or a 429** | [`QuotaExhausted`](ProviderErrorKind::QuotaExhausted) | Waiting does not refill a balance. Reading it as a rate limit would park the turn behind a delay that changes nothing, so it falls back to another candidate instead. |
//!
//! Both are recognized **before** the status is consulted, which is what stops
//! a billing 429 from being read as a rate limit — the Messages API returns an
//! empty balance with either status, so the status cannot be what decides. The
//! quota kind carries the scope the message named, as
//! [`QUOTA_SCOPE_CREDIT_BALANCE`], [`QUOTA_SCOPE_SPEND_LIMIT`] or
//! [`QUOTA_SCOPE_QUOTA`].

use std::time::Duration;

use serde::Deserialize;
use turnframe_provider::error::{ErrorCode, ProviderError, ProviderErrorKind};
use turnframe_provider::secret::Redactor;

/// The status a stream-borne error is classified as when its type says nothing.
///
/// A mid-stream failure arrives inside a `200`, so there is no status to read.
/// Treating an unrecognized one as a server error makes it retryable, which is
/// the safe reading of "the connection said something we do not model".
const STREAM_FALLBACK_STATUS: u16 = 500;

/// Quota scope reported when the account's prepaid balance is spent.
///
/// ```
/// use turnframe_provider_anthropic::QUOTA_SCOPE_CREDIT_BALANCE;
///
/// assert_eq!(QUOTA_SCOPE_CREDIT_BALANCE, "credit_balance");
/// ```
pub const QUOTA_SCOPE_CREDIT_BALANCE: &str = "credit_balance";

/// Quota scope reported when a configured spend limit was reached.
pub const QUOTA_SCOPE_SPEND_LIMIT: &str = "spend_limit";

/// Quota scope reported when the endpoint says only that a quota is exhausted.
pub const QUOTA_SCOPE_QUOTA: &str = "quota";

/// The error envelope the Messages API returns, plus the two flatter spellings
/// proxies in front of it use.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiErrorEnvelope {
    /// Always `"error"` on a real Anthropic body. Read for nothing; present so
    /// a body that only carries it still decodes.
    #[serde(default, rename = "type")]
    pub(crate) envelope_type: Option<String>,
    #[serde(default)]
    pub(crate) error: Option<ApiError>,
    /// Some proxies answer with a bare `{"message": …}`.
    #[serde(default)]
    pub(crate) message: Option<String>,
    /// Gateways in front of Python services answer with `{"detail": …}`.
    #[serde(default)]
    pub(crate) detail: Option<String>,
}

/// The `error` object.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiError {
    #[serde(default, rename = "type")]
    pub(crate) error_type: Option<String>,
    #[serde(default)]
    pub(crate) message: Option<String>,
}

impl ApiErrorEnvelope {
    /// Decodes a body, falling back to an empty envelope for anything that is
    /// not JSON — a gateway's HTML error page, say.
    pub(crate) fn decode(body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_default()
    }

    /// An envelope carrying just this error, for the mid-stream `error` event.
    pub(crate) fn of(error: ApiError) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }

    /// The endpoint's own machine code, which Anthropic spells `error.type`.
    fn code(&self) -> Option<&str> {
        self.error
            .as_ref()
            .and_then(|error| error.error_type.as_deref())
            .filter(|code| !code.is_empty())
            .or_else(|| self.envelope_type.as_deref().filter(|t| *t != "error"))
    }

    /// The human message, from whichever field carries it. Used only to
    /// classify and to read integers out of; never stored.
    fn message(&self) -> Option<&str> {
        self.error
            .as_ref()
            .and_then(|error| error.message.as_deref())
            .or(self.message.as_deref())
            .or(self.detail.as_deref())
    }
}

/// Maps one failed HTTP exchange onto a normalized failure.
///
/// `retry_after_hint` comes from the response headers (see [`retry_after`]).
pub(crate) fn classify(
    status: u16,
    retry_after_hint: Option<Duration>,
    envelope: &ApiErrorEnvelope,
    redactor: &dyn Redactor,
) -> ProviderError {
    let raw_code = envelope.code().unwrap_or_default().to_owned();
    let raw_message = envelope.message().unwrap_or_default().to_owned();
    let code = raw_code.to_ascii_lowercase();
    let message = envelope.message().unwrap_or_default().to_ascii_lowercase();

    // These two are recognized before the status is consulted, so a billing 429
    // never reaches the rate-limit branch and an expired key never reaches the
    // plain authentication one.
    if let Some(scope) = quota_scope(&code, &message) {
        return attach_detail(
            attach_code(
                ProviderError::quota_exhausted(Some(scope)),
                &raw_code,
                redactor,
            ),
            &raw_message,
            redactor,
        );
    }
    if is_expired_credential(&code, status, &message) {
        return attach_detail(
            attach_code(ProviderError::credential_expired(), &raw_code, redactor),
            &raw_message,
            redactor,
        );
    }

    let error = if is_context_overflow(&code, &message) {
        let (needed, limit) = context_numbers(&message);
        ProviderError::context_overflow(needed, limit)
    } else if is_content_filter(&code, &message) {
        ProviderError::content_filter()
    } else if let Some(by_type) = by_error_type(&code, status, retry_after_hint) {
        by_type
    } else {
        by_status(status, retry_after_hint)
    };

    attach_detail(
        attach_code(error, &raw_code, redactor),
        &raw_message,
        redactor,
    )
}

/// Maps a mid-stream `error` event, which arrives inside a `200` and so has no
/// status of its own.
///
/// The synthetic status comes from the error type, so the classification is the
/// same one the equivalent HTTP failure would have produced.
pub(crate) fn classify_stream(
    envelope: &ApiErrorEnvelope,
    redactor: &dyn Redactor,
) -> ProviderError {
    let status = envelope.code().map_or(STREAM_FALLBACK_STATUS, |code| {
        status_for_error_type(&code.to_ascii_lowercase())
    });
    classify(status, None, envelope, redactor)
}

/// The status Anthropic pairs each error type with.
fn status_for_error_type(code: &str) -> u16 {
    match code {
        "invalid_request_error" => 400,
        "authentication_error" => 401,
        "permission_error" => 403,
        "not_found_error" => 404,
        "timeout_error" => 408,
        "request_too_large" => 413,
        "rate_limit_error" => 429,
        "overloaded_error" => 529,
        _ => STREAM_FALLBACK_STATUS,
    }
}

/// Classification driven by the envelope's own type.
fn by_error_type(
    code: &str,
    status: u16,
    retry_after_hint: Option<Duration>,
) -> Option<ProviderError> {
    Some(match code {
        "invalid_request_error" => ProviderError::new(ProviderErrorKind::InvalidRequest),
        "authentication_error" => ProviderError::authentication(),
        "permission_error" => ProviderError::authorization(),
        "not_found_error" => ProviderError::model_not_found(),
        // A body the endpoint refuses to read is a prompt that must shrink,
        // which is what `ContextOverflow` tells the runtime to do.
        "request_too_large" => ProviderError::context_overflow(None, None),
        "rate_limit_error" => ProviderError::rate_limited(retry_after_hint),
        "timeout_error" => ProviderError::timeout(),
        // The whole service is saturated, not this key's quota: back off on our
        // own schedule rather than waiting for a delay nobody sent.
        "overloaded_error" => ProviderError::server(Some(server_status(status, 529))),
        "api_error" => ProviderError::server(Some(server_status(status, 500))),
        _ => return None,
    })
}

/// A server error records the status it arrived with, or the one its type
/// implies when it arrived inside a stream.
const fn server_status(status: u16, implied: u16) -> u16 {
    if status >= 400 { status } else { implied }
}

/// Status-only classification, used when the body says nothing recognizable.
fn by_status(status: u16, retry_after_hint: Option<Duration>) -> ProviderError {
    match status {
        400 | 409 | 422 => ProviderError::new(ProviderErrorKind::InvalidRequest),
        401 => ProviderError::authentication(),
        403 => ProviderError::authorization(),
        404 => ProviderError::model_not_found(),
        408 => ProviderError::timeout(),
        413 => ProviderError::context_overflow(None, None),
        429 => ProviderError::rate_limited(retry_after_hint),
        500..=599 => ProviderError::server(Some(status)),
        other => ProviderError::other(format!("http_{other}")),
    }
}

/// Attaches the endpoint's machine code, redacted and sanitized.
fn attach_code(error: ProviderError, code: &str, redactor: &dyn Redactor) -> ProviderError {
    if code.is_empty() {
        return error;
    }
    let masked = redactor.redact(code);
    error.with_code(ErrorCode::new(masked).as_str())
}

/// Attaches the endpoint's own sentence, through the same redactor the code
/// goes through.
///
/// A code says which family a failure belongs to; only the sentence says what
/// was wrong with the request, and for a malformed one that is the caller's
/// own bug to fix rather than a provider to wait for.
fn attach_detail(error: ProviderError, message: &str, redactor: &dyn Redactor) -> ProviderError {
    if message.is_empty() {
        return error;
    }
    error.with_detail(redactor.redact(message))
}

/// Recognizes the two shapes Anthropic reports a context overflow in.
fn is_context_overflow(code: &str, message: &str) -> bool {
    code == "request_too_large"
        || message.contains("prompt is too long")
        || message.contains("exceed context limit")
        || message.contains("maximum context length")
        || message.contains("too many total text bytes")
}

/// Recognizes a safety intervention reported as a failure.
///
/// The ordinary Anthropic shape is a **successful** response whose
/// `stop_reason` is `refusal`; this covers the case where the classifier
/// rejects the request outright instead.
fn is_content_filter(code: &str, message: &str) -> bool {
    code == "content_filter"
        || message.contains("content filtering policy")
        || message.contains("blocked by content filter")
        || message.contains("usage policies")
}

/// Names the exhausted quota, spend limit or credit balance, when there is one.
///
/// Matched **before** the status, because the API returns this as a `429` as
/// readily as a `400` and a caller told to wait would wait forever. Ordinary
/// rate limiting is worded around requests, tokens and per-minute limits and
/// matches none of these.
fn quota_scope(code: &str, message: &str) -> Option<&'static str> {
    if message.contains("credit balance is too low")
        || message.contains("insufficient credit")
        || message.contains("purchase additional credits")
    {
        return Some(QUOTA_SCOPE_CREDIT_BALANCE);
    }
    if message.contains("spend limit") {
        return Some(QUOTA_SCOPE_SPEND_LIMIT);
    }
    if code == "billing_error"
        || message.contains("exceeded your current quota")
        || message.contains("billing details")
    {
        return Some(QUOTA_SCOPE_QUOTA);
    }
    None
}

/// Recognizes a credential the endpoint says used to be valid.
///
/// Only an authentication signal qualifies: the word "expired" in a validation
/// message about some other field is not a credential problem. A malformed or
/// simply unknown key stays a plain
/// [`Authentication`](ProviderErrorKind::Authentication) failure, because the
/// remedy is different — re-read the configuration rather than refresh or
/// reissue the credential.
fn is_expired_credential(code: &str, status: u16, message: &str) -> bool {
    let authentication = code == "authentication_error" || (code.is_empty() && status == 401);
    authentication
        && (message.contains("expired")
            || message.contains("revoked")
            || message.contains("has been disabled")
            || message.contains("no longer active"))
}

/// Reads the requested size and the limit out of a context-overflow message.
///
/// Integers are not secrets and they are what makes the failure actionable, so
/// they are the one thing lifted out of a message. Two shapes are recognized
/// and nothing else is guessed at:
///
/// * `prompt is too long: 215048 tokens > 199999 maximum` → `(215048, 199999)`;
/// * ``input length and `max_tokens` exceed context limit: 100000 + 40000 >
///   128000`` → `(140000, 128000)`, because both halves are what the request
///   asked for.
///
/// Returns `(needed, limit)`.
fn context_numbers(message: &str) -> (Option<u64>, Option<u64>) {
    if let Some((_, rest)) = message.split_once("prompt is too long:") {
        let needed = first_number(rest);
        let limit = rest
            .split_once('>')
            .and_then(|(_, after)| first_number(after));
        return (needed, limit);
    }
    if message.contains("exceed context limit")
        && let Some((before, after)) = message.split_once('>')
    {
        // Only the run after the last colon holds the request's own numbers.
        let asked = before.rsplit(':').next().unwrap_or_default();
        return (sum_numbers(asked), first_number(after));
    }
    (None, None)
}

/// The first integer in `text`.
fn first_number(text: &str) -> Option<u64> {
    let digits: String = text
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The sum of every integer in `text`, or `None` when there is none.
fn sum_numbers(text: &str) -> Option<u64> {
    let mut total: u64 = 0;
    let mut seen = false;
    let mut current = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            current.push(ch);
            continue;
        }
        if let Ok(value) = current.parse::<u64>() {
            total = total.saturating_add(value);
            seen = true;
        }
        current.clear();
    }
    seen.then_some(total)
}

/// Reads a retry hint out of the response headers.
///
/// `retry-after` is read as (possibly fractional) seconds. Anthropic also sends
/// `anthropic-ratelimit-*-reset` as an RFC 3339 instant; it is ignored rather
/// than turned into a delay, because a wrong delay is worse than none.
pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if let Some(millis) = text("retry-after-ms").and_then(|raw| raw.trim().parse::<u64>().ok()) {
        return Some(Duration::from_millis(millis));
    }
    let raw = text("retry-after")?.trim();
    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    raw.parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(Duration::from_secs_f64)
}

/// Maps a transport failure onto a normalized failure.
///
/// The `reqwest` error is inspected, never rendered: its `Display` carries the
/// request URL and, on a redirect, a good deal more.
pub(crate) fn transport(error: &reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        return ProviderError::timeout();
    }
    if error.is_connect() {
        return ProviderError::transport("connect_failed");
    }
    if error.is_decode() {
        return ProviderError::malformed("body_not_readable");
    }
    if error.is_body() {
        return ProviderError::transport("body_failed");
    }
    ProviderError::transport("request_failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_provider::error::RetryClass;
    use turnframe_provider::secret::{ApiKey, DefaultRedactor};

    const PLANTED: &str = "sk-ant-planted-0123456789abcdef";

    fn redactor() -> DefaultRedactor {
        DefaultRedactor::new().with_secret(&ApiKey::new(PLANTED))
    }

    fn body(error_type: &str, message: &str) -> String {
        serde_json::json!({
            "type": "error",
            "error": {"type": error_type, "message": message},
            "request_id": "req_011CS"
        })
        .to_string()
    }

    fn map(status: u16, body: &str) -> ProviderError {
        classify(status, None, &ApiErrorEnvelope::decode(body), &redactor())
    }

    #[test]
    fn the_error_type_decides_and_the_status_only_fills_in() {
        assert!(matches!(
            map(401, &body("authentication_error", "invalid x-api-key")).kind(),
            ProviderErrorKind::Authentication
        ));
        assert!(matches!(
            map(403, &body("permission_error", "not entitled")).kind(),
            ProviderErrorKind::Authorization
        ));
        assert!(matches!(
            map(404, &body("not_found_error", "model: nope")).kind(),
            ProviderErrorKind::ModelNotFound
        ));
        assert!(matches!(
            map(408, &body("timeout_error", "took too long")).kind(),
            ProviderErrorKind::Timeout
        ));
        assert!(matches!(
            map(
                400,
                &body("invalid_request_error", "tools.0.name: too long")
            )
            .kind(),
            ProviderErrorKind::InvalidRequest
        ));
        assert!(matches!(
            map(413, &body("request_too_large", "request body too large")).kind(),
            ProviderErrorKind::ContextOverflow { .. }
        ));
        // A status with no body at all still classifies.
        assert!(matches!(
            map(503, "<html>bad gateway</html>").kind(),
            ProviderErrorKind::Server { status: Some(503) }
        ));
        assert!(matches!(map(418, "{}").kind(), ProviderErrorKind::Other));
    }

    #[test]
    fn overloaded_is_a_server_error_and_not_a_rate_limit() {
        let error = map(529, &body("overloaded_error", "Overloaded"));
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::Server { status: Some(529) }
        ));
        assert_eq!(error.retry_class(), RetryClass::Retry);
        assert!(error.retry_after().is_none());
        assert_eq!(
            error.code().map(|c| c.as_str().to_owned()),
            Some("overloaded_error".to_owned())
        );
    }

    #[test]
    fn a_real_rate_limit_keeps_its_delay() {
        let limited = classify(
            429,
            Some(Duration::from_secs(3)),
            &ApiErrorEnvelope::decode(&body(
                "rate_limit_error",
                "Number of request tokens has exceeded your per-minute rate limit",
            )),
            &redactor(),
        );
        assert!(matches!(
            limited.kind(),
            ProviderErrorKind::RateLimited { .. }
        ));
        assert_eq!(limited.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(limited.retry_class(), RetryClass::RetryAfter);
    }

    #[test]
    fn a_spent_balance_is_a_quota_failure_on_every_status_it_arrives_with() {
        // The API returns it as a 400 …
        let on_400 = map(
            400,
            &body(
                "invalid_request_error",
                "Your credit balance is too low to access the Anthropic API. \
                 Please go to Plans & Billing to upgrade or purchase credits.",
            ),
        );
        // … and as a 429, where waiting out a delay would change nothing.
        let on_429 = classify(
            429,
            Some(Duration::from_secs(3)),
            &ApiErrorEnvelope::decode(&body(
                "rate_limit_error",
                "Your organization has exceeded its monthly spend limit",
            )),
            &redactor(),
        );
        for error in [&on_400, &on_429] {
            assert!(
                matches!(error.kind(), ProviderErrorKind::QuotaExhausted { .. }),
                "{error}"
            );
            assert_eq!(error.retry_class(), RetryClass::Fallback, "{error}");
        }
        assert_eq!(
            on_400.kind(),
            &ProviderErrorKind::QuotaExhausted {
                scope: Some(ErrorCode::new(QUOTA_SCOPE_CREDIT_BALANCE))
            }
        );
        assert_eq!(
            on_429.kind(),
            &ProviderErrorKind::QuotaExhausted {
                scope: Some(ErrorCode::new(QUOTA_SCOPE_SPEND_LIMIT))
            }
        );
        // The delay the 429 advertised is not carried: there is nothing to wait
        // for, and a caller that saw one would wait for it.
        assert!(on_429.retry_after().is_none());
        // The vendor's own type still travels as the sanitized code.
        assert_eq!(
            on_429.code().map(|code| code.as_str().to_owned()),
            Some("rate_limit_error".to_owned())
        );
    }

    #[test]
    fn an_expired_key_is_told_apart_from_a_wrong_one() {
        for message in [
            "This API key has expired",
            "This organization's API key has been revoked",
            "This API key has been disabled",
        ] {
            let error = map(401, &body("authentication_error", message));
            assert!(
                matches!(error.kind(), ProviderErrorKind::CredentialExpired),
                "{message}: {error}"
            );
            // Never `Retry`: the same expired credential is a busy loop.
            assert_eq!(error.retry_class(), RetryClass::Fallback);
            assert_eq!(
                error.code().map(|code| code.as_str().to_owned()),
                Some("authentication_error".to_owned())
            );
        }

        // A key that was never valid stays a plain authentication failure: the
        // remedy is to re-read the configuration, not to reissue the key.
        let wrong = map(401, &body("authentication_error", "invalid x-api-key"));
        assert!(matches!(wrong.kind(), ProviderErrorKind::Authentication));
        assert_eq!(
            wrong.code().map(|code| code.as_str().to_owned()),
            Some("authentication_error".to_owned())
        );

        // And "expired" in a message about something else is not a credential.
        let unrelated = map(
            400,
            &body("invalid_request_error", "the attached file has expired"),
        );
        assert!(matches!(
            unrelated.kind(),
            ProviderErrorKind::InvalidRequest
        ));
    }

    #[test]
    fn both_context_overflow_messages_yield_their_numbers() {
        let long = map(
            400,
            &body(
                "invalid_request_error",
                "prompt is too long: 215048 tokens > 199999 maximum",
            ),
        );
        assert_eq!(
            long.kind(),
            &ProviderErrorKind::ContextOverflow {
                needed_tokens: Some(215_048),
                limit_tokens: Some(199_999)
            }
        );
        assert_eq!(long.retry_class(), RetryClass::Fatal);

        let combined = map(
            400,
            &body(
                "invalid_request_error",
                "input length and `max_tokens` exceed context limit: 100000 + 40000 > 128000, \
                 decrease input length or `max_tokens` and try again",
            ),
        );
        assert_eq!(
            combined.kind(),
            &ProviderErrorKind::ContextOverflow {
                needed_tokens: Some(140_000),
                limit_tokens: Some(128_000)
            }
        );

        // An unrecognized shape yields no numbers rather than a guess.
        let vague = map(400, &body("invalid_request_error", "prompt is too long"));
        assert_eq!(
            vague.kind(),
            &ProviderErrorKind::ContextOverflow {
                needed_tokens: None,
                limit_tokens: None
            }
        );
    }

    #[test]
    fn a_safety_intervention_is_not_a_server_error() {
        let filtered = map(
            400,
            &body(
                "invalid_request_error",
                "Output blocked by content filtering policy",
            ),
        );
        assert!(matches!(filtered.kind(), ProviderErrorKind::ContentFilter));
        assert_eq!(filtered.retry_class(), RetryClass::Fatal);
    }

    #[test]
    fn a_stream_error_is_classified_by_its_type_alone() {
        let overloaded = classify_stream(
            &ApiErrorEnvelope::of(ApiError {
                error_type: Some("overloaded_error".to_owned()),
                message: Some("Overloaded".to_owned()),
            }),
            &redactor(),
        );
        assert!(matches!(
            overloaded.kind(),
            ProviderErrorKind::Server { status: Some(529) }
        ));

        let unknown = classify_stream(
            &ApiErrorEnvelope::of(ApiError {
                error_type: Some("something_new".to_owned()),
                message: None,
            }),
            &redactor(),
        );
        assert!(matches!(
            unknown.kind(),
            ProviderErrorKind::Server { status: Some(500) }
        ));
        assert_eq!(unknown.retry_class(), RetryClass::Retry);
    }

    #[test]
    fn nothing_from_the_body_survives_into_the_error() {
        let leaky = serde_json::json!({
            "type": "error",
            "error": {
                "type": format!("invalid_api_key {PLANTED}"),
                "message": format!("the key {PLANTED} was rejected")
            }
        })
        .to_string();
        let error = map(401, &leaky);
        let renderings = [error.to_string(), format!("{error:?}")];
        for rendering in &renderings {
            assert!(!rendering.contains("sk-ant-planted"), "{rendering}");
            assert!(!rendering.contains("was rejected"), "{rendering}");
        }
        assert!(
            error
                .code()
                .is_some_and(|code| code.as_str().contains("REDACTED"))
        );
    }

    #[test]
    fn retry_after_reads_seconds_and_milliseconds_and_refuses_a_date() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert!(retry_after(&headers).is_none());
        headers.insert("retry-after", "3".parse().expect("a header value"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(3)));
        headers.insert("retry-after", "1.5".parse().expect("a header value"));
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(1500)));
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT"
                .parse()
                .expect("a header value"),
        );
        assert!(retry_after(&headers).is_none());
        headers.insert("retry-after-ms", "250".parse().expect("a header value"));
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(250)));
    }

    #[test]
    fn the_number_helpers_are_total() {
        assert_eq!(first_number("no digits here"), None);
        assert_eq!(first_number("tokens 42 and 7"), Some(42));
        assert_eq!(sum_numbers("nothing"), None);
        assert_eq!(sum_numbers(" 1 + 2 + 3 "), Some(6));
    }
}
