//! Vendor failure → normalized failure (spec §20.7, §24, §25.2).
//!
//! Two rules shape this module.
//!
//! **Nothing from the wire reaches the error.** A status code and an error body
//! are read, classified and thrown away. What survives is a
//! [`ProviderErrorKind`], a short [`ErrorCode`] the endpoint's own `code` or
//! `type` field suggested — sanitized by [`ErrorCode::new`] and passed through
//! the configured [`Redactor`] first, so an endpoint that echoes a credential
//! into its error code cannot get it into a log line — and, for a context
//! overflow, the two integers the message named. No header, no message, no
//! body, ever.
//!
//! **Classification is by meaning, not by status.** Three mappings only the
//! body can decide, and each one is a different reaction:
//!
//! | The body says | Kind | Why not the status's kind |
//! |---|---|---|
//! | `insufficient_quota`, an empty balance | [`QuotaExhausted`](ProviderErrorKind::QuotaExhausted) | It usually arrives as 429 **with** a `Retry-After`. Waiting will not refill a quota, so sleeping on that delay burns the turn's deadline for nothing; the router must move on instead. |
//! | an expired token | [`CredentialExpired`](ProviderErrorKind::CredentialExpired) | It arrives as the same 401 a wrong key does. A caller holding a refresher can refresh and continue; told the key is *invalid*, it gives up. |
//! | `context_length_exceeded` | [`ContextOverflow`](ProviderErrorKind::ContextOverflow) | A 400 is generic; this one is fixed by shrinking the prompt, not by shopping for a bigger window mid-flight. |
//!
//! Everything else falls back to the status line.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use turnframe_provider::error::{ErrorCode, ProviderError, ProviderErrorKind};
use turnframe_provider::secret::Redactor;

/// The error envelope every OpenAI-compatible endpoint returns, plus the two
/// flatter spellings some gateways use.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiErrorEnvelope {
    #[serde(default)]
    pub(crate) error: Option<ApiError>,
    /// vLLM and a few proxies answer with a bare `{"message": …}`.
    #[serde(default)]
    pub(crate) message: Option<String>,
    /// Some gateways in front of Python services answer with `{"detail": …}`.
    #[serde(default)]
    pub(crate) detail: Option<String>,
}

/// The `error` object.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiError {
    #[serde(default)]
    pub(crate) message: Option<String>,
    #[serde(default)]
    pub(crate) r#type: Option<String>,
    #[serde(default)]
    pub(crate) code: Option<CodeValue>,
}

/// An error code, which is a string almost everywhere and an integer on a few
/// gateways.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum CodeValue {
    Text(String),
    Number(i64),
}

impl CodeValue {
    /// The code as text.
    fn as_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Number(number) => number.to_string(),
        }
    }
}

impl ApiErrorEnvelope {
    /// Decodes a body, falling back to an empty envelope for anything that is
    /// not JSON — a gateway's HTML error page, say.
    pub(crate) fn decode(body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_default()
    }

