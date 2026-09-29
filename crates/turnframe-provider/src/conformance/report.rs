//! What a conformance run produces.
//!
//! The report is data, not an assertion. No check panics and
//! [`run_all`](super::run_all) never aborts a run early, so a failing adapter
//! gets a full picture instead of the first thing that broke. The calling test
//! turns it into a verdict:
//!
//! ```rust,ignore
//! let report = run_all(&factory, &fixtures).await;
//! assert!(report.passed(), "{report}");
//! ```

use std::fmt;

use super::status::StatusRow;
use crate::ids::{ModelKey, ProviderKey};

/// One row of the spec §20.8 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Check {
    /// A schema-conformant body maps to a normalized response with every field
    /// intact.
    ValidStructuredResponse,
    /// A body that is not JSON is a typed failure, never a partial parse.
    MalformedJson,
    /// A field the schema forbids rejects the whole response.
    UnknownFields,
    /// A missing required field rejects the whole response; no default is
    /// filled in.
    MissingRequiredFields,
    /// A multi-act plan round-trips in order, as one proposal.
    MultipleActs,
    /// Tool and read request ids survive, and
    /// [`preserves_call_ids`](crate::capabilities::ProviderCapabilities::preserves_call_ids)
    /// matches what actually happens.
    ToolAndReadRequestIds,
    /// The reassembled stream equals the non-streamed answer for the same
    /// exchange.
    StreamingReconstruction,
    /// An answer that arrived as several wire events produced several deltas.
    ///
    /// Without this row an adapter that reads the whole body, then emits one
    /// [`TextDelta`](crate::stream::StreamEvent::TextDelta) at the end,
    /// reassembles perfectly and gives an adopter nothing: the point of
    /// streaming is prose appearing while it is written.
    StreamingIncremental,
    /// The streamed and the whole path report the same
    /// [`TokenUsage`](crate::response::TokenUsage) for the same answer.
    ///
    /// Usage rides on a final frame in most vendors, so it is the first thing a
    /// streaming implementation drops — and a cost figure that depends on which
    /// path served the turn is worse than no cost figure.
    StreamingUsageAgreement,
    /// The reported usage keeps its own contract: `cached_input` is a subset of
    /// `input`, never a figure alongside it.
    ///
    /// `input` is the **whole** prompt and `cached_input` is the part of it the
    /// provider served from its cache. An adapter that reports the net figure —
    /// input minus cache — makes every cost estimate wrong and every cache-hit
    /// ratio over one.
    TokenUsageContract,
    /// An empty body is a typed failure or an empty response, never an empty
    /// plan that parses.
    EmptyOutput,
    /// A refusal maps to the refusal variant, distinguishable from malformed
    /// output.
    Refusal,
    /// A deadline that passes produces
    /// [`Timeout`](crate::error::ProviderErrorKind::Timeout).
    Timeout,
    /// A rate limit maps to its variant and preserves the `Retry-After` hint.
    RateLimit,
    /// Rejected credentials map to a non-retryable variant, and the key never
    /// appears in the error.
    AuthenticationFailure,
    /// A context overflow gets its own variant, so the runtime can shrink the
    /// prompt instead of retrying blindly.
    ContextOverflow,
    /// Dropping the future aborts cleanly, with nothing left running.
    Cancellation,
    /// Every observed error carries the [`RetryClass`](crate::error::RetryClass)
    /// the policy layer expects.
    RetryClassification,
    /// The configured credential appears in no rendering of the adapter, its
    /// responses or its errors.
    SecretRedaction,
    /// A profile that declares schema enforcement actually sends the schema.
    NoSilentCapabilityDowngrade,
    /// One wire failure maps onto the one error kind it means (spec §24).
    ///
    /// [`RetryClassification`](Self::RetryClassification) proves the class of
    /// whatever kind the adapter produced; these rows prove it produced the
    /// right kind. Without them an adapter that flattens every failure onto
    /// [`Transport`](crate::error::ProviderErrorKind::Transport) passes the
    /// whole suite.
    StatusMapping(StatusRow),
}

