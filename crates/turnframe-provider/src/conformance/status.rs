//! Per-status error mapping rows (spec §20.8, §24).
//!
//! The seventeen rows of [`Check::ALL`] prove that whatever error an adapter
//! *did* produce carries the right [`RetryClass`](crate::error::RetryClass).
//! They do not prove it produced the right error: an adapter mapping every
//! failure onto [`Transport`](crate::error::ProviderErrorKind::Transport) is
//! internally consistent and useless. These rows close that hole by putting a
//! specific wire failure in front of the adapter and demanding a specific kind
//! back — and the class it carries, because the kind is only half the contract.
//!
//! | Row | On the wire | Required kind |
//! |-----|-------------|---------------|
//! | [`Unauthorized`](StatusRow::Unauthorized) | HTTP 401 | `authentication` |
//! | [`Forbidden`](StatusRow::Forbidden) | HTTP 403 | `authorization` |
//! | [`NotFound`](StatusRow::NotFound) | HTTP 404 | `model_not_found` |
//! | [`RequestTimeout`](StatusRow::RequestTimeout) | HTTP 408 | `timeout` |
//! | [`ConnectionReset`](StatusRow::ConnectionReset) | the socket dies mid-call | `timeout` or `transport` |
//! | [`TooManyRequests`](StatusRow::TooManyRequests) | HTTP 429 with `Retry-After` | `rate_limited`, delay intact |
//! | [`ContextLength`](StatusRow::ContextLength) | HTTP 400 the vendor describes as a context-length problem | `context_overflow` |
//! | [`BadRequest`](StatusRow::BadRequest) | HTTP 400 that is genuinely a bad request | `invalid_request` |
//! | [`InternalServerError`](StatusRow::InternalServerError) | HTTP 500 | `server` |
//! | [`ServiceUnavailable`](StatusRow::ServiceUnavailable) | HTTP 503 | `server` |
//! | [`ContentFilter`](StatusRow::ContentFilter) | the vendor's safety-filter shape | `content_filter` or `refusal` |
//! | [`ExpiredCredential`](StatusRow::ExpiredCredential) | the vendor's expired-credential signal | `credential_expired` |
//! | [`QuotaExhausted`](StatusRow::QuotaExhausted) | the vendor's quota or billing signal | `quota_exhausted` |
//!
//! The last two rows are the ones a status code cannot decide: an expired token
//! usually arrives as 401 and a spent quota often as 429, so branching on the
//! status alone reports a permanently bad key for something a refresh would fix,
//! and sleeps on a `Retry-After` for a balance nobody has paid. The signal is in
//! the body, and only the adapter knows its vendor's shape.
//!
//! Two rows accept more than one answer on purpose. A **connection reset** is
//! legitimately either kind — the adapter cannot tell a peer that vanished from
//! a deadline that passed — and both carry the same retry class. A **content
//! filter** may arrive as a failure or as a response that finished as one; what
//! is refused is calling it a server error.
//!
//! An endpoint that genuinely cannot produce a status skips that row, but only
//! when the fixture says so in words: the default is mounted, a skip without a
//! reason is itself a failure, and a skipped row is reported as **unproven** so
//! a published table cannot claim more than the run demonstrated. The same
//! vocabulary covers the feature rows a deployment can lack, and
//! [`Check::is_declarable`] says which cannot be declared away.

use std::fmt;
use std::time::Duration;

use super::checks::{Context, Outcome, stage};
use super::payloads;
use super::report::Check;
use super::{ProviderFactory, Scenario, WireFixtures};
use crate::error::{ProviderError, RetryClass};
use crate::provider::ModelProvider;
use crate::request::ModelRequest;
use crate::response::{FinishReason, ModelResponse};
use crate::secret::ApiKey;

/// How long the connection-reset row waits before calling the adapter stuck.
///
/// A reset resolves in microseconds; this only exists so an adapter that
/// silently retries a dead socket fails the row instead of hanging the suite.
const RESET_DEADLINE: Duration = Duration::from_secs(10);

/// How long the reset socket waits for the request before closing anyway.
const ACCEPT_WAIT: Duration = Duration::from_secs(1);

