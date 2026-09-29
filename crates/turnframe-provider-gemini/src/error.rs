//! Google failure → normalized failure (spec §20.7, §24, §25.2).
//!
//! Two rules shape this module.
//!
//! **Nothing from the wire reaches the error.** A status code and an error body
//! are read, classified and thrown away. What survives is a
//! [`ProviderErrorKind`](turnframe_provider::error::ProviderErrorKind), a short
//! [`ErrorCode`] built from Google's own canonical status name — sanitized by
//! [`ErrorCode::new`] and passed through the configured [`Redactor`] first, so
//! an endpoint that echoes a credential into a code cannot get it into a log
//! line — and, for a context overflow, the two integers the message named. No
//! header, no message, no body, ever.
//!
//! **Classification is by canonical status, then by HTTP code.** Google's APIs
//! carry a `status` field holding a gRPC canonical name, and it is far more
//! specific than the HTTP status it rides on. Several of those names carry
//! weight the HTTP code loses:
//!
//! | `status` | Normalized | Retry class | Why |
//! |---|---|---|---|
//! | `RESOURCE_EXHAUSTED`, per-minute | `RateLimited { retry_after }` | retry after the delay | Google puts the delay in a `RetryInfo` detail, not only in a header. |
//! | `RESOURCE_EXHAUSTED`, hard quota | `QuotaExhausted { scope }` | fall back | Waiting a minute never restores a daily quota or a credit balance. |
//! | `UNAUTHENTICATED`, expired | `CredentialExpired` | fall back, after a refresh | Routine on Vertex, whose token is short-lived by design. |
//! | `UNAUTHENTICATED`, anything else | `Authentication` | fall back | A missing or malformed credential. Refreshing will not help. |
//! | `PERMISSION_DENIED` | `Authorization` | fall back | The credential is valid and not entitled. Waiting cannot fix it; another candidate might. |
//! | `FAILED_PRECONDITION` | `Authorization` | fall back | Billing is off, or the tier is unavailable in this country. A configuration fault of *this* profile. |
//! | `INVALID_ARGUMENT` | `InvalidRequest`, or `ContextOverflow` when the message says the prompt is too long | fatal | Our request is wrong; a sibling model will reject it identically. |
//! | `UNAVAILABLE` | `Server { status }` | retry | The one Google failure that is genuinely worth trying again. |
//!
//! # Two splits Google makes and a single HTTP status does not
//!
//! **An expired token is not a bad token.** Vertex AI authenticates with an
//! OAuth access token that expires by design, so a 401 in a long-running
//! process is a routine, recoverable condition rather than a
//! misconfiguration — and folding it into a generic authentication failure
//! would make this adapter unusable for exactly the deployments it is for. The
//! documented answer is the
//! [`TokenSource`](crate::credential::TokenSource): the adapter consults it
//! **once per request**, so a caller that refreshes and retries the same
//! logical call gets a fresh token with no further ceremony. The two cases are
//! told apart by the message saying so; where it does not, the conservative
//! reading wins and the failure stays a plain `Authentication`, because
//! claiming expiry over a key that is simply wrong would send a caller into a
//! refresh loop.
//!
//! **A rate limit is not an exhausted quota.** `RESOURCE_EXHAUSTED` covers
//! both, and they want opposite handling: waiting fixes the first and never
//! fixes the second. They are told apart by the `QuotaFailure` detail, whose
//! `quotaId` names the window it counts (`…PerMinute…` against
//! `…PerDay…`), and by a message naming billing or credit. Where the response
//! carries neither — which happens, since the detail is optional — the
//! classification stays `RateLimited`. That is the conservative choice: its
//! `RetryAfter` class still permits moving to another candidate, so a
//! misread hard quota costs one delay, whereas a misread rate limit would
//! abandon a healthy provider outright.
//!
//! The safety and recitation *finish reasons* are the other half of this story
//! and live in [`wire::response`](crate::wire::response): a blocked answer is a
//! [`ContentFilter`](turnframe_provider::error::ProviderErrorKind::ContentFilter),
//! never a generic failure, because `ContentFilter` is `Fatal` and a generic
//! failure would be retried — and retrying elsewhere until a model complies is
//! a safety bypass, not a recovery.