impl Check {
    /// The feature rows of spec §20.8, in the order
    /// [`run_all`](super::run_all) executes them.
    ///
    /// Seventeen come from the specification's own table; the three that follow
    /// [`StreamingReconstruction`](Self::StreamingReconstruction) were added
    /// because reassembly on its own proves neither that a stream is
    /// incremental nor that the counts survive it.
    ///
    /// A full run also reports the thirteen [`STATUS`](Self::STATUS) rows;
    /// [`run_order`](Self::run_order) is the complete sequence.
    pub const ALL: [Self; 20] = [
        Self::ValidStructuredResponse,
        Self::MalformedJson,
        Self::UnknownFields,
        Self::MissingRequiredFields,
        Self::MultipleActs,
        Self::ToolAndReadRequestIds,
        Self::StreamingReconstruction,
        Self::StreamingIncremental,
        Self::StreamingUsageAgreement,
        Self::TokenUsageContract,
        Self::EmptyOutput,
        Self::Refusal,
        Self::Timeout,
        Self::RateLimit,
        Self::AuthenticationFailure,
        Self::ContextOverflow,
        Self::Cancellation,
        Self::RetryClassification,
        Self::SecretRedaction,
        Self::NoSilentCapabilityDowngrade,
    ];

    /// The thirteen per-status rows, in the order a run executes them.
    ///
    /// They run before [`RetryClassification`](Self::RetryClassification), so
    /// the kinds they observe are classified too.
    pub const STATUS: [Self; 13] = [
        Self::StatusMapping(StatusRow::Unauthorized),
        Self::StatusMapping(StatusRow::Forbidden),
        Self::StatusMapping(StatusRow::NotFound),
        Self::StatusMapping(StatusRow::RequestTimeout),
        Self::StatusMapping(StatusRow::ConnectionReset),
        Self::StatusMapping(StatusRow::TooManyRequests),
        Self::StatusMapping(StatusRow::ContextLength),
        Self::StatusMapping(StatusRow::BadRequest),
        Self::StatusMapping(StatusRow::InternalServerError),
        Self::StatusMapping(StatusRow::ServiceUnavailable),
        Self::StatusMapping(StatusRow::ContentFilter),
        Self::StatusMapping(StatusRow::ExpiredCredential),
        Self::StatusMapping(StatusRow::QuotaExhausted),
    ];