/// One wire failure and the error kind an adapter must map it onto.
///
/// Growable: match with a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum StatusRow {
    /// HTTP 401 must become
    /// [`Authentication`](crate::error::ProviderErrorKind::Authentication) —
    /// not [`Authorization`](crate::error::ProviderErrorKind::Authorization).
    /// The key is wrong, and no other candidate configured with the same key
    /// will do better.
    Unauthorized,
    /// HTTP 403 must become
    /// [`Authorization`](crate::error::ProviderErrorKind::Authorization). The
    /// key is fine and this model is not entitled to it, which is a different
    /// thing to tell an operator.
    Forbidden,
    /// HTTP 404 must become
    /// [`ModelNotFound`](crate::error::ProviderErrorKind::ModelNotFound), so
    /// the router moves to a candidate that exists instead of retrying a name
    /// that does not.
    NotFound,
    /// HTTP 408 must become
    /// [`Timeout`](crate::error::ProviderErrorKind::Timeout). The provider is
    /// saying the deadline passed on its side; it means the same as the
    /// deadline passing on ours.
    RequestTimeout,
    /// A socket that dies mid-call must become
    /// [`Timeout`](crate::error::ProviderErrorKind::Timeout) or
    /// [`Transport`](crate::error::ProviderErrorKind::Transport) — never a
    /// server error, and never something fatal.
    ConnectionReset,
    /// HTTP 429 must become
    /// [`RateLimited`](crate::error::ProviderErrorKind::RateLimited), carrying
    /// whatever delay the response advertised.
    TooManyRequests,
    /// An HTTP 400 the provider describes as a context-length problem must
    /// become [`ContextOverflow`](crate::error::ProviderErrorKind::ContextOverflow),
    /// so the runtime shrinks the prompt instead of re-sending it.
    ContextLength,
    /// An HTTP 400 that is genuinely a bad request must become
    /// [`InvalidRequest`](crate::error::ProviderErrorKind::InvalidRequest):
    /// our request is wrong and every other provider will reject it too.
    BadRequest,
    /// HTTP 500 must become
    /// [`Server`](crate::error::ProviderErrorKind::Server).
    InternalServerError,
    /// HTTP 503 must become
    /// [`Server`](crate::error::ProviderErrorKind::Server) as well: a busy
    /// provider is retryable, not fatal.
    ServiceUnavailable,
    /// A safety filter must surface as
    /// [`ContentFilter`](crate::error::ProviderErrorKind::ContentFilter) or
    /// [`Refusal`](crate::error::ProviderErrorKind::Refusal) — as a kind, or
    /// as a response finishing that way — never as a server error a policy
    /// layer would retry.
    ContentFilter,
    /// The vendor's **expired credential** signal must become
    /// [`CredentialExpired`](crate::error::ProviderErrorKind::CredentialExpired),
    /// not [`Authentication`](crate::error::ProviderErrorKind::Authentication).
    ///
    /// Most vendors report it with the same 401 they use for a wrong key, so
    /// only the body tells them apart and only the adapter knows the shape.
    /// Getting it wrong strands a caller that holds a refresher: it is told the
    /// key is bad when refreshing would have fixed the call.
    ExpiredCredential,
    /// The vendor's **quota or billing** signal must become
    /// [`QuotaExhausted`](crate::error::ProviderErrorKind::QuotaExhausted), not
    /// [`RateLimited`](crate::error::ProviderErrorKind::RateLimited).
    ///
    /// Several vendors report an exhausted quota or an empty credit balance
    /// with HTTP 429, the same status as a rate limit. An adapter that reads
    /// the status alone makes the runtime sleep on a delay that will not help,
    /// burning the turn's deadline for a balance only a human can refill.
    QuotaExhausted,
}

impl StatusRow {
    /// Every per-status row, in the order a run reports them.
    pub const ALL: [Self; 13] = [
        Self::Unauthorized,
        Self::Forbidden,
        Self::NotFound,
        Self::RequestTimeout,
        Self::ConnectionReset,
        Self::TooManyRequests,
        Self::ContextLength,
        Self::BadRequest,
        Self::InternalServerError,
        Self::ServiceUnavailable,
        Self::ContentFilter,
        Self::ExpiredCredential,
        Self::QuotaExhausted,
    ];

