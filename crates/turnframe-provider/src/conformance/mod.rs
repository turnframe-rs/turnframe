//! The reusable adapter conformance suite (spec §20.8, §27.5).
//!
//! Every adapter crate runs this suite against its own wiremock fixtures. It
//! exists because a shared wire format does not make two providers behave
//! alike, and because the one thing the whole system trusts — a profile's
//! capability declaration — is the one thing only a test can check.
//!
//! **Conformance is per provider-model pair.** A passing report for
//! `openai/gpt-4o` says nothing about `openai/gpt-4o-mini`, and the report
//! records both keys so nobody can read it as a statement about a brand.
//!
//! # What an adapter supplies
//!
//! The suite owns the corpus: the schema, the requests and the payloads all
//! live in [`payloads`], so two adapters are measured on the same thing. The
//! adapter supplies two small pieces:
//!
//! * a [`ProviderFactory`] that builds it against a base URL with a dummy key;
//! * a [`WireFixtures`] that mounts, for each [`Scenario`], the vendor-shaped
//!   mock that provokes it.
//!
//! Then [`run_all`] drives the twenty feature rows of [`Check::ALL`] — the
//! seventeen of spec §20.8, plus the two that prove a stream is incremental and
//! agrees with the whole path on usage, plus the one that holds token usage to
//! its own contract — and the thirteen [`StatusRow`] rows that prove the
//! adapter maps each wire failure onto the kind it means and the class that
//! kind must carry, and returns a [`ConformanceReport`].
//!
//! ```rust,ignore
//! use turnframe_provider::conformance::{ProviderFactory, Scenario, WireFixtures, run_all};
//!
//! struct Factory;
//!
//! impl ProviderFactory for Factory {
//!     type Provider = MyAdapter;
//!     fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
//!         Ok(MyAdapter::new(base_url, api_key))
//!     }
//! }
//!
//! #[async_trait::async_trait]
//! impl WireFixtures for Fixtures {
//!     async fn mount(&self, server: &MockServer, scenario: Scenario) {
//!         // one vendor-shaped mock per scenario
//!     }
//! }
//!
//! #[tokio::test]
//! async fn adapter_conforms() {
//!     let report = run_all(&Factory, &Fixtures).await;
//!     assert!(report.passed(), "{report}");
//! }
//! ```
//!
//! # Two rules for reading a failure
//!
//! A failing structured-output row means the **declaration** is wrong, not the
//! test. Lower [`ProviderCapabilities::structured_output`](crate::capabilities::ProviderCapabilities::structured_output)
//! to what the profile actually does; that is what
//! [`Check::is_declaration_check`] marks.
//!
//! A [`Skipped`](CheckStatus::Skipped) row is not a pass. It means the fixture
//! said, in words, that this deployment cannot produce the row, so it could not
//! be exercised — the adapter is unproven there, which is the honest state to
//! be in. [`ConformanceReport::compatibility_table`] renders those rows as
//! **unproven**, so a table copied into documentation cannot claim a behaviour
//! the run never demonstrated.
//!
//! A skip is never free. A per-status row is declared through
//! [`WireFixtures::status_support`] and a feature row through
//! [`WireFixtures::feature_support`], both with a reason, and a reason-less
//! declaration fails the row. Declaring `streaming: false` no longer makes the
//! streaming rows disappear either: they fail, and point at the hook. The rows
//! that skip on a capability alone are the two where the capability *is* the
//! statement — [`ToolAndReadRequestIds`](Check::ToolAndReadRequestIds) for a
//! profile with no tool calling, and
//! [`NoSilentCapabilityDowngrade`](Check::NoSilentCapabilityDowngrade) for one
//! whose transport does not enforce a schema.

mod checks;
pub mod payloads;
mod report;
mod status;

use std::fmt;

use async_trait::async_trait;
use wiremock::MockServer;

pub use report::{Check, CheckResult, CheckStatus, ConformanceReport};
pub use status::{RowSupport, StatusRow, StatusSupport};

use crate::error::ProviderError;
use crate::provider::ModelProvider;
use crate::secret::ApiKey;
use checks::Context;

