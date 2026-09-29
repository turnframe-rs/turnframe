//! Vendor failure → normalized failure (spec §20.7, §24, §25.2).
//!
//! Two rules shape this module.
//!
//! **Nothing from the wire reaches the error.** A status code and an error body
//! are read, classified and thrown away. What survives is a
//! [`ProviderErrorKind`], a short [`ErrorCode`] — sanitized by
//! [`ErrorCode::new`] and passed through the configured [`Redactor`] first, so
//! a proxy that echoes a bearer token into its error code cannot get it into a
//! log line — and, for a context overflow, the two integers the message named.
//! No header, no message, no body, ever.
//!
//! **Classification is by meaning, not by status.** Ollama's error body is a
//! bare `{"error": "…"}` string, so the message *is* the machine-readable
//! signal; there is no code field to lead with. Four readings matter enough to
//! be named:
//!
//! | On the wire | Normalized | Why |
//! |---|---|---|
//! | 404 `model "x" not found, try pulling it first` | [`ModelNotFound`](ProviderErrorKind::ModelNotFound) | The commonest failure of a local runtime by a wide margin, and it has a fix a human can act on: `ollama pull x`. Flattening it into a generic failure hides the one thing worth saying. |
//! | connection refused | [`Transport`](ProviderErrorKind::Transport), code [`DAEMON_UNREACHABLE_CODE`] | Nothing is listening. The code says so in words, because "transport error" sends an adopter reading their network configuration when the answer is that `ollama serve` is not running. |
//! | a message naming the context window | [`ContextOverflow`](ProviderErrorKind::ContextOverflow) | Fixed by shrinking the prompt or raising `num_ctx`; a generic bad request is fixed by neither. |
//! | 400 the daemon rejected | [`InvalidRequest`](ProviderErrorKind::InvalidRequest) | Our request is wrong and every other provider will reject it too. |
//!
//! # The two rows a bare daemon cannot produce
//!
//! An unauthenticated local runtime issues no credential and meters no quota,
//! so [`CredentialExpired`](ProviderErrorKind::CredentialExpired) and
//! [`QuotaExhausted`](ProviderErrorKind::QuotaExhausted) are unreachable there
//! — which the conformance fixtures declare rather than skip. They are mapped
//! all the same, because the same adapter serves a **proxied** instance: an
//! authenticating gateway in front of Ollama, or a hosted Ollama-compatible
//! runtime, really does answer 401 with an expired bearer token and 402 or 429
//! with a spent balance. Both are recognized **before** the status is
//! consulted, so a billing 429 never reaches the rate-limit branch and an
//! expired token never reaches the plain authentication one.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use turnframe_provider::error::{ErrorCode, ProviderError, ProviderErrorKind};
use turnframe_provider::secret::Redactor;

/// The code attached when the daemon cannot be reached at all.
///
/// It is deliberately a sentence rather than a label: this is the first thing
/// anyone running Ollama locally will hit, and the answer is almost always that
/// the daemon is not running.
///
/// ```
/// use turnframe_provider_ollama::DAEMON_UNREACHABLE_CODE;
///
/// assert_eq!(
///     DAEMON_UNREACHABLE_CODE,
///     "ollama_unreachable_is_the_daemon_running"
/// );
/// ```
pub const DAEMON_UNREACHABLE_CODE: &str = "ollama_unreachable_is_the_daemon_running";

/// The code attached when the configured model has not been pulled.
///
/// ```
/// use turnframe_provider_ollama::MODEL_NOT_PULLED_CODE;
///
/// assert_eq!(MODEL_NOT_PULLED_CODE, "model_not_pulled");
/// ```
pub const MODEL_NOT_PULLED_CODE: &str = "model_not_pulled";

/// Quota scope reported when a prepaid balance is spent.
pub const QUOTA_SCOPE_CREDIT_BALANCE: &str = "credit_balance";

/// Quota scope reported when the endpoint says only that a quota is exhausted.
pub const QUOTA_SCOPE_QUOTA: &str = "quota";