    /// Stable snake-case label, unique across every [`Check`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "status_401_authentication",
            Self::Forbidden => "status_403_authorization",
            Self::NotFound => "status_404_model_not_found",
            Self::RequestTimeout => "status_408_timeout",
            Self::ConnectionReset => "connection_reset_timeout",
            Self::TooManyRequests => "status_429_rate_limited",
            Self::ContextLength => "status_400_context_overflow",
            Self::BadRequest => "status_400_invalid_request",
            Self::InternalServerError => "status_500_server",
            Self::ServiceUnavailable => "status_503_server",
            Self::ContentFilter => "content_filter_kind",
            Self::ExpiredCredential => "expired_credential_kind",
            Self::QuotaExhausted => "quota_exhausted_kind",
        }
    }

    /// The HTTP status this row puts on the wire, when it is one.
    ///
    /// [`ConnectionReset`](Self::ConnectionReset) has none — the point is that
    /// no status ever arrives — and [`ContentFilter`](Self::ContentFilter)
    /// leaves the framing to the vendor.
    #[must_use]
    pub const fn http_status(self) -> Option<u16> {
        match self {
            Self::Unauthorized => Some(401),
            Self::Forbidden => Some(403),
            Self::NotFound => Some(404),
            Self::RequestTimeout => Some(408),
            Self::TooManyRequests => Some(429),
            Self::ContextLength | Self::BadRequest => Some(400),
            Self::InternalServerError => Some(500),
            Self::ServiceUnavailable => Some(503),
            // These three are decided by the body, not the status line: the
            // vendor may frame them on any status it likes.
            Self::ConnectionReset
            | Self::ContentFilter
            | Self::ExpiredCredential
            | Self::QuotaExhausted => None,
        }
    }

    /// The scenario a fixture mounts for this row.
    ///
    /// [`ConnectionReset`](Self::ConnectionReset) returns `None`: the suite
    /// owns that one, because a reset is produced by a socket rather than by a
    /// mock response.
    #[must_use]
    pub const fn scenario(self) -> Option<Scenario> {
        match self {
            Self::Unauthorized => Some(Scenario::Authentication),
            Self::Forbidden => Some(Scenario::Authorization),
            Self::NotFound => Some(Scenario::ModelNotFound),
            Self::RequestTimeout => Some(Scenario::RequestTimeout),
            Self::TooManyRequests => Some(Scenario::RateLimited),
            Self::ContextLength => Some(Scenario::ContextOverflow),
            Self::BadRequest => Some(Scenario::InvalidRequest),
            Self::InternalServerError => Some(Scenario::ServerError),
            Self::ServiceUnavailable => Some(Scenario::ServiceUnavailable),
            Self::ContentFilter => Some(Scenario::ContentFilter),
            Self::ExpiredCredential => Some(Scenario::ExpiredCredential),
            Self::QuotaExhausted => Some(Scenario::QuotaExhausted),
            Self::ConnectionReset => None,
        }
    }

    /// The [`ProviderErrorKind`](crate::error::ProviderErrorKind) labels this
    /// row accepts.
    ///
    /// One entry for every row but the two documented at the module level.
    #[must_use]
    pub const fn expected_kinds(self) -> &'static [&'static str] {
        match self {
            Self::Unauthorized => &["authentication"],
            Self::Forbidden => &["authorization"],
            Self::NotFound => &["model_not_found"],
            Self::RequestTimeout => &["timeout"],
            Self::ConnectionReset => &["timeout", "transport"],
            Self::TooManyRequests => &["rate_limited"],
            Self::ContextLength => &["context_overflow"],
            Self::BadRequest => &["invalid_request"],
            Self::InternalServerError | Self::ServiceUnavailable => &["server"],
            Self::ContentFilter => &["content_filter", "refusal"],
            Self::ExpiredCredential => &["credential_expired"],
            Self::QuotaExhausted => &["quota_exhausted"],
        }
    }

    /// The [`RetryClass`] the kind this row requires must carry.
    ///
    /// Every row has exactly one, including the two that accept two kinds:
    /// `timeout` and `transport` are both [`Retry`](RetryClass::Retry), and
    /// `content_filter` and `refusal` are both [`Fatal`](RetryClass::Fatal).
    /// The class is asserted separately from the kind because they can drift
    /// apart, and it is the class the policy layer actually acts on.
    #[must_use]
    pub const fn expected_retry_class(self) -> RetryClass {
        match self {
            Self::RequestTimeout | Self::ConnectionReset => RetryClass::Retry,
            Self::InternalServerError | Self::ServiceUnavailable => RetryClass::Retry,
            Self::TooManyRequests => RetryClass::RetryAfter,
            Self::Unauthorized
            | Self::Forbidden
            | Self::NotFound
            | Self::ExpiredCredential
            | Self::QuotaExhausted => RetryClass::Fallback,
            Self::ContextLength | Self::BadRequest | Self::ContentFilter => RetryClass::Fatal,
        }
    }

    /// How a failure message names what was put on the wire.
    #[must_use]
    pub const fn wire_description(self) -> &'static str {
        match self {
            Self::Unauthorized => "HTTP 401",
            Self::Forbidden => "HTTP 403",
            Self::NotFound => "HTTP 404",
            Self::RequestTimeout => "HTTP 408",
            Self::ConnectionReset => "a connection reset",
            Self::TooManyRequests => "HTTP 429",
            Self::ContextLength => "HTTP 400 (context length)",
            Self::BadRequest => "HTTP 400 (bad request)",
            Self::InternalServerError => "HTTP 500",
            Self::ServiceUnavailable => "HTTP 503",
            Self::ContentFilter => "the vendor's content-filter shape",
            Self::ExpiredCredential => "the vendor's expired-credential signal",
            Self::QuotaExhausted => "the vendor's quota or billing signal",
        }
    }

    /// Returns `true` when a *successful* response finishing this way is an
    /// acceptable answer for this row.
    ///
    /// Only the content-filter row has one: every other row exists because a
    /// call failed, and a response where a failure was required is a defect.
    #[must_use]
    pub const fn accepts_finish(self, finish: FinishReason) -> bool {
        matches!(self, Self::ContentFilter)
            && matches!(finish, FinishReason::ContentFilter | FinishReason::Refusal)
    }

    /// The corpus request this row sends.
    fn request(self) -> ModelRequest {
        match self {
            // A structured call is the one that overflows a window and the one
            // a safety filter has something to object to.
            Self::ContextLength | Self::ContentFilter => payloads::structured_request(),
            Self::ExpiredCredential | Self::QuotaExhausted => payloads::structured_request(),
            _ => payloads::narration_request(),
        }
    }

    /// `"authentication"`, or `"timeout|transport"`.
    fn expected_label(self) -> String {
        self.expected_kinds().join("|")
    }
}

