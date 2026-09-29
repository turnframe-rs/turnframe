//! SDK failure → normalized failure (spec §20.7, §24, §25.2).
//!
//! Two rules shape this module.
//!
//! **Nothing from the wire reaches the error.** The SDK hands over a decoded
//! exception with a code, a message and the raw response. The message is read
//! to classify and — for a context overflow — to lift two integers out of, and
//! is then thrown away. What survives is a
//! [`ProviderErrorKind`](turnframe_provider::error::ProviderErrorKind), a short
//! [`ErrorCode`] taken from the exception's own AWS error code, sanitized and
//! passed through a [`Redactor`] first. No header, no message, no body, ever.
//!
//! **Classification is by meaning, not by status.** The AWS error code leads
//! and the HTTP status only fills in what the code does not say:
//!
//! | AWS error code | Status | Normalized | Retry class |
//! |---|---|---|---|
//! | `ValidationException` | 400 | `InvalidRequest` | fatal |
//! | `ValidationException` naming a prompt that is too long | 400 | `ContextOverflow` | fatal — shrink the prompt |
//! | `UnrecognizedClientException`, `InvalidSignatureException` | 401/403 | `Authentication` | fall back |
//! | `AccessDeniedException` | 403 | `Authorization` | fall back |
//! | `ExpiredTokenException` | 403 | `CredentialExpired` | fall back, after a refresh |
//! | `ResourceNotFoundException` | 404 | `ModelNotFound` | fall back |
//! | `ModelTimeoutException` | 408 | `Timeout` | retry |
//! | `ThrottlingException` | 429 | `RateLimited { retry_after }` | retry after the delay |
//! | `ServiceQuotaExceededException` | 429 | `QuotaExhausted` | fall back |
//! | `ModelNotReadyException` | 429/503 | `Server` | retry |
//! | `InternalServerException`, `ModelErrorException` | 500 | `Server` | retry |
//! | `ServiceUnavailableException` | 503 | `Server` | retry |
//!
//! # Why Bedrock is the reason the expired-credential kind exists
//!
//! Bedrock is signed with SigV4, and in every deployment that is not a
//! long-lived access key — an assumed role, an instance profile, a container
//! task role, an SSO session — the credential is **short-lived by design**. It
//! expires on a schedule, and when it does the endpoint answers with
//! `ExpiredTokenException`: the same 403 family a genuinely wrong key produces,
//! told apart only by the code.
//!
//! Reporting that as [`Authentication`](turnframe_provider::error::ProviderErrorKind::Authentication)
//! would be a lie with a cost. A caller holding a credential provider — which,
//! on Bedrock, is nearly every caller — would be told the key is bad when
//! refreshing would have fixed the call on the next attempt. That is why
//! [`CredentialExpired`](turnframe_provider::error::ProviderErrorKind::CredentialExpired)
//! exists, and this adapter is the one that meets it most often.
//!
//! Two rows behave the same way and for the same reason:
//!
//! | Signal | Normalized | Why it is not the obvious thing |
//! |---|---|---|
//! | `ExpiredTokenException` | `CredentialExpired` | A session token that *was* valid is a refresh problem, not a configuration typo. |
//! | `ServiceQuotaExceededException`, on a **429** | `QuotaExhausted` | Waiting does not raise a quota. Reading it as a rate limit would park the turn behind a delay that changes nothing, so it falls back to another candidate instead. |
//!
//! Both are recognized **before** the status is consulted, which is what stops
//! a quota 429 from being read as a rate limit and an expired session from
//! being read as a bad key.

use std::time::Duration;

use aws_credential_types::provider::error::CredentialsError;
use aws_sdk_bedrockruntime::error::{ProvideErrorMetadata, SdkError};
use turnframe_provider::error::{ErrorCode, ProviderError, ProviderErrorKind};
use turnframe_provider::secret::Redactor;