    /// The endpoint's own machine code, preferring `code` over `type`.
    fn code(&self) -> Option<String> {
        let error = self.error.as_ref()?;
        error
            .code
            .as_ref()
            .map(CodeValue::as_text)
            .or_else(|| error.r#type.clone())
            .filter(|code| !code.is_empty())
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
/// `retry_after` comes from the response headers (see [`retry_after`]).
pub(crate) fn classify(
    status: u16,
    retry_after_hint: Option<Duration>,
    envelope: &ApiErrorEnvelope,
    redactor: &dyn Redactor,
) -> ProviderError {
    let code = envelope.code().unwrap_or_default();
    let lowered = code.to_ascii_lowercase();
    let message = envelope.message().unwrap_or_default().to_ascii_lowercase();

    let error = if is_context_overflow(&lowered, &message) {
        let (needed, limit) = context_numbers(&message);
        ProviderError::context_overflow(needed, limit)
    } else if is_content_filter(&lowered, &message) {
        ProviderError::content_filter()
    } else if is_quota(&lowered, &message) {
        // Valid credentials, no entitlement left. Waiting does not help — not
        // even for the `Retry-After` a 429 carrying this body advertises —
        // but another candidate might.
        ProviderError::quota_exhausted(Some(quota_scope(&lowered, &message)))
    } else if is_expired_credential(&lowered, &message) {
        // The credential was valid and lapsed. A caller that owns a refresher
        // can refresh and call this same profile again, which is precisely
        // what `authentication` would have told it not to bother trying.
        ProviderError::credential_expired()
    } else if is_model_not_found(&lowered, &message) {
        ProviderError::model_not_found()
    } else {
        by_status(status, retry_after_hint)
    };

    attach_code(error, &code, redactor)
        .with_detail(redactor.redact(envelope.message().unwrap_or_default()))
}

/// Status-only classification, used when the body says nothing recognizable.
fn by_status(status: u16, retry_after_hint: Option<Duration>) -> ProviderError {
    match status {
        400 | 409 | 422 => ProviderError::new(ProviderErrorKind::InvalidRequest),
        401 => ProviderError::authentication(),
        // Payment Required is a billing signal even when the body is a
        // gateway's HTML page: no scope to name, but never a plain retry.
        402 => ProviderError::quota_exhausted(None),
        403 => ProviderError::authorization(),
        404 => ProviderError::model_not_found(),
        408 => ProviderError::timeout(),
        // A payload the endpoint refuses to read is a prompt that must shrink,
        // which is what `ContextOverflow` tells the runtime to do.
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

/// Recognizes a context overflow across the spellings endpoints use.
fn is_context_overflow(code: &str, message: &str) -> bool {
    matches!(
        code,
        "context_length_exceeded" | "context_window_exceeded" | "string_above_max_length"
    ) || message.contains("maximum context length")
        || message.contains("context length")
        || message.contains("reduce the length")
        || message.contains("too many tokens")
}

/// Recognizes a safety filter, including Azure's own spelling.
fn is_content_filter(code: &str, message: &str) -> bool {
    matches!(
        code,
        "content_filter" | "content_policy_violation" | "responsibleaipolicyviolation"
    ) || message.contains("content management policy")
        || message.contains("content filter")
}

/// Recognizes an exhausted quota, which is not a rate limit.
///
/// The spellings come from the endpoints this adapter serves: OpenAI's
/// `insufficient_quota` and `billing_hard_limit_reached`, Azure's
/// `quota_exceeded`, and the prose several gateways use when a prepaid balance
/// runs out.
fn is_quota(code: &str, message: &str) -> bool {
    matches!(
        code,
        "insufficient_quota"
            | "billing_hard_limit_reached"
            | "quota_exceeded"
            | "insufficient_user_quota"
            | "credit_limit_reached"
    ) || message.contains("exceeded your current quota")
        || message.contains("insufficient credits")
        || message.contains("credit balance is too low")
}

/// Names *which* allowance ran out, as a short stable code.
///
/// Two are worth telling apart because the remedy differs: a balance is topped
/// up by a human, a quota window resets on its own. Anything unrecognized is
/// the generic `quota`, never a guess.
fn quota_scope(code: &str, message: &str) -> &'static str {
    if code == "billing_hard_limit_reached"
        || code == "credit_limit_reached"
        || message.contains("credit")
    {
        "credit_balance"
    } else {
        "quota"
    }
}

/// Recognizes a credential that was valid and has lapsed.
///
/// Nearly every endpoint reports it on the same 401 it uses for a wrong key,
/// so only the body separates them: Azure's Entra tokens expire on a schedule,
/// as do the short-lived tokens gateways in front of managed identities issue.
/// A wrong key says "incorrect" or "invalid"; an expired one says "expired".
fn is_expired_credential(code: &str, message: &str) -> bool {
    matches!(
        code,
        "token_expired"
            | "expired_token"
            | "expiredtoken"
            | "expiredtokenexception"
            | "credential_expired"
            | "session_expired"
    ) || message.contains("token has expired")
        || message.contains("token is expired")
        || message.contains("expired token")
        || message.contains("credential has expired")
        || message.contains("has expired. refresh")
}

/// Recognizes a missing model or Azure deployment.
fn is_model_not_found(code: &str, message: &str) -> bool {
    matches!(
        code,
        "model_not_found" | "deploymentnotfound" | "invalid_model"
    ) || message.contains("does not exist or you do not have access")
        || message.contains("model not found")
}

/// Reads the limit and the requested size out of a context-overflow message.
///
/// Integers are not secrets and they are what makes the failure actionable, so
/// they are the one thing lifted out of a message. Anything unrecognized
/// yields `None`, never a guess.
///
/// Returns `(needed, limit)`.
fn context_numbers(message: &str) -> (Option<u64>, Option<u64>) {
    let limit = number_after(message, "maximum context length is");
    let needed = number_after(message, "resulted in")
        .or_else(|| number_after(message, "you requested"))
        .or_else(|| number_after(message, "requested"));
    (needed, limit)
}

/// The first integer that follows `marker`.
fn number_after(message: &str, marker: &str) -> Option<u64> {
    let rest = message.split_once(marker)?.1;
    let digits: String = rest
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Reads a retry hint out of the response headers.
///
/// RFC 9110 gives `Retry-After` two forms and endpoints use both: a
/// delay in seconds, and an HTTP date. `retry-after-ms` — an OpenAI extension
/// several gateways copy — wins when present because it is the precise one;
/// then plain seconds, whole or fractional; then the date form.
///
/// # Reading a date
///
/// A date is an absolute instant, so it only becomes a delay relative to
/// something. That something is **the response itself**: its `Date` header when
/// it has one, and the local clock otherwise. Taking the response's own clock
/// as the origin means a skewed peer that says "wait until 12:00:30" and
/// stamps the answer "12:00:00" yields thirty seconds on our side too, instead
/// of a delay warped by the difference between the two clocks.
///
/// A date already in the past clamps to zero rather than becoming a negative
/// wait: the endpoint is saying the window has already reopened.
pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if let Some(millis) = text("retry-after-ms").and_then(|raw| raw.trim().parse::<u64>().ok()) {
        return Some(Duration::from_millis(millis));
    }
    let raw = text("retry-after")?.trim();
    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    if let Some(seconds) = raw
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
    {
        return Some(Duration::from_secs_f64(seconds));
    }
    let until = http_date(raw)?;
    let origin = text("date").and_then(http_date).unwrap_or_else(Utc::now);
    // `to_std` fails exactly when the span is negative, which is the past-date
    // case: the window has already reopened, so there is nothing to wait for.
    Some(
        until
            .signed_duration_since(origin)
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

/// Parses the one date format HTTP headers carry.
///
/// `Retry-After` and `Date` are IMF-fixdate (`Wed, 21 Oct 2026 07:28:00 GMT`),
/// which is RFC 2822 with `GMT` as the zone. Anything else yields `None`, and
/// the caller falls back rather than inventing a delay.
fn http_date(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(raw.trim())
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
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
    use turnframe_provider::error::{ProviderErrorKind, RetryClass};
    use turnframe_provider::secret::{ApiKey, DefaultRedactor};

    fn redactor() -> DefaultRedactor {
        DefaultRedactor::new().with_secret(&ApiKey::new("sk-planted-0123456789abcdef"))
    }

    fn map(status: u16, body: &str) -> ProviderError {
        classify(status, None, &ApiErrorEnvelope::decode(body), &redactor())
    }

    #[test]
    fn statuses_map_to_their_families() {
        assert!(matches!(
            map(401, "{}").kind(),
            ProviderErrorKind::Authentication
        ));
        assert!(matches!(
            map(403, "{}").kind(),
            ProviderErrorKind::Authorization
        ));
        assert!(matches!(
            map(404, "{}").kind(),
            ProviderErrorKind::ModelNotFound
        ));
        assert!(matches!(map(408, "{}").kind(), ProviderErrorKind::Timeout));
        assert!(matches!(
            map(422, "{}").kind(),
            ProviderErrorKind::InvalidRequest
        ));
        assert!(matches!(
            map(503, "{}").kind(),
            ProviderErrorKind::Server { status: Some(503) }
        ));
        assert_eq!(map(418, "{}").retry_class(), RetryClass::Fatal);
    }

    #[test]
    fn a_rate_limit_keeps_the_delay_the_endpoint_asked_for() {
        let error = classify(
            429,
            Some(Duration::from_secs(3)),
            &ApiErrorEnvelope::decode("{\"error\":{\"code\":\"rate_limit_exceeded\"}}"),
            &redactor(),
        );
        assert_eq!(error.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(error.retry_class(), RetryClass::RetryAfter);
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("rate_limit_exceeded".to_owned())
        );
    }

    #[test]
    fn an_exhausted_quota_is_not_a_rate_limit_even_on_a_429_with_a_delay() {
        // The trap this test exists for: the status and the header are those of
        // a real rate limit, and only the body says the wait would be wasted.
        let error = classify(
            429,
            Some(Duration::from_secs(3)),
            &ApiErrorEnvelope::decode(
                "{\"error\":{\"code\":\"insufficient_quota\",\"message\":\"You exceeded your \
                 current quota\"}}",
            ),
            &redactor(),
        );
        assert_eq!(
            *error.kind(),
            ProviderErrorKind::QuotaExhausted {
                scope: Some(ErrorCode::new("quota"))
            }
        );
        assert_eq!(error.retry_class(), RetryClass::Fallback);
        assert_eq!(error.retry_after(), None, "a quota has no useful delay");
    }

    #[test]
    fn an_empty_balance_names_the_balance_rather_than_a_window() {
        let error = map(
            402,
            "{\"error\":{\"code\":\"billing_hard_limit_reached\",\"message\":\"Your credit \
             balance is too low to access this model.\"}}",
        );
        assert_eq!(
            *error.kind(),
            ProviderErrorKind::QuotaExhausted {
                scope: Some(ErrorCode::new("credit_balance"))
            }
        );
        assert!(error.to_string().contains("credit_balance"), "{error}");

        // A gateway that answers 402 with an HTML page still says "billing".
        let bare = map(402, "<html>Payment Required</html>");
        assert_eq!(
            *bare.kind(),
            ProviderErrorKind::QuotaExhausted { scope: None }
        );
        assert_eq!(bare.retry_class(), RetryClass::Fallback);
    }

    #[test]
    fn an_expired_credential_is_not_a_rejected_one() {
        // Both are 401s; only the body tells them apart, and the two call for
        // opposite reactions from a caller that owns a refresher.
        let expired = map(
            401,
            "{\"error\":{\"code\":\"token_expired\",\"message\":\"The access token has expired. \
             Refresh the token and try again.\",\"type\":\"invalid_request_error\"}}",
        );
        assert_eq!(*expired.kind(), ProviderErrorKind::CredentialExpired);
        assert_eq!(expired.retry_class(), RetryClass::Fallback);

        let wrong = map(
            401,
            "{\"error\":{\"code\":\"invalid_api_key\",\"message\":\"Incorrect API key \
             provided.\"}}",
        );
        assert_eq!(*wrong.kind(), ProviderErrorKind::Authentication);

        // Recognized by the prose alone, which is how Azure reports a lapsed
        // Entra token: the code is the bare status.
        let by_prose = map(
            401,
            "{\"error\":{\"code\":\"401\",\"message\":\"Access denied: the bearer token is \
             expired.\"}}",
        );
        assert_eq!(*by_prose.kind(), ProviderErrorKind::CredentialExpired);
    }

    #[test]
    fn a_context_overflow_keeps_its_two_numbers_and_nothing_else() {
        let body = "{\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"This model's \
                    maximum context length is 8192 tokens. However, your messages resulted in \
                    10245 tokens. Please reduce the length.\",\"type\":\"invalid_request_error\"}}";
        let error = map(400, body);
        assert_eq!(
            *error.kind(),
            ProviderErrorKind::ContextOverflow {
                needed_tokens: Some(10245),
                limit_tokens: Some(8192)
            }
        );
        assert_eq!(error.retry_class(), RetryClass::Fatal);
        let rendered = error.to_string();
        assert!(!rendered.contains("However"), "{rendered}");
        assert!(rendered.contains("needed=10245"), "{rendered}");
    }

    #[test]
    fn a_context_overflow_without_numbers_says_so() {
        let error = map(413, "{}");
        assert_eq!(
            *error.kind(),
            ProviderErrorKind::ContextOverflow {
                needed_tokens: None,
                limit_tokens: None
            }
        );
    }

    #[test]
    fn a_safety_filter_is_recognized_in_both_dialects() {
        assert!(matches!(
            map(400, "{\"error\":{\"code\":\"content_policy_violation\"}}").kind(),
            ProviderErrorKind::ContentFilter
        ));
        assert!(matches!(
            map(
                400,
                "{\"error\":{\"message\":\"The response was filtered due to the prompt \
                 triggering our content management policy\"}}"
            )
            .kind(),
            ProviderErrorKind::ContentFilter
        ));
    }

    #[test]
    fn a_numeric_code_and_a_flat_message_still_decode() {
        let error = map(500, "{\"error\":{\"code\":503,\"message\":\"upstream\"}}");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("503".to_owned())
        );
        let bare = map(400, "{\"message\":\"model not found\"}");
        assert!(matches!(bare.kind(), ProviderErrorKind::ModelNotFound));
        let detail = map(422, "{\"detail\":\"nothing familiar\"}");
        assert!(matches!(detail.kind(), ProviderErrorKind::InvalidRequest));
    }

    #[test]
    fn a_body_that_is_not_json_classifies_by_status_alone() {
        let error = map(502, "<html><body>Bad gateway</body></html>");
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::Server { status: Some(502) }
        ));
        assert!(error.code().is_none());
    }

    #[test]
    fn a_credential_echoed_into_the_error_code_does_not_survive() {
        let body = "{\"error\":{\"code\":\"sk-planted-0123456789abcdef\"}}";
        let error = map(401, body);
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("sk-planted"), "{rendered}");
        assert!(rendered.contains("REDACTED"), "{rendered}");
    }

    #[test]
    fn retry_after_reads_seconds_whole_and_fractional() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&headers), None);