/// The status a stream-borne error is classified as when nothing says more.
///
/// A mid-stream failure arrives inside a `200`, so there is no status to read.
/// Treating an unrecognized one as a server error makes it retryable, which is
/// the safe reading of "the connection said something we do not model".
const STREAM_FALLBACK_STATUS: u16 = 500;

/// The error envelope, in every shape this endpoint and its proxies use.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiErrorEnvelope {
    /// Ollama itself answers `{"error": "…"}`; proxies wrap it in an object.
    #[serde(default)]
    pub(crate) error: Option<ErrorField>,
    /// Some proxies answer with a bare `{"message": …}`.
    #[serde(default)]
    pub(crate) message: Option<String>,
    /// Gateways in front of Python services answer with `{"detail": …}`.
    #[serde(default)]
    pub(crate) detail: Option<String>,
}

/// The `error` field: a plain string on Ollama, an object on a proxy.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum ErrorField {
    /// What the daemon itself sends.
    Text(String),
    /// What an OpenAI-shaped proxy in front of it sends.
    Object {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        r#type: Option<String>,
        #[serde(default)]
        code: Option<String>,
    },
}

impl ApiErrorEnvelope {
    /// Decodes a body, falling back to an empty envelope for anything that is
    /// not JSON — a gateway's HTML error page, say.
    pub(crate) fn decode(body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_default()
    }

    /// An envelope holding whatever a chunk's `error` field carried.
    pub(crate) fn of_value(value: &Value) -> Self {
        serde_json::from_value(value.clone()).map_or_else(
            |_| Self {
                error: Some(ErrorField::Text(value.to_string())),
                ..Self::default()
            },
            |error| Self {
                error: Some(error),
                ..Self::default()
            },
        )
    }