use std::time::Duration;

use serde::Deserialize;
use turnframe_provider::error::{ErrorCode, ProviderError, ProviderErrorKind};
use turnframe_provider::secret::Redactor;

/// The `@type` of the detail Google puts a retry delay in.
const RETRY_INFO_TYPE: &str = "google.rpc.RetryInfo";

/// The `@type` of the detail Google describes an exhausted quota in.
const QUOTA_FAILURE_TYPE: &str = "google.rpc.QuotaFailure";

/// Code an expired credential carries, whatever kind it is mapped onto.
pub(crate) const CREDENTIAL_EXPIRED_CODE: &str = "credential_expired";

/// Code an exhausted quota or credit balance carries.
pub(crate) const QUOTA_EXHAUSTED_CODE: &str = "quota_exhausted";

/// The normalized failure an **expired** credential maps onto.
///
/// [`CredentialExpired`](ProviderErrorKind::CredentialExpired), whose class is
/// `Fallback`: a caller holding a [`TokenSource`](crate::credential::TokenSource)
/// may refresh and call this same profile again, and a caller without one moves
/// on. The code repeats the distinction for a log reader.
fn credential_expired() -> ProviderError {
    ProviderError::credential_expired().with_code(CREDENTIAL_EXPIRED_CODE)
}

/// The normalized failure an exhausted **quota or credit balance** maps onto.
///
/// [`QuotaExhausted`](ProviderErrorKind::QuotaExhausted), whose class is
/// `Fallback` — deliberately not `RateLimited`, whose `RetryAfter` class would
/// make a caller sit out a delay that refills nothing. `scope` names the quota
/// when the response named one, sanitized on the way in.
fn quota_exhausted(scope: Option<&str>) -> ProviderError {
    ProviderError::quota_exhausted(scope).with_code(QUOTA_EXHAUSTED_CODE)
}

/// The error envelope every Google API returns.
///
/// Streamed calls sometimes deliver the same object wrapped in a one-element
/// array, which [`ApiErrorEnvelope::decode`] unwraps.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiErrorEnvelope {
    #[serde(default)]
    pub(crate) error: Option<ApiError>,
}

/// The `error` object of a Google API failure.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiError {
    /// The HTTP status, repeated inside the body.
    #[serde(default)]
    pub(crate) code: Option<u16>,
    #[serde(default)]
    pub(crate) message: Option<String>,
    /// The gRPC canonical name, e.g. `"RESOURCE_EXHAUSTED"`.
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) details: Vec<ErrorDetail>,
}

/// One entry of `error.details`.
///
/// Only two of Google's detail types are read: `RetryInfo`, for how long to
/// wait, and `QuotaFailure`, for whether waiting helps at all.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ErrorDetail {
    #[serde(default, rename = "@type")]
    pub(crate) kind: Option<String>,
    /// A protobuf `Duration`, spelled `"17s"` or `"1.500s"`.
    #[serde(default, rename = "retryDelay")]
    pub(crate) retry_delay: Option<String>,
    /// The quotas a `QuotaFailure` says were hit.
    #[serde(default)]
    pub(crate) violations: Vec<QuotaViolation>,
}

/// One quota named by a `QuotaFailure` detail.
///
/// The identifiers are Google's own metric names — configuration labels, not
/// user text — and only their *shape* is read: whether the window they count
/// over is a minute or a day.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct QuotaViolation {
    #[serde(default, rename = "quotaId")]
    pub(crate) quota_id: Option<String>,
    #[serde(default, rename = "quotaMetric")]
    pub(crate) quota_metric: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
}

impl ApiErrorEnvelope {
    /// Decodes a body, falling back to an empty envelope for anything that is
    /// not the expected shape — a load balancer's HTML page, say.
    pub(crate) fn decode(body: &str) -> Self {
        // A streamed failure arrives as `[{"error": …}]`. The array form is
        // checked first because serde would otherwise read the array *as* the
        // struct's fields in order and produce a silently empty envelope.
        if body.trim_start().starts_with('[') {
            return serde_json::from_str::<Vec<Self>>(body)
                .ok()
                .and_then(|wrapped| wrapped.into_iter().find(Self::has_error))
                .unwrap_or_default();
        }
        serde_json::from_str::<Self>(body).unwrap_or_default()
    }