/// Quota scope reported when an account-level Bedrock quota is spent.
///
/// ```
/// use turnframe_provider_bedrock::QUOTA_SCOPE_SERVICE_QUOTA;
///
/// assert_eq!(QUOTA_SCOPE_SERVICE_QUOTA, "service_quota");
/// ```
pub const QUOTA_SCOPE_SERVICE_QUOTA: &str = "service_quota";

/// Quota scope reported when the message names provisioned model units rather
/// than an account quota.
pub const QUOTA_SCOPE_PROVISIONED_THROUGHPUT: &str = "provisioned_throughput";

/// The status a stream-borne exception is classified as when its code says
/// nothing.
///
/// A mid-stream failure arrives inside a `200`, so there is no status to read.
/// Treating an unrecognized one as a server error makes it retryable, which is
/// the safe reading of "the connection said something we do not model".
const STREAM_FALLBACK_STATUS: u16 = 500;

/// Maps one failed SDK call onto a normalized failure.
///
/// Generic over the operation error so `Converse` and `ConverseStream` are
/// classified by exactly the same rules: both carry AWS error metadata, and
/// nothing here reads an operation-specific field.
pub(crate) fn classify<E>(error: &SdkError<E>, redactor: &dyn Redactor) -> ProviderError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    match error {
        SdkError::ConstructionFailure(_) => credentials_failure(error)
            .unwrap_or_else(|| ProviderError::invalid_request("request_construction")),
        SdkError::TimeoutError(_) => ProviderError::timeout(),
        SdkError::DispatchFailure(failure) => {
            if failure.is_timeout() {
                ProviderError::timeout()
            } else if failure.is_user() {
                credentials_failure(error)
                    .unwrap_or_else(|| ProviderError::invalid_request("request_rejected_locally"))
            } else {
                ProviderError::transport("dispatch_failed")
            }
        }
        // The response never arrived whole: headers without a body, a peer that
        // hung up mid-frame. That is a transport failure, and retryable.
        SdkError::ResponseError(_) => ProviderError::transport("incomplete_response"),
        SdkError::ServiceError(context) => {
            let raw = context.raw();
            let hint = raw.headers().get("retry-after").and_then(parse_retry_after);
            classify_service(context.err(), Some(raw.status().as_u16()), hint, redactor)
        }
        // `SdkError` is growable; an unplaced variant fails closed as a server
        // error, which is retryable rather than fatal.
        _ => ProviderError::server(None),
    }
}

/// Maps a failure inside a `ConverseStream` body.
///
/// The exception arrives as an event-stream frame inside a `200`, so there is
/// no status and no header to read: the AWS error code is all there is, and an
/// unrecognized one is a retryable server error.
pub(crate) fn classify_stream<E, R>(
    error: &SdkError<E, R>,
    redactor: &dyn Redactor,
) -> ProviderError
where
    E: ProvideErrorMetadata,
{
    match error {
        SdkError::TimeoutError(_) => ProviderError::timeout(),
        SdkError::DispatchFailure(_) | SdkError::ResponseError(_) => {
            ProviderError::transport("stream_read_failed")
        }
        SdkError::ServiceError(context) => classify_service(context.err(), None, None, redactor),
        _ => ProviderError::malformed("stream_frame_not_understood"),
    }
}

/// Classifies a decoded service exception.
fn classify_service<E: ProvideErrorMetadata>(
    error: &E,
    status: Option<u16>,
    retry_after_hint: Option<Duration>,
    redactor: &dyn Redactor,
) -> ProviderError {
    let raw_code = error.code().unwrap_or_default().to_owned();
    let code = raw_code.to_ascii_lowercase();
    let message = error.message().unwrap_or_default().to_ascii_lowercase();
    attach_code(
        by_meaning(&code, &message, status, retry_after_hint),
        &raw_code,
        redactor,
    )
}