impl fmt::Display for StatusRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a fixture can put a given conformance row on the wire.
///
/// Returned by
/// [`WireFixtures::status_support`](super::WireFixtures::status_support) for a
/// [`StatusRow`] and by
/// [`WireFixtures::feature_support`](super::WireFixtures::feature_support) for
/// one of the [declarable](Check::is_declarable) feature rows. The default of
/// both is [`Mounted`](Self::Mounted). An adapter never skips a row by staying
/// quiet: it skips one by saying, in words, that the deployment cannot produce
/// it.
///
/// Growable: match with a `_` arm.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RowSupport {
    /// The fixture mounts this row, and the adapter must satisfy it.
    Mounted,
    /// The deployment genuinely cannot produce this row. The row is reported as
    /// skipped — that is, **unproven** — with the reason attached.
    NotProducible {
        /// Why it cannot be produced, in words. An empty reason fails the row:
        /// a compatibility table needs to say why something is unproven, and
        /// "no reason given" is indistinguishable from an adapter dodging a row
        /// it fails.
        reason: String,
    },
}

impl RowSupport {
    /// Declares that this deployment cannot produce the row, and why.
    ///
    /// ```
    /// use turnframe_provider::conformance::RowSupport;
    ///
    /// let support = RowSupport::not_producible("the endpoint answers 400, never 408");
    /// assert!(matches!(support, RowSupport::NotProducible { .. }));
    /// ```
    #[must_use]
    pub fn not_producible(reason: impl Into<String>) -> Self {
        Self::NotProducible {
            reason: reason.into(),
        }
    }

    /// The reason a row was declared unproducible, trimmed, when there is one.
    ///
    /// An empty or blank reason reads as no reason at all, which is what makes
    /// a reason-less declaration fail its row rather than skip it.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Mounted => None,
            Self::NotProducible { reason } => Some(reason.trim()),
        }
    }
}

/// The name [`RowSupport`] had when only per-status rows could be declared.
///
/// Kept because every adapter names it in its
/// [`status_support`](super::WireFixtures::status_support) implementation, and
/// because "status support" is still what that method is about.
pub type StatusSupport = RowSupport;