/// One provider behaviour the suite needs a mock for.
///
/// A fixture translates a scenario into vendor framing: the endpoint, the body
/// shape, the status code. What the body *says* comes from [`payloads`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Scenario {
    /// A schema-conformant one-act plan: [`payloads::valid_plan`].
    ValidStructured,
    /// A body that is not JSON: [`payloads::MALFORMED_JSON`].
    MalformedJson,
    /// A two-act plan whose second act carries a forbidden field:
    /// [`payloads::plan_with_unknown_field`].
    UnknownField,
    /// A two-act plan whose second act lacks a required field:
    /// [`payloads::plan_with_missing_field`].
    MissingField,
    /// A well-formed two-act plan: [`payloads::two_act_plan`].
    MultipleActs,
    /// A single tool call named [`payloads::TOOL_NAME`] with id
    /// [`payloads::EXPECTED_CALL_ID`].
    ToolCallIds,
    /// The same answer available both whole and streamed, so the suite can
    /// compare them. Mount both: the fixture distinguishes them by the
    /// streaming flag the adapter puts on the wire.
    ///
    /// Three rows read this scenario, and each asks something of the streamed
    /// half:
    ///
    /// * it must **reassemble** into the whole answer, content and finish
    ///   reason alike;
    /// * it must deliver the prose in **at least two wire events**, or
    ///   [`Check::StreamingIncremental`] cannot tell an adapter that streams
    ///   from one that buffers the body and emits a single delta at the end;
    /// * it must report the **same token usage** as the whole half — which
    ///   means both halves report usage at all, usually on a final frame.
    StreamingReconstruction,
    /// A successful answer whose reported usage carries a cache figure:
    /// [`payloads::USAGE_INPUT_TOKENS`] prompt tokens of which
    /// [`payloads::USAGE_CACHED_TOKENS`] came from the provider's cache, and
    /// [`payloads::USAGE_OUTPUT_TOKENS`] generated.
    ///
    /// Mount the vendor's own cache fields — `prompt_tokens_details`,
    /// `cache_read_input_tokens`, `cacheReadInputTokens`,
    /// `cachedContentTokenCount` — rather than inventing a shape, because the
    /// row is about reading them correctly. The answer itself is prose; only
    /// the counts matter.
    CachedUsage,
    /// A successful status with no usable content.
    EmptyOutput,
    /// The vendor's refusal shape, carrying [`payloads::REFUSAL_TEXT`].
    Refusal,
    /// A successful answer delayed by [`payloads::SLOW_RESPONSE_DELAY`], used
    /// by both the timeout and the cancellation checks.
    SlowResponse,
    /// HTTP 429 with `Retry-After: {}` seconds
    #[doc = concat!("(", stringify!(payloads::RETRY_AFTER_SECONDS), ").")]
    RateLimited,
    /// HTTP 401 with the vendor's authentication error body.
    Authentication,
    /// The vendor's context-length error body.
    ContextOverflow,
    /// A body with [`payloads::DUMMY_API_KEY`] planted inside it, so the
    /// redaction check has something specific to hunt for.
    SecretInBody,
    /// HTTP 403: the credential is valid and not entitled to this model.
    Authorization,
    /// HTTP 404: the configured model does not exist at this provider.
    ModelNotFound,
    /// HTTP 408: the provider gave up waiting on its own side.
    RequestTimeout,
    /// HTTP 400 that is genuinely a bad request — **not** a context-length
    /// problem. Mount a body no reasonable adapter could mistake for one:
    /// these two rows exist to be told apart.
    InvalidRequest,
    /// HTTP 500.
    ServerError,
    /// HTTP 503.
    ServiceUnavailable,
    /// The vendor's safety-filter shape, whether that is a failure body or a
    /// successful answer whose finish reason says the content was filtered.
    ContentFilter,
    /// The vendor's **expired credential** shape, whatever status carries it.
    ///
    /// Mount the body a short-lived token produces once it lapses — usually a
    /// 401 with a code such as `token_expired`, distinguishable from the 401
    /// [`Authentication`](Self::Authentication) mounts only by what the body
    /// says.
    ExpiredCredential,
    /// The vendor's **quota or billing** shape, whatever status carries it.
    ///
    /// Mount the body an exhausted quota or an empty credit balance produces.
    /// If the vendor uses 429 for it — several do — mount the 429 with its real
    /// body: telling it apart from [`RateLimited`](Self::RateLimited) is the
    /// point of the row.
    QuotaExhausted,
}