    /// Returns `true` when the body actually was a Google failure envelope.
    pub(crate) const fn has_error(&self) -> bool {
        self.error.is_some()
    }

    /// Google's canonical status name, upper-cased.
    fn status(&self) -> String {
        self.error
            .as_ref()
            .and_then(|error| error.status.as_deref())
            .unwrap_or_default()
            .trim()
            .to_ascii_uppercase()
    }

    /// The HTTP code the body repeats, when it does.
    fn code(&self) -> Option<u16> {
        self.error.as_ref().and_then(|error| error.code)
    }

    /// The human message. Used only to classify and to read integers out of;
    /// never stored.
    fn message(&self) -> String {
        self.error
            .as_ref()
            .and_then(|error| error.message.as_deref())
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    /// The delay a `RetryInfo` detail asks for.
    fn retry_delay(&self) -> Option<Duration> {
        let details = &self.error.as_ref()?.details;
        details
            .iter()
            .filter(|detail| {
                detail
                    .kind
                    .as_deref()
                    .is_some_and(|kind| kind.ends_with(RETRY_INFO_TYPE))
            })
            .find_map(|detail| parse_proto_duration(detail.retry_delay.as_deref()?))
    }

    /// Every quota a `QuotaFailure` detail names, lower-cased and joined.
    fn quota_labels(&self) -> String {
        let Some(error) = self.error.as_ref() else {
            return String::new();
        };
        let mut labels = String::new();
        for detail in &error.details {
            let quota_failure = detail
                .kind
                .as_deref()
                .is_some_and(|kind| kind.ends_with(QUOTA_FAILURE_TYPE));
            if !quota_failure {
                continue;
            }
            for violation in &detail.violations {
                for field in [
                    violation.quota_id.as_deref(),
                    violation.quota_metric.as_deref(),
                    violation.description.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    labels.push_str(&field.to_ascii_lowercase());
                    labels.push(' ');
                }
            }
        }
        labels
    }
}

/// Maps one failed HTTP exchange onto a normalized failure.
///
/// `retry_after_hint` comes from the response headers (see [`retry_after`]);
/// a `RetryInfo` detail in the body wins over it, because it is the delay the
/// service itself computed for this call.
pub(crate) fn classify(
    status: u16,
    hints: &ResponseHints,
    envelope: &ApiErrorEnvelope,
    redactor: &dyn Redactor,
) -> ProviderError {
    let canonical = envelope.status();
    let message = envelope.message();
    let delay = envelope.retry_delay().or(hints.retry_after);
    // The body repeats the HTTP status, and it is the only one a stream chunk
    // has, since a mid-stream failure arrives over a 200.
    let http = if status == 0 {
        envelope.code().unwrap_or(0)
    } else {
        status
    };
    let unauthenticated = canonical == "UNAUTHENTICATED" || (canonical.is_empty() && http == 401);

    let error = if is_context_overflow(&canonical, &message) {
        let (needed, limit) = context_numbers(&message);
        ProviderError::context_overflow(needed, limit)
    } else if is_content_filter(&message) {
        ProviderError::content_filter()
    } else if unauthenticated && is_expired_credential(&message, hints.www_authenticate) {
        credential_expired()
    } else if http == 402 {
        quota_exhausted(Some("credit_balance"))
    } else if let Some(scope) = hard_quota_scope(&message, &envelope.quota_labels()) {
        quota_exhausted(Some(scope))
    } else {
        match canonical.as_str() {
            // Not a hard quota, by the branch above: a window that reopens.
            "RESOURCE_EXHAUSTED" => ProviderError::rate_limited(delay),
            "UNAUTHENTICATED" => ProviderError::authentication(),
            // Valid credentials, no entitlement: billing disabled, an API not
            // enabled on the project, a region the tier does not serve.
            "PERMISSION_DENIED" | "FAILED_PRECONDITION" => ProviderError::authorization(),
            "NOT_FOUND" => ProviderError::model_not_found(),
            "INVALID_ARGUMENT" | "OUT_OF_RANGE" | "ALREADY_EXISTS" => {
                ProviderError::new(ProviderErrorKind::InvalidRequest)
            }
            "DEADLINE_EXCEEDED" => ProviderError::timeout(),
            "CANCELLED" => ProviderError::cancelled(),
            "UNIMPLEMENTED" => ProviderError::unsupported("model_method"),
            // `UNAVAILABLE` is the retryable one; `INTERNAL`, `UNKNOWN`,
            // `DATA_LOSS` and `ABORTED` are all server-side too.
            "UNAVAILABLE" | "INTERNAL" | "UNKNOWN" | "DATA_LOSS" | "ABORTED" => {
                ProviderError::server(Some(server_status(http, &canonical)))
            }
            _ => by_status(http, delay),
        }
    };

    let error = attach_code(error, &canonical, http, redactor);
    if message.is_empty() {
        error
    } else {
        error.with_detail(redactor.redact(&message))
    }
}

/// Recognizes a credential that was valid and has expired, as opposed to one
/// that was never right.
///
/// Google says so in two places, and both are checked: the `WWW-Authenticate`
/// challenge's `error_description`, and the message itself. Neither saying so
/// means the answer is no — a wrong key and an expired token share a status
/// code, and guessing "expired" would send a caller into a refresh loop over a
/// credential no refresh can fix.
fn is_expired_credential(message: &str, www_authenticate: Option<&str>) -> bool {
    let challenge = www_authenticate.unwrap_or_default().to_ascii_lowercase();
    message.contains("expired")
        || message.contains("token is not valid anymore")
        || challenge.contains("expired")
}

/// Names the quota when no amount of waiting reopens it.
///
/// `labels` is the lower-cased text of every `QuotaFailure` violation. A quota
/// counted per day, per month or over a lifetime is spent; one counted per
/// minute is a window that reopens. A message naming billing or a credit
/// balance is spent whatever the detail says.
///
/// Returns the scope to record, or `None` when the response does not say the
/// quota is spent — which is the conservative reading, since `RateLimited`
/// still permits moving to another candidate while `QuotaExhausted` forbids
/// waiting.
fn hard_quota_scope(message: &str, labels: &str) -> Option<&'static str> {
    const LONG_WINDOWS: &[(&str, &str)] = &[
        ("perday", "per_day"),
        ("per day", "per_day"),
        ("daily", "per_day"),
        ("permonth", "per_month"),
        ("per month", "per_month"),
        ("lifetime", "lifetime"),
    ];
    const SHORT_WINDOWS: &[&str] = &["perminute", "per minute", "persecond", "per second"];
    const BILLING: &[&str] = &[
        "billing",
        "credit balance",
        "insufficient credit",
        "insufficient funds",
        "out of credits",
        "upgrade your plan",
        "spending limit",
    ];