/// The classification itself, on nothing but a code, a message and a status.
fn by_meaning(
    code: &str,
    message: &str,
    status: Option<u16>,
    retry_after_hint: Option<Duration>,
) -> ProviderError {
    // These two are recognized before the status is consulted, so a quota 429
    // never reaches the rate-limit branch and an expired session never reaches
    // the plain authentication one.
    if let Some(scope) = quota_scope(code, message) {
        return ProviderError::quota_exhausted(Some(scope));
    }
    if is_expired_credential(code, message) {
        return ProviderError::credential_expired();
    }
    if let Some(by_code) = by_error_code(code, message, status, retry_after_hint) {
        return by_code;
    }
    by_status(status, retry_after_hint)
}

/// Classification driven by the AWS error code.
fn by_error_code(
    code: &str,
    message: &str,
    status: Option<u16>,
    retry_after_hint: Option<Duration>,
) -> Option<ProviderError> {
    Some(match code {
        "validationexception" | "serializationexception" => {
            if is_context_overflow(message) {
                let (needed, limit) = context_numbers(message);
                ProviderError::context_overflow(needed, limit)
            } else {
                ProviderError::new(ProviderErrorKind::InvalidRequest)
            }
        }
        "throttlingexception" => ProviderError::rate_limited(retry_after_hint),
        "accessdeniedexception" => ProviderError::authorization(),
        // The credential itself was not accepted. AWS answers 403 for most of
        // these and a fronting gateway answers 401; either way the key is
        // wrong, not merely unentitled.
        "unrecognizedclientexception"
        | "invalidsignatureexception"
        | "incompletesignature"
        | "invalidclienttokenid"
        | "missingauthenticationtoken"
        | "signaturedoesnotmatch"
        | "authfailure" => ProviderError::authentication(),
        "resourcenotfoundexception" => ProviderError::model_not_found(),
        "modeltimeoutexception" => ProviderError::timeout(),
        // The model is warming up or the fleet is busy: our request is fine and
        // the same call will work shortly, which is a server error, not a
        // configuration fault.
        "modelnotreadyexception" | "serviceunavailableexception" => {
            ProviderError::server(Some(server_status(status, 503)))
        }
        "internalserverexception" | "modelerrorexception" | "modelstreamerrorexception" => {
            ProviderError::server(Some(server_status(status, STREAM_FALLBACK_STATUS)))
        }
        _ => return None,
    })
}

/// A server error records the status it arrived with, or the one its code
/// implies when it arrived inside a stream.
const fn server_status(status: Option<u16>, implied: u16) -> u16 {
    match status {
        Some(status) if status >= 400 => status,
        _ => implied,
    }
}

/// Status-only classification, used when the code says nothing recognizable.
fn by_status(status: Option<u16>, retry_after_hint: Option<Duration>) -> ProviderError {
    match status {
        Some(400 | 409 | 422) => ProviderError::new(ProviderErrorKind::InvalidRequest),
        Some(401) => ProviderError::authentication(),
        Some(403) => ProviderError::authorization(),
        Some(404) => ProviderError::model_not_found(),
        Some(408) => ProviderError::timeout(),
        Some(413) => ProviderError::context_overflow(None, None),
        Some(429) => ProviderError::rate_limited(retry_after_hint),
        Some(status @ 500..=599) => ProviderError::server(Some(status)),
        Some(other) => ProviderError::other(format!("http_{other}")),
        None => ProviderError::server(Some(STREAM_FALLBACK_STATUS)),
    }
}

/// Attaches the AWS error code, redacted and sanitized.
fn attach_code(error: ProviderError, code: &str, redactor: &dyn Redactor) -> ProviderError {
    if code.is_empty() {
        return error;
    }
    let masked = redactor.redact(code);
    error.with_code(ErrorCode::new(masked).as_str())
}