impl Scenario {
    /// The scenarios behind the feature rows of [`Check::ALL`].
    ///
    /// A full run also mounts [`SecretInBody`](Self::SecretInBody) and every
    /// scenario in [`STATUS`](Self::STATUS).
    pub const ALL: [Self; 14] = [
        Self::ValidStructured,
        Self::MalformedJson,
        Self::UnknownField,
        Self::MissingField,
        Self::MultipleActs,
        Self::ToolCallIds,
        Self::StreamingReconstruction,
        Self::CachedUsage,
        Self::EmptyOutput,
        Self::Refusal,
        Self::SlowResponse,
        Self::RateLimited,
        Self::Authentication,
        Self::ContextOverflow,
    ];

    /// The scenarios behind the per-status rows a fixture must also mount.
    ///
    /// [`Authentication`](Self::Authentication), [`RateLimited`](Self::RateLimited)
    /// and [`ContextOverflow`](Self::ContextOverflow) already cover three of
    /// the thirteen [`StatusRow`]s, and [`StatusRow::ConnectionReset`] needs no
    /// mock at all, so only these nine are new.
    pub const STATUS: [Self; 9] = [
        Self::Authorization,
        Self::ModelNotFound,
        Self::RequestTimeout,
        Self::InvalidRequest,
        Self::ServerError,
        Self::ServiceUnavailable,
        Self::ContentFilter,
        Self::ExpiredCredential,
        Self::QuotaExhausted,
    ];

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ValidStructured => "valid_structured",
            Self::MalformedJson => "malformed_json",
            Self::UnknownField => "unknown_field",
            Self::MissingField => "missing_field",
            Self::MultipleActs => "multiple_acts",
            Self::ToolCallIds => "tool_call_ids",
            Self::StreamingReconstruction => "streaming_reconstruction",
            Self::CachedUsage => "cached_usage",
            Self::EmptyOutput => "empty_output",
            Self::Refusal => "refusal",
            Self::SlowResponse => "slow_response",
            Self::RateLimited => "rate_limited",
            Self::Authentication => "authentication",
            Self::ContextOverflow => "context_overflow",
            Self::SecretInBody => "secret_in_body",
            Self::Authorization => "authorization",
            Self::ModelNotFound => "model_not_found",
            Self::RequestTimeout => "request_timeout",
            Self::InvalidRequest => "invalid_request",
            Self::ServerError => "server_error",
            Self::ServiceUnavailable => "service_unavailable",
            Self::ContentFilter => "content_filter",
            Self::ExpiredCredential => "expired_credential",
            Self::QuotaExhausted => "quota_exhausted",
        }
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The vendor-specific half of the suite.
///
/// One implementation per adapter crate, living in that crate's own tests.
#[async_trait]
pub trait WireFixtures: Send + Sync {
    /// Mounts on `server` the mocks that make the adapter meet `scenario`.
    ///
    /// Called on a **fresh** server for every check, so a fixture may mount
    /// unconditionally without worrying about earlier mounts. A scenario the
    /// vendor cannot produce may be left unmounted: the adapter will then fail
    /// against an empty server, which the check reports as a failure — declare
    /// it unsupported instead, through the capability declaration for a feature
    /// row and through [`status_support`](Self::status_support) for a
    /// per-status row, so the check is skipped rather than failed.
    async fn mount(&self, server: &MockServer, scenario: Scenario);

    /// Whether this endpoint can put `row` on the wire at all.
    ///
    /// The default is [`StatusSupport::Mounted`] for every row, so an adapter
    /// that says nothing is taken to mount everything and fails loudly where
    /// it does not. A row the endpoint genuinely cannot produce — a vendor
    /// with no 403, or one that never answers 408 — is declared with
    /// [`StatusSupport::not_producible`] **and a reason**, which the report
    /// records as an unproven row. A reason-less declaration fails the row: an
    /// unexplained skip and a dodged failure are indistinguishable in a
    /// compatibility table, and only one of them is honest.
    ///
    /// ```rust,ignore
    /// fn status_support(&self, row: StatusRow) -> StatusSupport {
    ///     match row {
    ///         StatusRow::RequestTimeout => {
    ///             StatusSupport::not_producible("the endpoint answers 504, never 408")
    ///         }
    ///         _ => StatusSupport::Mounted,
    ///     }
    /// }
    /// ```
    fn status_support(&self, row: StatusRow) -> RowSupport {
        let _ = row;
        RowSupport::Mounted
    }