    if BILLING.iter().any(|marker| message.contains(marker)) {
        return Some("credit_balance");
    }
    let short = SHORT_WINDOWS.iter().any(|marker| labels.contains(marker));
    if short {
        return None;
    }
    LONG_WINDOWS
        .iter()
        .find(|(marker, _)| labels.contains(marker))
        .map(|(_, scope)| *scope)
}

/// The header-borne hints a failed exchange carries.
///
/// A streamed failure has none, because it arrives inside a chunk of a 200.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ResponseHints<'a> {
    /// The delay a `Retry-After` header asked for.
    pub(crate) retry_after: Option<Duration>,
    /// The `WWW-Authenticate` challenge, which says why a 401 happened.
    pub(crate) www_authenticate: Option<&'a str>,
}

impl<'a> ResponseHints<'a> {
    /// Reads both hints out of a header map.
    pub(crate) fn from_headers(headers: &'a reqwest::header::HeaderMap) -> Self {
        Self {
            retry_after: retry_after(headers),
            www_authenticate: headers
                .get("www-authenticate")
                .and_then(|value| value.to_str().ok()),
        }
    }
}

/// The status a `Server` failure reports when the body carried a canonical name
/// but the exchange had no usable HTTP status (a mid-stream error chunk).
fn server_status(http: u16, canonical: &str) -> u16 {
    if (500..=599).contains(&http) {
        return http;
    }
    match canonical {
        "UNAVAILABLE" => 503,
        "ABORTED" => 409,
        _ => 500,
    }
}