    /// Every row a full run reports, in execution order.
    ///
    /// The [`STATUS`](Self::STATUS) rows sit where they are exercised — after
    /// the failure checks and before
    /// [`RetryClassification`](Self::RetryClassification) — rather than
    /// appended at the end, so the classification row covers what they saw.
    #[must_use]
    pub fn run_order() -> Vec<Self> {
        let mut order = Vec::with_capacity(Self::ALL.len() + Self::STATUS.len());
        for check in Self::ALL {
            if check == Self::RetryClassification {
                order.extend_from_slice(&Self::STATUS);
            }
            order.push(check);
        }
        order
    }

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ValidStructuredResponse => "valid_structured_response",
            Self::MalformedJson => "malformed_json",
            Self::UnknownFields => "unknown_fields",
            Self::MissingRequiredFields => "missing_required_fields",
            Self::MultipleActs => "multiple_acts",
            Self::ToolAndReadRequestIds => "tool_and_read_request_ids",
            Self::StreamingReconstruction => "streaming_reconstruction",
            Self::StreamingIncremental => "streaming_incremental",
            Self::StreamingUsageAgreement => "streaming_usage_agreement",
            Self::TokenUsageContract => "token_usage_contract",
            Self::EmptyOutput => "empty_output",
            Self::Refusal => "refusal",
            Self::Timeout => "timeout",
            Self::RateLimit => "rate_limit",
            Self::AuthenticationFailure => "authentication_failure",
            Self::ContextOverflow => "context_overflow",
            Self::Cancellation => "cancellation",
            Self::RetryClassification => "retry_classification",
            Self::SecretRedaction => "secret_redaction",
            Self::NoSilentCapabilityDowngrade => "no_silent_capability_downgrade",
            Self::StatusMapping(row) => row.as_str(),
        }
    }

    /// The per-status row this check exercises, when it is one.
    #[must_use]
    pub const fn status_row(self) -> Option<StatusRow> {
        match self {
            Self::StatusMapping(row) => Some(row),
            _ => None,
        }
    }

    /// Returns `true` for the checks whose failure means the profile's
    /// declaration is wrong rather than its plumbing.
    ///
    /// Failing one of these is never fixed by relaxing the test: the
    /// declaration must be lowered instead (spec §20.3).
    #[must_use]
    pub const fn is_declaration_check(self) -> bool {
        matches!(
            self,
            Self::NoSilentCapabilityDowngrade
                | Self::ToolAndReadRequestIds
                | Self::StreamingReconstruction
                | Self::StreamingIncremental
                | Self::StreamingUsageAgreement
        )
    }

    /// Returns `true` for the feature rows a deployment may declare it cannot
    /// produce, in words, through
    /// [`WireFixtures::feature_support`](super::WireFixtures::feature_support).
    ///
    /// These are the rows whose behaviour depends on what sits *around* the
    /// model rather than on the model: a daemon started on a laptop
    /// authenticates nobody, meters nothing and has no safety filter, and a
    /// deployment may genuinely have no streaming endpoint. Every other row is
    /// a property of the adapter itself and cannot be declared away — a
    /// declaration on one of those fails the row rather than skipping it.
    ///
    /// A **per-status** row is declared through
    /// [`WireFixtures::status_support`](super::WireFixtures::status_support)
    /// instead, which is why it is not listed here.
    #[must_use]
    pub const fn is_declarable(self) -> bool {
        matches!(
            self,
            Self::StreamingReconstruction
                | Self::StreamingIncremental
                | Self::StreamingUsageAgreement
                | Self::AuthenticationFailure
                | Self::RateLimit
                | Self::Refusal
        )
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How one check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckStatus {
    /// The adapter behaved as the spec requires.
    Passed,
    /// It did not.
    Failed,
    /// The check does not apply: the profile declares the feature absent, or
    /// the fixtures do not model the scenario.
    Skipped,
}

impl CheckStatus {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

impl fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One check and how it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Which check.
    pub check: Check,
    /// Its status.
    pub status: CheckStatus,
    /// Why it failed, or why it was skipped. Codes and shapes only — a
    /// conformance detail is written into CI output, so it holds no credential
    /// and no response body.
    pub detail: Option<String>,
}

impl CheckResult {
    /// A passing result.
    #[must_use]
    pub const fn passed(check: Check) -> Self {
        Self {
            check,
            status: CheckStatus::Passed,
            detail: None,
        }
    }

    /// A failing result.
    #[must_use]
    pub fn failed(check: Check, detail: impl Into<String>) -> Self {
        Self {
            check,
            status: CheckStatus::Failed,
            detail: Some(detail.into()),
        }
    }

    /// A skipped result.
    #[must_use]
    pub fn skipped(check: Check, reason: impl Into<String>) -> Self {
        Self {
            check,
            status: CheckStatus::Skipped,
            detail: Some(reason.into()),
        }
    }
}

impl fmt::Display for CheckResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:<32} {}", self.check.as_str(), self.status)?;
        if let Some(detail) = &self.detail {
            write!(f, "  {detail}")?;
        }
        Ok(())
    }
}

/// The result of one adapter's conformance run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    /// The provider that was exercised.
    pub provider: ProviderKey,
    /// The model that was exercised. Conformance is per provider-model pair;
    /// a passing report says nothing about a sibling model.
    pub model: ModelKey,
    /// Every check, in [`Check::run_order`] order: the feature rows of
    /// [`Check::ALL`] with the [`Check::STATUS`] rows spliced in where they are
    /// exercised, which is before
    /// [`RetryClassification`](Check::RetryClassification) rather than at the
    /// end.
    pub results: Vec<CheckResult>,
}

impl ConformanceReport {
    /// An empty report for a provider-model pair.
    #[must_use]
    pub fn new(provider: ProviderKey, model: ModelKey) -> Self {
        Self {
            provider,
            model,
            results: Vec::new(),
        }
    }

    /// Records one result.
    pub fn push(&mut self, result: CheckResult) {
        self.results.push(result);
    }