    /// Whether this deployment can put a **feature** row on the wire at all.
    ///
    /// The counterpart of [`status_support`](Self::status_support) for the
    /// handful of rows [`Check::is_declarable`] admits: the three streaming
    /// rows, authentication, rate limiting and refusal. Everything else is a
    /// property of the adapter rather than of what surrounds it, and declaring
    /// one of those unproducible fails the row instead of skipping it.
    ///
    /// The default is [`RowSupport::Mounted`] for every row, so an adapter that
    /// says nothing is taken to exercise everything. A daemon started on a
    /// laptop authenticates nobody, meters nothing and filters nothing — and a
    /// profile with no streaming endpoint says *that* here rather than letting
    /// `streaming: false` make three rows vanish from the table.
    ///
    /// ```rust,ignore
    /// fn feature_support(&self, check: Check) -> RowSupport {
    ///     match check {
    ///         Check::AuthenticationFailure => RowSupport::not_producible(
    ///             "`ollama serve` authenticates nothing: every request that reaches \
    ///              /api/chat is served, so no credential is ever rejected",
    ///         ),
    ///         _ => RowSupport::Mounted,
    ///     }
    /// }
    /// ```
    fn feature_support(&self, check: Check) -> RowSupport {
        let _ = check;
        RowSupport::Mounted
    }
}

/// Turns a [`RowSupport`] declaration into the outcome it implies, if any.
///
/// `Mounted` returns `None` and the row runs. A declaration with a reason skips
/// the row as unproven; one without a reason fails it, because an unexplained
/// skip and a dodged failure are the same thing in a report.
fn declared(
    support: &RowSupport,
    wire_description: &str,
    consequence: &str,
) -> Option<checks::Outcome> {
    let reason = support.reason()?;
    if reason.is_empty() {
        return Some(checks::Outcome::Failed(format!(
            "{wire_description} was declared not producible without a reason; \
             name the deployment behaviour, because an unexplained skip and \
             a dodged failure look the same in a report"
        )));
    }
    Some(checks::Outcome::Skipped(format!(
        "{wire_description} is not produced by this deployment ({reason}), {consequence}"
    )))
}

/// Consults [`WireFixtures::feature_support`] for one feature row.
///
/// A row [`Check::is_declarable`] refuses cannot be declared away: the
/// declaration itself becomes the failure, naming the row, so a fixture cannot
/// quietly retire a check that describes the adapter rather than the
/// deployment.
fn feature_declaration<W: WireFixtures>(fixtures: &W, check: Check) -> Option<checks::Outcome> {
    let support = fixtures.feature_support(check);
    if support.reason().is_some() && !check.is_declarable() {
        return Some(checks::Outcome::Failed(format!(
            "{check} was declared not producible, but it is a property of the adapter \
             rather than of the deployment around it and cannot be declared away"
        )));
    }
    declared(
        &support,
        &format!("the behaviour {check} exercises"),
        "so the row is unproven",
    )
}

/// Builds the adapter under test.
///
/// `Provider` must be `Debug` because the secret-redaction check renders the
/// adapter and looks for the configured key in the output.
pub trait ProviderFactory: Send + Sync {
    /// The adapter this factory builds.
    type Provider: ModelProvider + fmt::Debug;

    /// Builds an adapter pointed at `base_url` and configured with `api_key`.
    ///
    /// `base_url` is the mock server's root, with no trailing slash. The key is
    /// [`payloads::DUMMY_API_KEY`] and is never valid anywhere.
    ///
    /// # Errors
    ///
    /// Returns a [`ProviderError`] when the configuration is rejected; the run
    /// then reports every check as failed with that reason.
    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError>;
}