        headers.insert("retry-after", "3".parse().expect("header"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(3)));

        headers.insert("retry-after", "1.5".parse().expect("header"));
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(1500)));

        // The millisecond extension is the precise one and wins.
        headers.insert("retry-after-ms", "250".parse().expect("header"));
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(250)));
    }

    #[test]
    fn a_date_becomes_the_delay_from_the_response_that_carried_it() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:30 GMT".parse().expect("header"),
        );
        headers.insert(
            "date",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().expect("header"),
        );
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));

        // A date already past is zero, never a negative wait.
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:27:00 GMT".parse().expect("header"),
        );
        assert_eq!(retry_after(&headers), Some(Duration::ZERO));
    }

    #[test]
    fn a_date_without_a_response_date_is_measured_against_our_own_clock() {
        let mut headers = reqwest::header::HeaderMap::new();
        let soon = Utc::now() + chrono::Duration::seconds(120);
        headers.insert("retry-after", soon.to_rfc2822().parse().expect("header"));
        let delay = retry_after(&headers).expect("a date is a delay");
        // A whole-second header truncates, and the assertion must survive a
        // slow machine between the two clock reads.
        assert!(
            delay <= Duration::from_secs(120) && delay >= Duration::from_secs(110),
            "{delay:?}"
        );
    }

    #[test]
    fn a_retry_after_that_is_neither_a_number_nor_a_date_is_no_hint_at_all() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "soon".parse().expect("header"));
        assert_eq!(retry_after(&headers), None, "a guess is worse than nothing");
    }
}