/// Status-only classification, used when the body names no canonical status.
fn by_status(status: u16, delay: Option<Duration>) -> ProviderError {
    match status {
        400 | 409 | 422 => ProviderError::new(ProviderErrorKind::InvalidRequest),
        401 => ProviderError::authentication(),
        403 => ProviderError::authorization(),
        404 => ProviderError::model_not_found(),
        408 => ProviderError::timeout(),
        // A payload the endpoint refuses to read is a prompt that must shrink,
        // which is what `ContextOverflow` tells the runtime to do.
        413 => ProviderError::context_overflow(None, None),
        429 => ProviderError::rate_limited(delay),
        499 => ProviderError::cancelled(),
        504 => ProviderError::timeout(),
        500..=599 => ProviderError::server(Some(status)),
        other => ProviderError::other(format!("http_{other}")),
    }
}

/// Attaches Google's canonical status name as the machine code, redacted and
/// sanitized. Falls back to the HTTP status when the body named none.
///
/// A code the classification already chose — `credential_expired`,
/// `quota_exhausted` — is the more specific one and is left alone.
fn attach_code(
    error: ProviderError,
    canonical: &str,
    http: u16,
    redactor: &dyn Redactor,
) -> ProviderError {
    if error.code().is_some() {
        return error;
    }
    if canonical.is_empty() {
        if http == 0 {
            return error;
        }
        return error.with_code(format!("http_{http}"));
    }
    let masked = redactor.redact(canonical);
    error.with_code(ErrorCode::new(masked).as_str())
}

/// Recognizes a prompt that did not fit, across the spellings Google uses.
fn is_context_overflow(canonical: &str, message: &str) -> bool {
    if canonical == "OUT_OF_RANGE" {
        return true;
    }
    message.contains("exceeds the maximum number of tokens")
        || message.contains("input token count")
        || message.contains("token count exceeds")
        || message.contains("request payload size exceeds")
        || message.contains("context length")
        || message.contains("too many tokens")
}

/// Recognizes a safety refusal that arrived as an error rather than as a
/// finish reason.
fn is_content_filter(message: &str) -> bool {
    message.contains("blocked due to safety")
        || message.contains("blocked by the safety")
        || message.contains("safety_settings")
        || message.contains("prohibited content")
        || message.contains("content policy")
}

/// Reads the two integers out of a context-overflow message.
///
/// Integers are not secrets and they are what makes the failure actionable, so
/// they are the one thing lifted out of a message. Anything unrecognized yields
/// `None`, never a guess.
///
/// Returns `(needed, limit)`.
fn context_numbers(message: &str) -> (Option<u64>, Option<u64>) {
    let needed = number_after(message, "input token count")
        .or_else(|| number_after(message, "token count"))
        .or_else(|| number_after(message, "request payload size exceeds the limit"));
    let limit = number_after(message, "maximum number of tokens allowed")
        .or_else(|| number_after(message, "maximum context length is"));
    (needed, limit)
}

/// The first integer that follows `marker`, in or out of parentheses.
fn number_after(message: &str, marker: &str) -> Option<u64> {
    let rest = message.split_once(marker)?.1;
    let digits: String = rest
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Parses a protobuf `Duration` as JSON renders it: seconds with an `s` suffix,
/// possibly fractional.
fn parse_proto_duration(raw: &str) -> Option<Duration> {
    let seconds: f64 = raw.trim().strip_suffix('s')?.parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(seconds))
}