/// Runs the whole suite.
///
/// Checks run in [`Check::ALL`] order, each against its own fresh
/// [`MockServer`]. No check panics and none aborts the run, so a failing
/// adapter produces a complete picture.
pub async fn run_all<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> ConformanceReport {
    // One throwaway build to learn the profile the checks reason about.
    let probe_server = MockServer::start().await;
    let probe = match factory.build(&probe_server.uri(), ApiKey::new(payloads::DUMMY_API_KEY)) {
        Ok(provider) => provider,
        Err(error) => {
            let mut report = ConformanceReport::new(
                crate::ids::ProviderKey::from("unknown"),
                crate::ids::ModelKey::from("unknown"),
            );
            for check in Check::run_order() {
                report.push(checks::build_failure(check, &error));
            }
            return report;
        }
    };
    let mut report = ConformanceReport::new(probe.provider_key(), probe.model_key());
    let mut context = Context::new(probe.capabilities());
    drop(probe);
    drop(probe_server);

    report.push(
        checks::valid_structured_response(factory, fixtures)
            .await
            .into_result(Check::ValidStructuredResponse),
    );
    report.push(
        checks::malformed_json(factory, fixtures)
            .await
            .into_result(Check::MalformedJson),
    );
    report.push(
        checks::unknown_fields(factory, fixtures)
            .await
            .into_result(Check::UnknownFields),
    );
    report.push(
        checks::missing_required_fields(factory, fixtures)
            .await
            .into_result(Check::MissingRequiredFields),
    );
    report.push(
        checks::multiple_acts(factory, fixtures)
            .await
            .into_result(Check::MultipleActs),
    );
    report.push(
        checks::tool_and_read_request_ids(factory, fixtures, &context)
            .await
            .into_result(Check::ToolAndReadRequestIds),
    );
    report.push(
        match feature_declaration(fixtures, Check::StreamingReconstruction) {
            Some(outcome) => outcome,
            None => checks::streaming_reconstruction(factory, fixtures, &context).await,
        }
        .into_result(Check::StreamingReconstruction),
    );
    report.push(
        match feature_declaration(fixtures, Check::StreamingIncremental) {
            Some(outcome) => outcome,
            None => checks::streaming_incremental(factory, fixtures, &context).await,
        }
        .into_result(Check::StreamingIncremental),
    );
    report.push(
        match feature_declaration(fixtures, Check::StreamingUsageAgreement) {
            Some(outcome) => outcome,
            None => checks::streaming_usage_agreement(factory, fixtures, &context).await,
        }
        .into_result(Check::StreamingUsageAgreement),
    );
    report.push(
        checks::token_usage_contract(factory, fixtures, &context)
            .await
            .into_result(Check::TokenUsageContract),
    );
    report.push(
        checks::empty_output(factory, fixtures)
            .await
            .into_result(Check::EmptyOutput),
    );
    report.push(
        match feature_declaration(fixtures, Check::Refusal) {
            Some(outcome) => outcome,
            None => checks::refusal(factory, fixtures, &mut context).await,
        }
        .into_result(Check::Refusal),
    );
    report.push(
        checks::timeout(factory, fixtures, &mut context)
            .await
            .into_result(Check::Timeout),
    );
    report.push(
        match feature_declaration(fixtures, Check::RateLimit) {
            Some(outcome) => outcome,
            None => checks::rate_limit(factory, fixtures, &mut context).await,
        }
        .into_result(Check::RateLimit),
    );
    report.push(
        match feature_declaration(fixtures, Check::AuthenticationFailure) {
            Some(outcome) => outcome,
            None => checks::authentication_failure(factory, fixtures, &mut context).await,
        }
        .into_result(Check::AuthenticationFailure),
    );
    report.push(
        checks::context_overflow(factory, fixtures, &mut context)
            .await
            .into_result(Check::ContextOverflow),
    );
    report.push(
        checks::cancellation(factory, fixtures)
            .await
            .into_result(Check::Cancellation),
    );
    for row in StatusRow::ALL {
        report.push(
            status::status_mapping(factory, fixtures, row, &mut context)
                .await
                .into_result(Check::StatusMapping(row)),
        );
    }
    report.push(checks::retry_classification(&context).into_result(Check::RetryClassification));
    report.push(
        checks::secret_redaction(factory, fixtures)
            .await
            .into_result(Check::SecretRedaction),
    );
    report.push(
        checks::no_silent_capability_downgrade(factory, fixtures, &context)
            .await
            .into_result(Check::NoSilentCapabilityDowngrade),
    );
    refuse_undeclarable_declarations(&mut report, fixtures);
    report
}