/// Runs one per-status row.
pub(super) async fn status_mapping<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    row: StatusRow,
    context: &mut Context,
) -> Outcome {
    if let Some(outcome) = super::declared(
        &fixtures.status_support(row),
        row.wire_description(),
        &format!("so {} is unproven", row.expected_label()),
    ) {
        return outcome;
    }

    if row == StatusRow::ConnectionReset {
        return connection_reset(factory, context).await;
    }
    let Some(scenario) = row.scenario() else {
        return Outcome::Failed(format!(
            "{} has no fixture scenario, which is a defect in the suite",
            row.wire_description()
        ));
    };
    let (_server, provider) = match stage(factory, fixtures, scenario).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::Failed(reason),
    };
    let result = provider.generate(row.request()).await;
    judge(row, result, context)
}

/// Points the adapter at a socket that accepts and immediately resets.
///
/// The suite owns this row rather than the fixture: a reset is a property of
/// the connection, not of a response body, so no mock can express it and every
/// adapter would have to invent the same listener.
async fn connection_reset<F: ProviderFactory>(factory: &F, context: &mut Context) -> Outcome {
    let row = StatusRow::ConnectionReset;
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(error) => {
            return Outcome::Failed(format!(
                "the suite could not bind a local socket for {row}: {error}"
            ));
        }
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            return Outcome::Failed(format!("the suite's local socket has no address: {error}"));
        }
    };
    let closer = tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            // Wait until the request has landed, then close without reading a
            // byte of it. Closing a socket whose receive queue is not empty
            // makes the kernel answer with a reset rather than a graceful
            // shutdown, which is the failure this row is about. The timeout
            // covers an adapter that connects without sending anything: the
            // close then arrives as an end of stream, which is still a
            // transport failure and still not a status.
            //
            // `peek` rather than a readiness probe: readiness can be reported
            // spuriously, and it is the presence of *unread bytes* — not the
            // readiness of the socket — that makes the close a reset. Peeking
            // leaves them in the receive queue, which a read would drain.
            let mut scratch = [0_u8; 1];
            let _ = tokio::time::timeout(ACCEPT_WAIT, stream.peek(&mut scratch)).await;
            drop(stream);
        }
    });

    let outcome = match factory.build(
        &format!("http://{address}"),
        ApiKey::new(payloads::DUMMY_API_KEY),
    ) {
        Err(error) => Outcome::Failed(format!("adapter could not be built: {error}")),
        Ok(provider) => {
            match tokio::time::timeout(RESET_DEADLINE, provider.generate(row.request())).await {
                Err(_elapsed) => Outcome::Failed(format!(
                    "{} left the call unresolved after {}s, expected {}",
                    row.wire_description(),
                    RESET_DEADLINE.as_secs(),
                    row.expected_label()
                )),
                Ok(result) => judge(row, result, context),
            }
        }
    };
    closer.abort();
    outcome
}

/// Names the confusion behind a wrong mapping, where there is a known one.
///
/// A message that only says "expected A, got B" leaves the adapter author to
/// rediscover *why* the suite cares. These three are the mistakes the rows were
/// added for, and each one has a consequence worth spelling out.
fn confusion_hint(row: StatusRow, observed: &str) -> &'static str {
    match (row, observed) {
        (StatusRow::ExpiredCredential, "authentication") => {
            "; an expired credential is not a rejected one — a caller holding a \
             refresher could have refreshed and continued, and this tells it the key is bad"
        }
        (StatusRow::QuotaExhausted, "rate_limited") => {
            "; a spent quota is not a rate limit — waiting will not refill it, so this \
             makes the runtime sleep through its deadline for nothing, whatever status \
             the provider used"
        }
        (StatusRow::ContextLength, "invalid_request") => {
            "; a context overflow is fixed by shrinking the prompt, and a generic bad \
             request is not fixed at all"
        }
        _ => "",
    }
}