    /// The endpoint's own machine code, when it has one. The daemon has none.
    fn code(&self) -> Option<&str> {
        match self.error.as_ref()? {
            ErrorField::Text(_) => None,
            ErrorField::Object { code, r#type, .. } => code
                .as_deref()
                .or(r#type.as_deref())
                .filter(|code| !code.is_empty()),
        }
    }

    /// The human message, from whichever field carries it. Used only to
    /// classify and to read integers out of; never stored.
    fn message(&self) -> Option<&str> {
        let from_error = match self.error.as_ref() {
            Some(ErrorField::Text(text)) => Some(text.as_str()),
            Some(ErrorField::Object { message, .. }) => message.as_deref(),
            None => None,
        };
        from_error
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

    // Recognized before the status is consulted, so a billing 429 never reaches
    // the rate-limit branch and an expired token never reaches the plain
    // authentication one.
    if let Some(scope) = quota_scope(&code, status, &message) {
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

    if is_model_not_pulled(&code, status, &message) {
        // The code is this crate's own constant, so it needs no redaction: it
        // is the actionable half of the failure and it names the fix.
        return ProviderError::model_not_found().with_code(MODEL_NOT_PULLED_CODE);
    }

    let error = if is_context_overflow(&code, &message) {
        let (needed, limit) = context_numbers(&message);
        ProviderError::context_overflow(needed, limit)
    } else if is_content_filter(&code, &message) {
        ProviderError::content_filter()
    } else {
        by_status(status, retry_after_hint)
    };

    attach_detail(
        attach_code(error, &raw_code, redactor),
        &raw_message,
        redactor,
    )
}

/// Maps an `error` object carried inside a streamed chunk, which arrives on a
/// `200` and so has no status of its own.
pub(crate) fn classify_stream(reported: &Value, redactor: &dyn Redactor) -> ProviderError {
    let envelope = ApiErrorEnvelope::of_value(reported);
    classify(STREAM_FALLBACK_STATUS, None, &envelope, redactor)
}

/// Status-only classification, used when the body says nothing recognizable.
fn by_status(status: u16, retry_after_hint: Option<Duration>) -> ProviderError {
    match status {
        400 | 409 | 422 => ProviderError::new(ProviderErrorKind::InvalidRequest),
        401 => ProviderError::authentication(),
        // A proxy that meters usage answers 402 when the account owes money.
        402 => ProviderError::quota_exhausted(Some(QUOTA_SCOPE_QUOTA)),
        403 => ProviderError::authorization(),
        // The daemon's own 404 is "that model is not pulled"; a proxy's is
        // "that route does not exist". Both mean this profile cannot serve the
        // call and another candidate might.
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

/// Recognizes the model this daemon has not pulled yet.
///
/// The daemon's own wording is `model "llama3.2" not found, try pulling it
/// first`, on a 404. The status alone would already be enough, but a proxy may
/// deliver the same message on a 400 or a 500, and the message is unmistakable.
fn is_model_not_pulled(code: &str, status: u16, message: &str) -> bool {
    matches!(code, "model_not_found" | "not_found_error")
        || message.contains("try pulling it first")
        || message.contains("no such model")
        || message.contains("model not found")
        || (message.contains("not found") && message.contains("model"))
        || (status == 404 && message.contains("pull"))
}

/// Recognizes a context overflow across the spellings the runners use.
fn is_context_overflow(code: &str, message: &str) -> bool {
    matches!(code, "context_length_exceeded" | "context_window_exceeded")
        || message.contains("context length")
        || message.contains("context window")
        || message.contains("context size")
        || message.contains("prompt is too long")
        || message.contains("input is too large")
        || message.contains("too many tokens")
}

/// Recognizes a safety filter. A bare daemon runs none; a policy gateway does.
fn is_content_filter(code: &str, message: &str) -> bool {
    matches!(code, "content_filter" | "content_policy_violation")
        || message.contains("content policy")
        || message.contains("content filter")
        || message.contains("blocked by policy")
}

/// Recognizes a credential that was valid and has lapsed.
///
/// Only ever true in front of an authenticating proxy: the daemon issues no
/// credential, so it has none to expire.
fn is_expired_credential(code: &str, status: u16, message: &str) -> bool {
    if matches!(
        code,
        "token_expired" | "expired_token" | "credential_expired"
    ) {
        return true;
    }
    let expired = message.contains("expired")
        || message.contains("has lapsed")
        || message.contains("token is no longer valid");
    expired && matches!(status, 401 | 403)
}

/// Recognizes an exhausted quota, which is not a rate limit.
///
/// Returns the scope the message named, so the failure says *which* limit was
/// hit without carrying a word of the body.
fn quota_scope(code: &str, status: u16, message: &str) -> Option<&'static str> {
    if matches!(code, "insufficient_quota" | "quota_exceeded") {
        return Some(QUOTA_SCOPE_QUOTA);
    }
    if message.contains("credit balance")
        || message.contains("insufficient credits")
        || message.contains("out of credits")
    {
        return Some(QUOTA_SCOPE_CREDIT_BALANCE);
    }
    if message.contains("quota") || (status == 402 && message.contains("payment")) {
        return Some(QUOTA_SCOPE_QUOTA);
    }
    None
}

/// Reads the size and the limit out of a context-overflow message.
///
/// Integers are not secrets and they are what makes the failure actionable, so
/// they are the one thing lifted out of a message. Anything unrecognized yields
/// `None`, never a guess.
///
/// Returns `(needed, limit)`.
fn context_numbers(message: &str) -> (Option<u64>, Option<u64>) {
    // The runners' commonest spelling is `N tokens > M maximum`.
    if let Some((left, right)) = message.split_once('>') {
        let needed = last_number(left);
        let limit = first_number(right);
        if needed.is_some() && limit.is_some() {
            return (needed, limit);
        }
    }
    let limit = number_after(message, "context length is")
        .or_else(|| number_after(message, "context size is"))
        .or_else(|| number_after(message, "maximum of"));
    let needed = number_after(message, "input length is")
        .or_else(|| number_after(message, "resulted in"))
        .or_else(|| number_after(message, "requested"));
    (needed, limit)
}

/// The first integer that follows `marker`.
fn number_after(message: &str, marker: &str) -> Option<u64> {
    first_number(message.split_once(marker)?.1)
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

/// The last integer in `text`.
fn last_number(text: &str) -> Option<u64> {
    let mut best = None;
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            best = current.parse().ok().or(best);
            current.clear();
        }
    }
    if current.is_empty() {
        best
    } else {
        current.parse().ok().or(best)
    }
}

/// Reads a retry hint out of the response headers.
///
/// The daemon sends none; a proxy in front of it may. `retry-after-ms` wins
/// because it is the precise one, and an HTTP-date `Retry-After` is ignored
/// rather than approximated: a wrong delay is worse than none.
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
///
/// The connect case is the one worth its own code. Everybody who runs this
/// adapter hits it on their first afternoon, and "transport error" sends them
/// reading their network configuration when the answer is that nothing is
/// listening on port 11434.
pub(crate) fn transport(error: &reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        return ProviderError::timeout();
    }
    if error.is_connect() {
        return ProviderError::transport(DAEMON_UNREACHABLE_CODE);
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

    fn redactor() -> DefaultRedactor {
        DefaultRedactor::new().with_secret(&ApiKey::new("sk-planted-0123456789abcdef"))
    }

    fn map(status: u16, body: &str) -> ProviderError {
        classify(status, None, &ApiErrorEnvelope::decode(body), &redactor())
    }

    #[test]
    fn a_model_that_is_not_pulled_is_not_a_generic_failure() {
        // The daemon's own words, verbatim, on its own status.
        let error = map(
            404,
            "{\"error\":\"model \\\"llama3.2\\\" not found, try pulling it first\"}",
        );
        assert!(matches!(error.kind(), ProviderErrorKind::ModelNotFound));
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some(MODEL_NOT_PULLED_CODE.to_owned())
        );
        // Another candidate may have the model; this one will not grow it.
        assert_eq!(error.retry_class(), RetryClass::Fallback);
        // And the daemon's prose does not survive into the error.
        assert!(!error.to_string().contains("llama3.2"), "{error}");
    }

    #[test]
    fn a_bare_404_is_still_a_missing_model() {
        assert!(matches!(
            map(404, "{}").kind(),
            ProviderErrorKind::ModelNotFound
        ));
    }

    #[test]
    fn a_daemon_that_is_not_running_says_so_in_the_code() {
        // The code is a sentence because the answer almost always is
        // "`ollama serve` is not running", not "check your network".
        assert!(DAEMON_UNREACHABLE_CODE.contains("daemon_running"));
        assert!(DAEMON_UNREACHABLE_CODE.len() <= turnframe_provider::error::MAX_ERROR_CODE_LEN);
        let sanitized = ErrorCode::new(DAEMON_UNREACHABLE_CODE);
        // It survives sanitization unchanged, so the message reaches the log.
        assert_eq!(sanitized.as_str(), DAEMON_UNREACHABLE_CODE);
    }

    #[test]
    fn a_context_problem_is_not_a_bad_request() {
        let error = map(
            400,
            "{\"error\":\"the request exceeds the available context size: 9001 tokens > 4096 \
             maximum. try increasing the context size or enable context shift\"}",
        );
        assert_eq!(
            *error.kind(),
            ProviderErrorKind::ContextOverflow {
                needed_tokens: Some(9001),
                limit_tokens: Some(4096)
            }
        );
        // Fatal on purpose: the remedy is a shorter prompt or a larger
        // `num_ctx`, not another attempt.
        assert_eq!(error.retry_class(), RetryClass::Fatal);
        let rendered = error.to_string();
        assert!(rendered.contains("needed=9001"), "{rendered}");
        assert!(!rendered.contains("context shift"), "{rendered}");
    }

    #[test]
    fn a_context_message_without_numbers_still_lands_in_the_right_family() {
        let error = map(400, "{\"error\":\"input is too large for this model\"}");
        assert_eq!(
            *error.kind(),
            ProviderErrorKind::ContextOverflow {
                needed_tokens: None,
                limit_tokens: None
            }
        );
    }

    #[test]
    fn a_request_the_daemon_rejects_is_an_invalid_request() {
        // A genuine Ollama 400 that no reasonable adapter could read as a
        // context problem.
        let error = map(
            400,
            "{\"error\":\"json: cannot unmarshal string into Go struct field \
             ChatRequest.messages of type []api.Message\"}",
        );
        assert!(matches!(error.kind(), ProviderErrorKind::InvalidRequest));
        assert_eq!(error.retry_class(), RetryClass::Fatal);
    }

    #[test]
    fn statuses_a_proxy_adds_map_to_their_families() {
        assert!(matches!(
            map(401, "{\"error\":\"unauthorized\"}").kind(),
            ProviderErrorKind::Authentication
        ));
        assert!(matches!(
            map(403, "{\"error\":\"forbidden\"}").kind(),
            ProviderErrorKind::Authorization
        ));
        assert!(matches!(map(408, "{}").kind(), ProviderErrorKind::Timeout));
        assert!(matches!(
            map(500, "{\"error\":\"llama runner process has terminated\"}").kind(),
            ProviderErrorKind::Server { status: Some(500) }
        ));
        assert!(matches!(
            map(503, "{}").kind(),
            ProviderErrorKind::Server { status: Some(503) }
        ));
        assert_eq!(map(418, "{}").retry_class(), RetryClass::Fatal);
    }

    #[test]
    fn an_expired_token_is_not_a_wrong_one() {
        let error = map(
            401,
            "{\"error\":{\"message\":\"the bearer token has expired\",\"type\":\"token_expired\"}}",
        );
        assert!(matches!(error.kind(), ProviderErrorKind::CredentialExpired));
        // Fallback, never Retry: the same token would fail again, and a caller
        // holding a refresher can refresh and come back to this profile.
        assert_eq!(error.retry_class(), RetryClass::Fallback);
    }

    #[test]
    fn a_spent_balance_on_a_429_is_not_a_rate_limit() {
        let error = classify(
            429,
            Some(Duration::from_secs(3)),
            &ApiErrorEnvelope::decode(
                "{\"error\":\"your credit balance is too low to run this model\"}",
            ),
            &redactor(),
        );
        assert_eq!(
            *error.kind(),
            ProviderErrorKind::QuotaExhausted {
                scope: Some(ErrorCode::new(QUOTA_SCOPE_CREDIT_BALANCE))
            }
        );
        // Waiting refills nothing, so the delay is deliberately not carried.
        assert_eq!(error.retry_class(), RetryClass::Fallback);
        assert_eq!(error.retry_after(), None);
    }

    #[test]
    fn a_rate_limit_keeps_the_delay_the_proxy_asked_for() {
        let error = classify(
            429,
            Some(Duration::from_secs(3)),
            &ApiErrorEnvelope::decode("{\"error\":\"too many requests\"}"),
            &redactor(),
        );
        assert_eq!(error.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(error.retry_class(), RetryClass::RetryAfter);
    }

    #[test]
    fn a_policy_gateway_surfaces_as_a_content_filter() {
        let error = map(
            403,
            "{\"error\":\"the prompt was blocked by policy before it reached the model\"}",
        );
        assert!(matches!(error.kind(), ProviderErrorKind::ContentFilter));
        assert_eq!(error.retry_class(), RetryClass::Fatal);
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
    fn a_credential_echoed_into_an_error_code_does_not_survive() {
        let body = "{\"error\":{\"message\":\"nope\",\"code\":\"sk-planted-0123456789abcdef\"}}";
        let error = map(401, body);
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("sk-planted"), "{rendered}");
        assert!(rendered.contains("REDACTED"), "{rendered}");
    }

    #[test]
    fn a_mid_stream_error_object_classifies_like_its_http_twin() {
        let reported = serde_json::json!("model \"x\" not found, try pulling it first");
        let error = classify_stream(&reported, &redactor());
        assert!(matches!(error.kind(), ProviderErrorKind::ModelNotFound));

        let unknown = serde_json::json!("the runner gave up");
        let error = classify_stream(&unknown, &redactor());
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::Server { status: Some(500) }
        ));
        assert_eq!(error.retry_class(), RetryClass::Retry);
    }

    #[test]
    fn retry_after_reads_both_headers_and_ignores_a_date() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert("retry-after", "3".parse().expect("header"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(3)));
        headers.insert("retry-after", "1.5".parse().expect("header"));
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(1500)));
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().expect("header"),
        );
        assert_eq!(retry_after(&headers), None);
        headers.insert("retry-after-ms", "250".parse().expect("header"));
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(250)));
    }
}