/// Turns a declaration on a row that cannot be declared away into a failure.
///
/// [`feature_declaration`] already refuses one on the rows it gates. This sweep
/// covers the rest, so a fixture cannot retire *any* row that describes the
/// adapter rather than the deployment — including one it never had a chance to
/// influence, which would otherwise pass while the fixture believed it had been
/// skipped.
fn refuse_undeclarable_declarations<W: WireFixtures>(report: &mut ConformanceReport, fixtures: &W) {
    for result in &mut report.results {
        let check = result.check;
        if check.is_declarable() || check.status_row().is_some() {
            continue;
        }
        if fixtures.feature_support(check).reason().is_some() {
            *result = CheckResult::failed(
                check,
                format!(
                    "{check} was declared not producible, but it is a property of the \
                     adapter rather than of the deployment around it and cannot be \
                     declared away"
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_labels_are_unique() {
        let mut labels: Vec<&str> = Scenario::ALL.iter().map(|s| s.as_str()).collect();
        labels.push(Scenario::SecretInBody.as_str());
        labels.extend(Scenario::STATUS.iter().map(|s| s.as_str()));
        assert_eq!(labels.len(), 24);
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 24);
        assert_eq!(Scenario::ValidStructured.to_string(), "valid_structured");
    }

    #[test]
    fn a_reason_less_declaration_fails_and_an_explained_one_skips() {
        let unexplained = declared(
            &RowSupport::not_producible(" "),
            "HTTP 503",
            "so it is unproven",
        )
        .expect("a declaration produces an outcome");
        let checks::Outcome::Failed(detail) = unexplained else {
            panic!("a declaration without a reason must fail its row");
        };
        assert!(detail.contains("without a reason"), "{detail}");

        let explained = declared(
            &RowSupport::not_producible("this daemon authenticates nothing"),
            "HTTP 401",
            "so authentication is unproven",
        )
        .expect("a declaration produces an outcome");
        let checks::Outcome::Skipped(reason) = explained else {
            panic!("an explained declaration must skip its row");
        };
        assert!(reason.contains("authenticates nothing"), "{reason}");
        assert!(reason.contains("unproven"), "{reason}");

        assert!(declared(&RowSupport::Mounted, "HTTP 401", "").is_none());
    }

    #[test]
    fn a_declaration_on_a_row_that_describes_the_adapter_is_refused() {
        struct DodgesTheSchemaRow;

        #[async_trait]
        impl WireFixtures for DodgesTheSchemaRow {
            async fn mount(&self, _server: &MockServer, _scenario: Scenario) {}

            fn feature_support(&self, check: Check) -> RowSupport {
                match check {
                    Check::MalformedJson => {
                        RowSupport::not_producible("our vendor never sends bad JSON")
                    }
                    _ => RowSupport::Mounted,
                }
            }
        }

        let outcome = feature_declaration(&DodgesTheSchemaRow, Check::MalformedJson)
            .expect("the declaration is answered");
        let checks::Outcome::Failed(detail) = outcome else {
            panic!("a row that describes the adapter cannot be declared away");
        };
        assert!(detail.contains("malformed_json"), "{detail}");
        assert!(detail.contains("cannot be declared away"), "{detail}");
    }

    #[test]
    fn every_status_row_names_a_scenario_a_fixture_can_mount() {
        for row in StatusRow::ALL {
            let Some(scenario) = row.scenario() else {
                // Only the reset row has no mock: the suite owns the socket.
                assert_eq!(row, StatusRow::ConnectionReset);
                continue;
            };
            assert!(
                Scenario::ALL.contains(&scenario) || Scenario::STATUS.contains(&scenario),
                "{row} points at {scenario}, which no fixture is told to mount"
            );
        }
    }

    #[test]
    fn a_fixture_that_says_nothing_is_taken_to_mount_every_row() {
        struct Silent;

        #[async_trait]
        impl WireFixtures for Silent {
            async fn mount(&self, _server: &MockServer, _scenario: Scenario) {}
        }

        for row in StatusRow::ALL {
            assert_eq!(
                Silent.status_support(row),
                StatusSupport::Mounted,
                "silence must never become a skip for {row}"
            );
        }
    }
}