/// Names the exhausted quota, when the failure is one.
///
/// Matched **before** the status, because Bedrock answers `429` for a spent
/// quota exactly as it does for a rate limit, and a caller told to wait would
/// wait for nothing.
fn quota_scope(code: &str, message: &str) -> Option<&'static str> {
    if message.contains("provisioned throughput") || message.contains("provisioned model units") {
        return Some(QUOTA_SCOPE_PROVISIONED_THROUGHPUT);
    }
    if code.contains("servicequota")
        || code == "quotaexceededexception"
        || code == "limitexceededexception"
        || message.contains("service quota")
        || message.contains("quota for this account")
    {
        return Some(QUOTA_SCOPE_SERVICE_QUOTA);
    }
    None
}

/// Recognizes a credential the endpoint says used to be valid.
///
/// Only a credential signal qualifies: the word "expired" in a validation
/// message about some other field is not a session problem. A wrong or unknown
/// key stays a plain
/// [`Authentication`](turnframe_provider::error::ProviderErrorKind::Authentication)
/// failure, because the remedy differs — re-read the configuration rather than
/// refresh the session.
fn is_expired_credential(code: &str, message: &str) -> bool {
    if matches!(
        code,
        "expiredtokenexception" | "expiredtoken" | "requestexpired" | "tokenrefreshrequired"
    ) {
        return true;
    }
    let credential_code = matches!(
        code,
        "unrecognizedclientexception" | "invalidclienttokenid" | "accessdeniedexception"
    );
    credential_code
        && (message.contains("security token included in the request is expired")
            || message.contains("token has expired")
            || message.contains("credentials have expired"))
}

/// Recognizes the shapes Bedrock reports a context overflow in.
fn is_context_overflow(message: &str) -> bool {
    message.contains("input is too long")
        || message.contains("prompt is too long")
        || message.contains("too many input tokens")
        || message.contains("maximum context length")
        || message.contains("context window")
        || message.contains("exceeds the context limit")
}

/// Reads the requested size and the limit out of a context-overflow message.
///
/// Integers are not secrets and they are what makes the failure actionable, so
/// they are the one thing lifted out of a message. Only a `needed > limit`
/// comparison is recognized — `215048 tokens > 199999` — and nothing else is
/// guessed at.
///
/// Returns `(needed, limit)`.
fn context_numbers(message: &str) -> (Option<u64>, Option<u64>) {
    let Some((before, after)) = message.split_once('>') else {
        return (None, None);
    };
    (last_number(before), first_number(after))
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
    let mut found = None;
    let mut current = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            current.push(ch);
            continue;
        }
        if let Ok(value) = current.parse::<u64>() {
            found = Some(value);
        }
        current.clear();
    }
    found
}

/// Reads a retry hint out of a `retry-after` header value.
///
/// It is read as (possibly fractional) seconds. Nothing else is turned into a
/// delay: a wrong delay is worse than none.
fn parse_retry_after(value: &str) -> Option<Duration> {
    let raw = value.trim();
    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    raw.parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(Duration::from_secs_f64)
}