/// Turns what the adapter produced into a verdict for `row`.
fn judge(
    row: StatusRow,
    result: Result<ModelResponse, ProviderError>,
    context: &mut Context,
) -> Outcome {
    match result {
        Ok(response) => {
            if row.accepts_finish(response.finish) {
                Outcome::Passed
            } else {
                Outcome::Failed(format!(
                    "{} produced a response finishing as {}, expected {}",
                    row.wire_description(),
                    response.finish,
                    row.expected_label()
                ))
            }
        }
        Err(error) => {
            let kind = error.kind().clone();
            context
                .observed
                .push((Check::StatusMapping(row), kind.clone()));
            if !row.expected_kinds().contains(&kind.as_str()) {
                let hint = confusion_hint(row, kind.as_str());
                return Outcome::Failed(format!(
                    "{} mapped to {kind}, expected {}{hint}",
                    row.wire_description(),
                    row.expected_label()
                ));
            }
            // The kind is only half the contract: the policy layer acts on the
            // class, and a family whose class had drifted would send it the
            // wrong way while every kind assertion still passed.
            let class = kind.retry_class();
            if class != row.expected_retry_class() {
                return Outcome::Failed(format!(
                    "{} mapped to {kind} classified as {class}, expected {} classified as {}",
                    row.wire_description(),
                    row.expected_label(),
                    row.expected_retry_class()
                ));
            }
            if row == StatusRow::TooManyRequests
                && kind.retry_after() != Some(Duration::from_secs(payloads::RETRY_AFTER_SECONDS))
            {
                return Outcome::Failed(format!(
                    "{} mapped to {kind}, expected rate_limited(retry_after={}s): \
                     the response advertised the delay and it must survive the mapping",
                    row.wire_description(),
                    payloads::RETRY_AFTER_SECONDS
                ));
            }
            Outcome::Passed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::ProviderCapabilities;
    use crate::error::{ProviderErrorKind, RetryClass};
    use crate::ids::RequestId;

    fn context() -> Context {
        Context::new(ProviderCapabilities::minimal())
    }

    #[test]
    fn every_row_has_a_unique_label_and_an_expectation() {
        let mut labels: Vec<&str> = StatusRow::ALL.iter().map(|row| row.as_str()).collect();
        assert_eq!(labels.len(), 13);
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 13, "labels must be unique");
        for row in StatusRow::ALL {
            assert!(!row.expected_kinds().is_empty(), "{row} expects nothing");
            assert!(!row.wire_description().is_empty());
            assert_eq!(row.scenario().is_none(), row == StatusRow::ConnectionReset);
        }
        assert_eq!(StatusRow::Forbidden.to_string(), "status_403_authorization");
    }

    #[test]
    fn the_two_four_hundreds_are_told_apart_by_expectation_not_by_status() {
        assert_eq!(StatusRow::ContextLength.http_status(), Some(400));
        assert_eq!(StatusRow::BadRequest.http_status(), Some(400));
        assert_ne!(
            StatusRow::ContextLength.expected_kinds(),
            StatusRow::BadRequest.expected_kinds()
        );
        assert_eq!(StatusRow::ConnectionReset.http_status(), None);
    }

    #[test]
    fn every_expected_kind_keeps_the_retry_class_the_row_needs() {
        // The rows exist so a policy layer can act; a row whose expected kind
        // was fatal where it should be retryable would be worse than no row.
        let cases = [
            (StatusRow::Unauthorized, RetryClass::Fallback),
            (StatusRow::Forbidden, RetryClass::Fallback),
            (StatusRow::NotFound, RetryClass::Fallback),
            (StatusRow::RequestTimeout, RetryClass::Retry),
            (StatusRow::TooManyRequests, RetryClass::RetryAfter),
            (StatusRow::InternalServerError, RetryClass::Retry),
            (StatusRow::ContextLength, RetryClass::Fatal),
            (StatusRow::BadRequest, RetryClass::Fatal),
        ];
        for (row, expected) in cases {
            let kind = match row.expected_kinds()[0] {
                "authentication" => ProviderErrorKind::Authentication,
                "authorization" => ProviderErrorKind::Authorization,
                "model_not_found" => ProviderErrorKind::ModelNotFound,
                "timeout" => ProviderErrorKind::Timeout,
                "rate_limited" => ProviderErrorKind::RateLimited { retry_after: None },
                "server" => ProviderErrorKind::Server { status: None },
                "context_overflow" => ProviderErrorKind::ContextOverflow {
                    needed_tokens: None,
                    limit_tokens: None,
                },
                "invalid_request" => ProviderErrorKind::InvalidRequest,
                other => panic!("unmapped expectation {other}"),
            };
            assert_eq!(kind.retry_class(), expected, "{row}");
        }
    }

    #[test]
    fn a_wrong_mapping_names_the_status_the_expectation_and_what_arrived() {
        let mut context = context();
        let outcome = judge(
            StatusRow::Forbidden,
            Err(ProviderError::authentication()),
            &mut context,
        );
        let Outcome::Failed(detail) = outcome else {
            panic!("a 403 mapped to authentication must fail");
        };
        assert!(detail.contains("HTTP 403"), "{detail}");
        assert!(detail.contains("expected authorization"), "{detail}");
        assert!(detail.contains("mapped to authentication"), "{detail}");
        assert_eq!(context.observed.len(), 1);
        assert_eq!(
            context.observed[0].0,
            Check::StatusMapping(StatusRow::Forbidden)
        );
    }

    #[test]
    fn a_dropped_retry_after_fails_the_rate_limit_row() {
        let mut context = context();
        let kept = judge(
            StatusRow::TooManyRequests,
            Err(ProviderError::rate_limited(Some(Duration::from_secs(
                payloads::RETRY_AFTER_SECONDS,
            )))),
            &mut context,
        );
        assert!(matches!(kept, Outcome::Passed));

        let dropped = judge(
            StatusRow::TooManyRequests,
            Err(ProviderError::rate_limited(None)),
            &mut context,
        );
        let Outcome::Failed(detail) = dropped else {
            panic!("a dropped Retry-After must fail");
        };
        assert!(detail.contains("HTTP 429"), "{detail}");
        assert!(detail.contains("retry_after=3s"), "{detail}");
    }

    #[test]
    fn a_response_where_a_failure_was_required_fails_the_row() {
        let mut context = context();
        let response = ModelResponse::new(RequestId::nil(), "p", "m").with_text("hello");
        let outcome = judge(StatusRow::InternalServerError, Ok(response), &mut context);
        let Outcome::Failed(detail) = outcome else {
            panic!("a 500 that produced an answer must fail");
        };
        assert!(detail.contains("HTTP 500"), "{detail}");
        assert!(detail.contains("finishing as stop"), "{detail}");
        assert!(context.observed.is_empty(), "no error, nothing to classify");
    }

    #[test]
    fn a_filtered_answer_passes_whether_it_arrives_as_a_kind_or_as_a_finish() {
        let mut context = context();
        let filtered =
            ModelResponse::new(RequestId::nil(), "p", "m").with_finish(FinishReason::ContentFilter);
        assert!(matches!(
            judge(StatusRow::ContentFilter, Ok(filtered), &mut context),
            Outcome::Passed
        ));
        assert!(matches!(
            judge(
                StatusRow::ContentFilter,
                Err(ProviderError::content_filter()),
                &mut context
            ),
            Outcome::Passed
        ));
        let as_server = judge(
            StatusRow::ContentFilter,
            Err(ProviderError::server(Some(500))),
            &mut context,
        );
        let Outcome::Failed(detail) = as_server else {
            panic!("a filter reported as a server error must fail");
        };
        assert!(detail.contains("content_filter|refusal"), "{detail}");
    }

    #[test]
    fn a_reset_accepts_a_timeout_and_a_transport_failure_but_nothing_else() {
        let mut context = context();
        for error in [ProviderError::timeout(), ProviderError::transport("reset")] {
            assert!(matches!(
                judge(StatusRow::ConnectionReset, Err(error), &mut context),
                Outcome::Passed
            ));
        }
        let wrong = judge(
            StatusRow::ConnectionReset,
            Err(ProviderError::other("mystery")),
            &mut context,
        );
        let Outcome::Failed(detail) = wrong else {
            panic!("an unclassified reset must fail");
        };
        assert!(detail.contains("a connection reset"), "{detail}");
        assert!(detail.contains("timeout|transport"), "{detail}");
    }

    #[test]
    fn every_rows_expected_kind_really_carries_the_class_the_row_declares() {
        // The two assertions in `judge` must agree with each other, or a row
        // could be unsatisfiable: a correct adapter would fail the class check
        // after passing the kind check.
        let representative = |label: &str| match label {
            "authentication" => ProviderErrorKind::Authentication,
            "authorization" => ProviderErrorKind::Authorization,
            "model_not_found" => ProviderErrorKind::ModelNotFound,
            "timeout" => ProviderErrorKind::Timeout,
            "transport" => ProviderErrorKind::Transport,
            "rate_limited" => ProviderErrorKind::RateLimited { retry_after: None },
            "server" => ProviderErrorKind::Server { status: None },
            "context_overflow" => ProviderErrorKind::ContextOverflow {
                needed_tokens: None,
                limit_tokens: None,
            },
            "invalid_request" => ProviderErrorKind::InvalidRequest,
            "content_filter" => ProviderErrorKind::ContentFilter,
            "refusal" => ProviderErrorKind::Refusal,
            "credential_expired" => ProviderErrorKind::CredentialExpired,
            "quota_exhausted" => ProviderErrorKind::QuotaExhausted { scope: None },
            other => panic!("unmapped expectation {other}"),
        };
        for row in StatusRow::ALL {
            for label in row.expected_kinds() {
                assert_eq!(
                    representative(label).retry_class(),
                    row.expected_retry_class(),
                    "{row} accepts {label}, whose class disagrees with the row"
                );
            }
        }
    }

    #[test]
    fn an_expired_credential_reported_as_a_bad_key_fails_and_says_why() {
        let mut context = context();
        let outcome = judge(
            StatusRow::ExpiredCredential,
            Err(ProviderError::authentication()),
            &mut context,
        );
        let Outcome::Failed(detail) = outcome else {
            panic!("an expired credential reported as authentication must fail");
        };
        assert!(detail.contains("expired-credential signal"), "{detail}");
        assert!(detail.contains("expected credential_expired"), "{detail}");
        assert!(detail.contains("mapped to authentication"), "{detail}");
        assert!(detail.contains("refresher"), "{detail}");

        assert!(matches!(
            judge(
                StatusRow::ExpiredCredential,
                Err(ProviderError::credential_expired()),
                &mut context
            ),
            Outcome::Passed
        ));
    }

    #[test]
    fn a_spent_quota_reported_as_a_rate_limit_fails_and_says_why() {
        let mut context = context();
        // The 429 case is the one that matters: the status is identical to a
        // real rate limit, so only the body could have told them apart.
        let outcome = judge(
            StatusRow::QuotaExhausted,
            Err(ProviderError::rate_limited(Some(Duration::from_secs(60)))),
            &mut context,
        );
        let Outcome::Failed(detail) = outcome else {
            panic!("a quota reported as a rate limit must fail");
        };
        assert!(detail.contains("quota or billing signal"), "{detail}");
        assert!(detail.contains("expected quota_exhausted"), "{detail}");
        assert!(detail.contains("waiting will not refill it"), "{detail}");

        assert!(matches!(
            judge(
                StatusRow::QuotaExhausted,
                Err(ProviderError::quota_exhausted(Some("credit_balance"))),
                &mut context
            ),
            Outcome::Passed
        ));
    }

    #[test]
    fn a_right_kind_with_a_drifted_class_still_fails() {
        // A synthetic check of the second assertion: if `credential_expired`
        // were ever reclassified as a plain retry, the row must catch it even
        // though the kind is right.
        assert_ne!(
            ProviderErrorKind::CredentialExpired.retry_class(),
            RetryClass::Retry,
            "an expired credential must never be a plain retry"
        );
        assert_eq!(
            StatusRow::ExpiredCredential.expected_retry_class(),
            RetryClass::Fallback
        );
        assert_eq!(
            StatusRow::QuotaExhausted.expected_retry_class(),
            RetryClass::Fallback
        );
        assert_ne!(
            StatusRow::QuotaExhausted.expected_retry_class(),
            StatusRow::TooManyRequests.expected_retry_class(),
            "a quota must not be classified like a rate limit"
        );
    }

    #[test]
    fn not_producible_needs_a_reason() {
        let explained = StatusSupport::not_producible("the endpoint never answers 408");
        assert_eq!(
            explained,
            RowSupport::NotProducible {
                reason: "the endpoint never answers 408".to_owned()
            }
        );
        assert!(format!("{explained:?}").contains("NotProducible"));
        assert_eq!(explained.reason(), Some("the endpoint never answers 408"));
        // Whitespace is not a reason, which is what makes the empty
        // declaration fail its row instead of skipping it.
        assert_eq!(RowSupport::not_producible("  \n ").reason(), Some(""));
        assert_eq!(RowSupport::Mounted.reason(), None);
    }
}