    /// Returns `true` when nothing failed. A skipped check does not fail a run,
    /// but it does mean the adapter is unproven on that row.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures().is_empty()
    }

    /// Every failing check.
    #[must_use]
    pub fn failures(&self) -> Vec<&CheckResult> {
        self.results
            .iter()
            .filter(|result| result.status == CheckStatus::Failed)
            .collect()
    }

    /// Every skipped check.
    #[must_use]
    pub fn skipped(&self) -> Vec<&CheckResult> {
        self.results
            .iter()
            .filter(|result| result.status == CheckStatus::Skipped)
            .collect()
    }

    /// The result of one check, when it ran.
    #[must_use]
    pub fn result(&self, check: Check) -> Option<&CheckResult> {
        self.results.iter().find(|result| result.check == check)
    }

    /// Renders the run as a Markdown compatibility table.
    ///
    /// A skipped row renders as **unproven**, with the reason it was skipped,
    /// because a skip is not a pass: it means the run never exercised the
    /// behaviour. Copying this table into a documentation page therefore
    /// cannot claim more than the run demonstrated — which is the whole reason
    /// the method exists rather than a caller filtering
    /// [`results`](Self::results) by hand and quietly dropping the skips.
    #[must_use]
    pub fn compatibility_table(&self) -> String {
        let (passed, failed, skipped) = self.counts();
        let mut out = format!(
            "**{}/{}**: {passed} proven, {failed} failed, {skipped} unproven\n\n\
             | Check | Result | Detail |\n\
             | --- | --- | --- |\n",
            self.provider, self.model
        );
        for result in &self.results {
            let verdict = match result.status {
                CheckStatus::Passed => "proven",
                CheckStatus::Failed => "failed",
                CheckStatus::Skipped => "unproven",
            };
            let detail = result
                .detail
                .as_deref()
                .unwrap_or_default()
                .replace('|', "\\|");
            out.push_str(&format!(
                "| `{}` | {verdict} | {detail} |\n",
                result.check.as_str()
            ));
        }
        out
    }

    /// Passed, failed and skipped counts.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for result in &self.results {
            match result.status {
                CheckStatus::Passed => counts.0 += 1,
                CheckStatus::Failed => counts.1 += 1,
                CheckStatus::Skipped => counts.2 += 1,
            }
        }
        counts
    }
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (passed, failed, skipped) = self.counts();
        writeln!(
            f,
            "conformance {}/{}: {passed} passed, {failed} failed, {skipped} skipped",
            self.provider, self.model
        )?;
        for result in &self.results {
            writeln!(f, "  {result}")?;
        }
        if skipped > 0 {
            writeln!(
                f,
                "  {skipped} row(s) unproven: a compatibility table must not claim them"
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_row_has_a_unique_label() {
        let mut labels: Vec<&str> = Check::ALL.iter().map(|check| check.as_str()).collect();
        assert_eq!(
            labels.len(),
            20,
            "the seventeen rows of spec §20.8 plus the three streaming and usage rows"
        );
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 20);
    }

    #[test]
    fn only_the_rows_a_deployment_can_lack_are_declarable() {
        for check in [
            Check::StreamingReconstruction,
            Check::StreamingIncremental,
            Check::StreamingUsageAgreement,
            Check::AuthenticationFailure,
            Check::RateLimit,
            Check::Refusal,
        ] {
            assert!(check.is_declarable(), "{check}");
        }
        // A row that is a property of the adapter can never be declared away:
        // "this endpoint cannot produce a malformed body" is not a statement
        // about a deployment, it is a dodged failure.
        for check in [
            Check::ValidStructuredResponse,
            Check::MalformedJson,
            Check::MultipleActs,
            Check::TokenUsageContract,
            Check::SecretRedaction,
            Check::RetryClassification,
            Check::StatusMapping(StatusRow::Forbidden),
        ] {
            assert!(!check.is_declarable(), "{check}");
        }
    }

    #[test]
    fn the_run_order_holds_every_row_once_with_the_status_rows_before_classification() {
        let order = Check::run_order();
        assert_eq!(order.len(), Check::ALL.len() + Check::STATUS.len());
        let mut labels: Vec<&str> = order.iter().map(|check| check.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), order.len(), "labels must stay unique");

        let classification = order
            .iter()
            .position(|check| *check == Check::RetryClassification)
            .expect("the classification row runs");
        for status in Check::STATUS {
            let at = order
                .iter()
                .position(|check| *check == status)
                .unwrap_or_else(|| panic!("{status} runs"));
            assert!(
                at < classification,
                "{status} must be observed before the classification row"
            );
        }
        assert_eq!(
            Check::StatusMapping(StatusRow::Forbidden).status_row(),
            Some(StatusRow::Forbidden)
        );
        assert_eq!(Check::Timeout.status_row(), None);
        assert!(!Check::StatusMapping(StatusRow::Forbidden).is_declaration_check());
    }

    #[test]
    fn a_compatibility_table_calls_a_skipped_row_unproven() {
        let mut report = ConformanceReport::new(ProviderKey::from("p"), ModelKey::from("m"));
        report.push(CheckResult::passed(Check::ValidStructuredResponse));
        report.push(CheckResult::skipped(
            Check::StatusMapping(StatusRow::RequestTimeout),
            "HTTP 408 is not produced by this endpoint (it answers 400), so timeout is unproven",
        ));
        report.push(CheckResult::failed(
            Check::StatusMapping(StatusRow::Forbidden),
            "HTTP 403 mapped to authentication, expected authorization",
        ));
        let table = report.compatibility_table();
        assert!(
            table.contains("**p/m**: 1 proven, 1 failed, 1 unproven"),
            "{table}"
        );
        assert!(
            table.contains("| `valid_structured_response` | proven |"),
            "{table}"
        );
        assert!(
            table.contains("| `status_408_timeout` | unproven |"),
            "{table}"
        );
        assert!(
            table.contains("| `status_403_authorization` | failed |"),
            "{table}"
        );
        // A row is never silently dropped: every result is a table row.
        assert_eq!(
            table.lines().filter(|line| line.starts_with("| `")).count(),
            3
        );
        // And the run's own rendering says the table cannot claim everything.
        assert!(report.to_string().contains("1 row(s) unproven"), "{report}");
    }

    #[test]
    fn a_detail_containing_a_pipe_cannot_break_the_table() {
        let mut report = ConformanceReport::new(ProviderKey::from("p"), ModelKey::from("m"));
        report.push(CheckResult::failed(
            Check::StatusMapping(StatusRow::ConnectionReset),
            "a connection reset mapped to other, expected timeout|transport",
        ));
        let table = report.compatibility_table();
        assert!(table.contains(r"timeout\|transport"), "{table}");
        assert_eq!(
            table.lines().filter(|line| line.starts_with("| `")).count(),
            1
        );
    }

    #[test]
    fn a_skipped_check_does_not_fail_a_run() {
        let mut report = ConformanceReport::new(ProviderKey::from("p"), ModelKey::from("m"));
        report.push(CheckResult::passed(Check::ValidStructuredResponse));
        report.push(CheckResult::skipped(
            Check::StreamingReconstruction,
            "streaming not declared",
        ));
        assert!(report.passed());
        assert_eq!(report.counts(), (1, 0, 1));
        assert_eq!(report.skipped().len(), 1);
        assert!(report.result(Check::MalformedJson).is_none());
    }

    #[test]
    fn a_failure_is_reported_with_its_detail() {
        let mut report = ConformanceReport::new(ProviderKey::from("p"), ModelKey::from("m"));
        report.push(CheckResult::failed(Check::MalformedJson, "parsed a prefix"));
        assert!(!report.passed());
        assert_eq!(report.failures().len(), 1);
        let rendered = report.to_string();
        assert!(rendered.contains("conformance p/m"), "{rendered}");
        assert!(rendered.contains("malformed_json"), "{rendered}");
        assert!(rendered.contains("parsed a prefix"), "{rendered}");
        assert!(rendered.contains("1 failed"), "{rendered}");
    }

    #[test]
    fn declaration_checks_are_marked() {
        assert!(Check::NoSilentCapabilityDowngrade.is_declaration_check());
        assert!(Check::StreamingReconstruction.is_declaration_check());
        assert!(!Check::Timeout.is_declaration_check());
        assert_eq!(CheckStatus::Passed.to_string(), "passed");
        assert_eq!(Check::Timeout.to_string(), "timeout");
    }
}