/// Recognizes a failure the credential provider raised before the call left.
///
/// The SDK owns credential resolution, so a session that could not be produced
/// arrives as a construction or user failure wrapping a [`CredentialsError`].
/// It is an authentication problem — another candidate with another credential
/// may well work — and never an invalid request, which would be fatal and would
/// strand a caller whose only fault was an unrefreshed session.
fn credentials_failure<E, R>(error: &SdkError<E, R>) -> Option<ProviderError>
where
    E: std::error::Error + 'static,
    R: std::fmt::Debug,
{
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(credentials) = current.downcast_ref::<CredentialsError>() {
            return Some(match credentials {
                CredentialsError::ProviderTimedOut(_) => {
                    ProviderError::timeout().with_code("credentials_timed_out")
                }
                _ => ProviderError::authentication().with_code("credentials_unavailable"),
            });
        }
        source = current.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_provider::error::{ProviderErrorKind, RetryClass};
    use turnframe_provider::secret::DefaultRedactor;

    fn classified(code: &str, message: &str, status: u16) -> ProviderError {
        attach_code(
            by_meaning(
                &code.to_ascii_lowercase(),
                &message.to_ascii_lowercase(),
                Some(status),
                None,
            ),
            code,
            &DefaultRedactor::new(),
        )
    }

    #[test]
    fn an_expired_session_is_not_a_rejected_key() {
        let expired = classified(
            "ExpiredTokenException",
            "The security token included in the request is expired",
            403,
        );
        assert_eq!(expired.kind(), &ProviderErrorKind::CredentialExpired);
        assert_eq!(expired.retry_class(), RetryClass::Fallback);
        assert_eq!(
            expired.code().map(ErrorCode::as_str),
            Some("ExpiredTokenException")
        );

        let wrong = classified(
            "UnrecognizedClientException",
            "The security token included in the request is invalid",
            401,
        );
        assert_eq!(wrong.kind(), &ProviderErrorKind::Authentication);
    }

    #[test]
    fn a_spent_quota_is_not_a_rate_limit_even_on_a_429() {
        let quota = classified(
            "ServiceQuotaExceededException",
            "Your request exceeded the service quota for this account",
            429,
        );
        assert!(matches!(
            quota.kind(),
            ProviderErrorKind::QuotaExhausted { .. }
        ));
        assert_eq!(quota.retry_class(), RetryClass::Fallback);

        let throttled = classified(
            "ThrottlingException",
            "Too many requests, please wait before trying again",
            429,
        );
        assert!(matches!(
            throttled.kind(),
            ProviderErrorKind::RateLimited { .. }
        ));
        assert_eq!(throttled.retry_class(), RetryClass::RetryAfter);
    }

    #[test]
    fn the_two_four_hundreds_are_told_apart_by_the_message() {
        let overflow = classified(
            "ValidationException",
            "Input is too long for requested model: 215048 tokens > 199999 maximum",
            400,
        );
        assert_eq!(
            overflow.kind(),
            &ProviderErrorKind::ContextOverflow {
                needed_tokens: Some(215_048),
                limit_tokens: Some(199_999),
            }
        );

        let bad = classified(
            "ValidationException",
            "1 validation error detected: Value at 'toolConfig.tools' failed to satisfy constraint",
            400,
        );
        assert_eq!(bad.kind(), &ProviderErrorKind::InvalidRequest);
    }

    #[test]
    fn a_model_that_is_not_ready_is_a_server_error_the_caller_may_retry() {
        let not_ready = classified("ModelNotReadyException", "Model is not ready", 429);
        assert_eq!(
            not_ready.kind(),
            &ProviderErrorKind::Server { status: Some(429) }
        );
        assert_eq!(not_ready.retry_class(), RetryClass::Retry);

        let unavailable = classified("ServiceUnavailableException", "busy", 503);
        assert_eq!(
            unavailable.kind(),
            &ProviderErrorKind::Server { status: Some(503) }
        );
    }

    #[test]
    fn the_status_only_fills_in_what_the_code_does_not_say() {
        assert_eq!(
            classified("", "", 401).kind(),
            &ProviderErrorKind::Authentication
        );
        assert_eq!(
            classified("", "", 403).kind(),
            &ProviderErrorKind::Authorization
        );
        assert_eq!(
            classified("", "", 404).kind(),
            &ProviderErrorKind::ModelNotFound
        );
        assert_eq!(classified("", "", 408).kind(), &ProviderErrorKind::Timeout);
        assert_eq!(
            classified("", "", 500).kind(),
            &ProviderErrorKind::Server { status: Some(500) }
        );
    }

    #[test]
    fn a_code_that_looks_like_a_credential_cannot_survive_readable() {
        let error = classified(
            "sk-turnframe-conformance-DUMMY-0000000000000000",
            "nonsense",
            400,
        );
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("sk-turnframe-conformance"));
    }

    #[test]
    fn a_provisioned_throughput_limit_names_its_own_scope() {
        let error = classified(
            "ThrottlingException",
            "Your provisioned throughput has no model units available",
            429,
        );
        assert!(matches!(
            error.kind(),
            ProviderErrorKind::QuotaExhausted { .. }
        ));
    }
}