/// Reads a retry hint out of the response headers.
///
/// An HTTP-date `Retry-After` is ignored rather than approximated: a wrong
/// delay is worse than none.
pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get("retry-after")?.to_str().ok()?.trim();
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
/// request URL, and a Vertex URL carries the project id.
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
    use serde_json::json;
    use turnframe_provider::error::RetryClass;
    use turnframe_provider::secret::{ApiKey, DefaultRedactor};

    fn envelope(value: serde_json::Value) -> ApiErrorEnvelope {
        ApiErrorEnvelope::decode(&value.to_string())
    }

    fn classified(status: u16, value: serde_json::Value) -> ProviderError {
        classify(
            status,
            &ResponseHints::default(),
            &envelope(value),
            &DefaultRedactor::new(),
        )
    }

    fn code_of(error: &ProviderError) -> Option<String> {
        error.code().map(|code| code.as_str().to_owned())
    }

    fn google(code: u16, status: &str, message: &str) -> serde_json::Value {
        json!({"error": {"code": code, "message": message, "status": status}})
    }

    #[test]
    fn resource_exhausted_is_a_rate_limit_that_keeps_googles_own_delay() {
        let error = classified(
            429,
            json!({"error": {
                "code": 429,
                "message": "Quota exceeded for quota metric 'Generate Content requests'.",
                "status": "RESOURCE_EXHAUSTED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": "17s"
                }]
            }}),
        );
        assert_eq!(error.retry_after(), Some(Duration::from_secs(17)));
        assert_eq!(error.retry_class(), RetryClass::RetryAfter);
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("RESOURCE_EXHAUSTED".to_owned())
        );
        // Nothing of the message survives.
        assert!(!error.to_string().contains("Quota exceeded"), "{error}");
    }

    #[test]
    fn a_retry_info_detail_beats_the_header_and_a_fractional_delay_survives() {
        let body = json!({"error": {
            "code": 429, "status": "RESOURCE_EXHAUSTED", "message": "slow down",
            "details": [
                {"@type": "type.googleapis.com/google.rpc.QuotaFailure"},
                {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "1.500s"}
            ]
        }});
        let error = classify(
            429,
            &ResponseHints {
                retry_after: Some(Duration::from_secs(60)),
                www_authenticate: None,
            },
            &envelope(body),
            &DefaultRedactor::new(),
        );
        assert_eq!(error.retry_after(), Some(Duration::from_millis(1500)));

        // With no detail, the header is used.
        let header_only = classify(
            429,
            &ResponseHints {
                retry_after: Some(Duration::from_secs(3)),
                www_authenticate: None,
            },
            &envelope(google(429, "RESOURCE_EXHAUSTED", "slow down")),
            &DefaultRedactor::new(),
        );
        assert_eq!(header_only.retry_after(), Some(Duration::from_secs(3)));
    }

    #[test]
    fn permission_denied_and_failed_precondition_fall_back_rather_than_wait() {
        for status in ["PERMISSION_DENIED", "FAILED_PRECONDITION"] {
            let error = classified(
                403,
                google(403, status, "The caller does not have permission."),
            );
            assert!(
                matches!(error.kind(), ProviderErrorKind::Authorization),
                "{status} mapped to {}",
                error.kind().as_str()
            );
            assert_eq!(error.retry_class(), RetryClass::Fallback);
        }
    }

    #[test]
    fn an_expired_token_is_told_apart_from_a_credential_that_was_never_right() {
        let expired = classified(
            401,
            google(
                401,
                "UNAUTHENTICATED",
                "Request had invalid authentication credentials. \
                 OAuth 2 access token has expired.",
            ),
        );
        assert!(matches!(
            expired.kind(),
            ProviderErrorKind::CredentialExpired
        ));
        assert_eq!(code_of(&expired), Some(CREDENTIAL_EXPIRED_CODE.to_owned()));
        assert_eq!(expired.retry_class(), RetryClass::Fallback);
        // Nothing of the message survives into the rendering.
        assert!(!expired.to_string().contains("OAuth 2"), "{expired}");

        // The challenge header says so too, and is believed.
        let by_header = classify(
            401,
            &ResponseHints {
                retry_after: None,
                www_authenticate: Some(
                    "Bearer error=\"invalid_token\", error_description=\"The token expired\"",
                ),
            },
            &envelope(google(
                401,
                "UNAUTHENTICATED",
                "invalid authentication credentials",
            )),
            &DefaultRedactor::new(),
        );
        assert_eq!(
            code_of(&by_header),
            Some(CREDENTIAL_EXPIRED_CODE.to_owned())
        );

        // A key that was simply wrong is a plain authentication failure: no
        // refresh will fix it, and saying otherwise invites a refresh loop.
        let wrong = classified(
            401,
            google(
                401,
                "UNAUTHENTICATED",
                "API key not valid. Please pass a valid API key.",
            ),
        );
        assert!(matches!(wrong.kind(), ProviderErrorKind::Authentication));
        assert_eq!(code_of(&wrong), Some("UNAUTHENTICATED".to_owned()));
        assert_eq!(wrong.retry_class(), RetryClass::Fallback);
    }

    #[test]
    fn a_spent_quota_falls_back_while_a_rate_limit_waits() {
        // A quota counted per day is spent: waiting a minute changes nothing.
        let daily = classified(
            429,
            json!({"error": {
                "code": 429,
                "status": "RESOURCE_EXHAUSTED",
                "message": "Quota exceeded for quota metric 'Generate Content requests'.",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                    "violations": [{
                        "quotaMetric": "generativelanguage.googleapis.com/generate_requests",
                        "quotaId": "GenerateRequestsPerDayPerProjectPerModel-FreeTier"
                    }]
                }]
            }}),
        );
        assert!(matches!(
            daily.kind(),
            ProviderErrorKind::QuotaExhausted { scope: Some(scope) } if scope.as_str() == "per_day"
        ));
        assert_eq!(code_of(&daily), Some(QUOTA_EXHAUSTED_CODE.to_owned()));
        assert_eq!(daily.retry_class(), RetryClass::Fallback);
        assert_eq!(daily.retry_after(), None);

        // A credit balance is the same story with different words.
        let billing = classified(
            429,
            google(
                429,
                "RESOURCE_EXHAUSTED",
                "You have insufficient credit balance. Please purchase more credits.",
            ),
        );
        assert!(matches!(
            billing.kind(),
            ProviderErrorKind::QuotaExhausted { scope: Some(scope) }
                if scope.as_str() == "credit_balance"
        ));
        assert_eq!(billing.retry_class(), RetryClass::Fallback);

        // A quota counted per minute is a window that reopens.
        let minute = classified(
            429,
            json!({"error": {
                "code": 429,
                "status": "RESOURCE_EXHAUSTED",
                "message": "Quota exceeded.",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                        "violations": [{"quotaId": "GenerateRequestsPerMinutePerProject"}]
                    },
                    {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "9s"}
                ]
            }}),
        );
        assert!(matches!(
            minute.kind(),
            ProviderErrorKind::RateLimited { .. }
        ));
        assert_eq!(minute.retry_class(), RetryClass::RetryAfter);
        assert_eq!(minute.retry_after(), Some(Duration::from_secs(9)));

        // With no detail at all the response cannot say which it is, and the
        // conservative reading keeps the door to both waiting and falling back.
        let ambiguous = classified(429, google(429, "RESOURCE_EXHAUSTED", "Quota exceeded."));
        assert!(matches!(
            ambiguous.kind(),
            ProviderErrorKind::RateLimited { .. }
        ));
        assert!(ambiguous.retry_class().allows_another_candidate());
    }

    #[test]
    fn a_payment_required_status_is_a_spent_balance_whatever_the_body_says() {
        let error = classified(402, json!({}));
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::QuotaExhausted { .. }
        ));
        assert_eq!(code_of(&error), Some(QUOTA_EXHAUSTED_CODE.to_owned()));
        assert_eq!(error.retry_class(), RetryClass::Fallback);
    }

    #[test]
    fn invalid_argument_is_fatal_and_unavailable_is_retryable() {
        let invalid = classified(
            400,
            google(400, "INVALID_ARGUMENT", "Invalid JSON payload received."),
        );
        assert!(matches!(invalid.kind(), ProviderErrorKind::InvalidRequest));
        assert_eq!(invalid.retry_class(), RetryClass::Fatal);

        let unavailable = classified(
            503,
            google(
                503,
                "UNAVAILABLE",
                "The model is overloaded. Please try again later.",
            ),
        );
        assert!(matches!(
            unavailable.kind(),
            ProviderErrorKind::Server { status: Some(503) }
        ));
        assert_eq!(unavailable.retry_class(), RetryClass::Retry);

        let internal = classified(500, google(500, "INTERNAL", "internal error"));
        assert_eq!(internal.retry_class(), RetryClass::Retry);
    }

    #[test]
    fn a_prompt_that_did_not_fit_gets_its_own_variant_and_its_two_numbers() {
        let error = classified(
            400,
            google(
                400,
                "INVALID_ARGUMENT",
                "The input token count (1250000) exceeds the maximum number of tokens \
                 allowed (1048576).",
            ),
        );
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::ContextOverflow {
                needed_tokens: Some(1_250_000),
                limit_tokens: Some(1_048_576)
            }
        ));
        assert_eq!(error.retry_class(), RetryClass::Fatal);

        // A payload too large for the transport is the same instruction:
        // shrink the prompt.
        let oversized = classified(413, json!({}));
        assert!(matches!(
            oversized.kind(),
            ProviderErrorKind::ContextOverflow { .. }
        ));
    }

    #[test]
    fn a_safety_rejection_that_arrives_as_an_error_is_still_a_content_filter() {
        let error = classified(
            400,
            google(
                400,
                "INVALID_ARGUMENT",
                "The response was blocked due to safety concerns.",
            ),
        );
        assert!(matches!(error.kind(), ProviderErrorKind::ContentFilter));
        assert_eq!(error.retry_class(), RetryClass::Fatal);
    }

    #[test]
    fn a_body_that_is_not_a_google_envelope_is_classified_by_status_alone() {
        let html = ApiErrorEnvelope::decode("<html><body>502 Bad Gateway</body></html>");
        let error = classify(
            502,
            &ResponseHints::default(),
            &html,
            &DefaultRedactor::new(),
        );
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::Server { status: Some(502) }
        ));
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("http_502".to_owned())
        );
        // And nothing of the page survives.
        assert!(!error.to_string().contains("Bad Gateway"), "{error}");
    }

    #[test]
    fn a_streamed_failure_arrives_wrapped_in_an_array() {
        let envelope = ApiErrorEnvelope::decode(
            &json!([{"error": {"code": 500, "status": "INTERNAL", "message": "boom"}}]).to_string(),
        );
        assert_eq!(envelope.status(), "INTERNAL");
        // A chunk carries no HTTP status of its own; the body's code is used.
        let error = classify(
            0,
            &ResponseHints::default(),
            &envelope,
            &DefaultRedactor::new(),
        );
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::Server { status: Some(500) }
        ));
    }

    #[test]
    fn a_credential_echoed_into_a_status_name_is_masked_before_it_becomes_a_code() {
        let key = ApiKey::new("AIzaSyPlanted0123456789");
        let redactor = DefaultRedactor::new().with_secret(&key);
        let error = classify(
            403,
            &ResponseHints::default(),
            &envelope(google(
                403,
                "PERMISSION_DENIED AIzaSyPlanted0123456789",
                "no",
            )),
            &redactor,
        );
        let rendered = error.to_string();
        assert!(!rendered.contains("AIzaSyPlanted"), "{rendered}");
        assert!(rendered.contains("PERMISSION_DENIED"), "{rendered}");
    }

    #[test]
    fn protobuf_durations_parse_only_when_they_are_durations() {
        assert_eq!(parse_proto_duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_proto_duration(" 0.250s "),
            Some(Duration::from_millis(250))
        );
        assert_eq!(parse_proto_duration("5"), None);
        assert_eq!(parse_proto_duration("-1s"), None);
        assert_eq!(parse_proto_duration("nans"), None);
    }

    #[test]
    fn header_retry_hints_accept_seconds_and_ignore_dates() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "7".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(7)));
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
        assert_eq!(retry_after(&reqwest::header::HeaderMap::new()), None);
    }

    #[test]
    fn unclassifiable_statuses_still_get_a_class() {
        for (status, expected) in [
            (404_u16, "model_not_found"),
            (408, "timeout"),
            (499, "cancelled"),
            (504, "timeout"),
            (418, "other"),
        ] {
            let error = classified(status, json!({}));
            assert_eq!(error.kind().as_str(), expected, "status {status}");
        }
        let unimplemented = classified(501, google(501, "UNIMPLEMENTED", "no"));
        assert!(matches!(
            unimplemented.kind(),
            ProviderErrorKind::Unsupported { .. }
        ));
        assert_eq!(unimplemented.retry_class(), RetryClass::Fallback);
    }
}
