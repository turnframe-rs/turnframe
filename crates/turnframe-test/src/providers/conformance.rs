//! Running the provider conformance suite from a test, and judging its report.
//!
//! The suite itself lives in
//! [`turnframe_provider::conformance`]: it owns
//! the corpus, the rows of [`Check::run_order`] and the report. What an adapter
//! crate needs on top of it is small and always the same — build a runtime, run
//! the suite, decide whether the report is acceptable, print it when it is not —
//! so it lives here once and the
//! [`provider_conformance_suite!`](crate::provider_conformance_suite) macro
//! writes the rest.
//!
//! Everything the harness exposes is re-exported from this module, so an
//! adapter's test file reaches the whole of it through one import and never has
//! to name the provider crate to write a fixture. That includes [`RowSupport`],
//! the vocabulary both declaration hooks speak.
//!
//! # Why a skipped check is not a pass
//!
//! [`ConformanceReport::passed`] is `true` when nothing *failed*, and a check
//! the profile declared out of scope is skipped rather than failed. That is the
//! right default for the suite, and the wrong default for a release gate: an
//! adapter whose streaming row was skipped is unproven there, not proven.
//! [`accept`] therefore refuses a skip unless the caller listed the check as
//! deliberately out of scope, which turns "we never tested it" into a line of
//! code somebody had to write.
//!
//! # Declaring the rows a deployment cannot produce
//!
//! The harness lets a fixture say, in words, that this deployment cannot put a
//! row on the wire at all: [`WireFixtures::status_support`] for a per-status row
//! and [`WireFixtures::feature_support`] for one of the
//! [declarable](Check::is_declarable) feature rows. Both answer with a
//! [`RowSupport`], both need a reason, and the row is then reported as
//! *unproven* rather than failed.
//!
//! Written by hand that is two trait methods and a list of allowed skips that
//! has to agree with them — the same rows named twice, in two shapes, with
//! nothing keeping them in step. [`DeclaredRows`] wraps any fixtures and answers
//! both hooks from one table, and the
//! [`provider_conformance_suite!`](crate::provider_conformance_suite) macro's
//! `not_producible` list builds that table and feeds the same rows to [`accept`],
//! so the declaration is written once and the gate cannot drift from it.

use std::fmt::{self, Write as _};

use async_trait::async_trait;
use wiremock::MockServer;

pub use turnframe_provider::conformance::{
    Check, CheckResult, CheckStatus, ConformanceReport, ProviderFactory, RowSupport, Scenario,
    StatusRow, StatusSupport, WireFixtures, payloads, run_all,
};

/// The suite could not be run at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConformanceRunError {
    /// A Tokio runtime could not be started. Usually means
    /// [`run_blocking`] was called from inside a runtime; use [`run`] there.
    #[error("the conformance suite needs its own runtime, and one could not be started: {reason}")]
    RuntimeUnavailable {
        /// The runtime builder's own message.
        reason: String,
    },
}

/// A report that is not good enough to accept.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConformanceGap {
    /// A check failed.
    #[error("check {check} failed: {detail}")]
    Failed {
        /// Which check.
        check: Check,
        /// What the suite saw. Codes and shapes only.
        detail: String,
    },
    /// A check was skipped and the caller did not declare it out of scope.
    #[error(
        "check {check} was skipped and is therefore unproven ({reason}); \
         list it as allowed-to-skip if that is deliberate"
    )]
    UnexpectedSkip {
        /// Which check.
        check: Check,
        /// Why the suite skipped it.
        reason: String,
    },
}

/// Runs the whole suite. Use this inside an existing async test.
pub async fn run<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> ConformanceReport {
    run_all(factory, fixtures).await
}

/// Runs the whole suite on a private multi-threaded runtime.
///
/// This is what the macros use, so an adapter crate needs no async test
/// attribute and no runtime of its own.
///
/// # Errors
///
/// [`ConformanceRunError::RuntimeUnavailable`] when a runtime cannot be
/// started — in particular when this is called from inside one.
pub fn run_blocking<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Result<ConformanceReport, ConformanceRunError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ConformanceRunError::RuntimeUnavailable {
            reason: error.to_string(),
        })?;
    Ok(runtime.block_on(run_all(factory, fixtures)))
}

/// Decides whether a report is acceptable: nothing failed, and nothing was
/// skipped except the checks listed in `allow_skipped`.
///
/// # Errors
///
/// Every [`ConformanceGap`] found, in report order, so one run diagnoses every
/// row at once instead of one per fix.
pub fn accept(
    report: &ConformanceReport,
    allow_skipped: &[Check],
) -> Result<(), Vec<ConformanceGap>> {
    let mut gaps = Vec::new();
    for result in &report.results {
        match result.status {
            CheckStatus::Passed => {}
            CheckStatus::Failed => gaps.push(ConformanceGap::Failed {
                check: result.check,
                detail: result.detail.clone().unwrap_or_default(),
            }),
            CheckStatus::Skipped => {
                if !allow_skipped.contains(&result.check) {
                    gaps.push(ConformanceGap::UnexpectedSkip {
                        check: result.check,
                        reason: result.detail.clone().unwrap_or_default(),
                    });
                }
            }
        }
    }
    if gaps.is_empty() { Ok(()) } else { Err(gaps) }
}

/// Renders a report and its gaps as one assertion message.
#[must_use]
pub fn describe(report: &ConformanceReport, gaps: &[ConformanceGap]) -> String {
    let mut out = format!("{report}");
    for gap in gaps {
        let _ = write!(out, "\n{gap}");
    }
    out
}

/// Fixtures plus the rows this deployment cannot put on the wire, declared in
/// one table.
///
/// The harness asks two different questions about an undeliverable row —
/// [`WireFixtures::status_support`] for a per-status row,
/// [`WireFixtures::feature_support`] for a
/// [declarable](Check::is_declarable) feature row — and a gate then has to be
/// told the same rows a third time, as the `allow_skipped` list. Three places,
/// one fact. This type holds the fact once: it answers both hooks from its own
/// table and hands the very same rows to [`accept`] through
/// [`declared`](Self::declared), so a declaration and the gate that tolerates it
/// cannot drift apart.
///
/// A row it says nothing about falls through to the wrapped fixtures, so an
/// adapter that already implements either hook keeps it and adds to it.
///
/// The reason is not decoration. The harness fails a reason-less declaration on
/// purpose, and refuses one on a row that describes the adapter rather than the
/// deployment, so this type deliberately validates nothing itself: it passes the
/// declaration through and lets the suite judge it.
///
/// ```
/// use async_trait::async_trait;
/// use turnframe_test::providers::conformance::{
///     Check, DeclaredRows, RowSupport, Scenario, StatusRow, WireFixtures,
/// };
/// use wiremock::MockServer;
///
/// struct MyFixtures;
///
/// #[async_trait]
/// impl WireFixtures for MyFixtures {
///     async fn mount(&self, _server: &MockServer, _scenario: Scenario) {
///         // one vendor-shaped mock per scenario
///     }
/// }
///
/// let fixtures = DeclaredRows::new(MyFixtures)
///     .not_producible(
///         Check::StreamingReconstruction,
///         "this deployment runs the model with its streaming route switched off",
///     )
///     .not_producible(
///         Check::StatusMapping(StatusRow::RequestTimeout),
///         "the gateway answers 504, never 408",
///     );
///
/// // Both hooks answer from the one table...
/// assert_eq!(
///     fixtures.feature_support(Check::StreamingReconstruction).reason(),
///     Some("this deployment runs the model with its streaming route switched off"),
/// );
/// assert_eq!(
///     fixtures.status_support(StatusRow::RequestTimeout).reason(),
///     Some("the gateway answers 504, never 408"),
/// );
/// // ...anything else is mounted, exactly as the wrapped fixtures said.
/// assert_eq!(fixtures.feature_support(Check::Refusal), RowSupport::Mounted);
/// // ...and the gate is told the same two rows, not a hand-kept copy of them.
/// assert_eq!(
///     fixtures.declared(),
///     vec![
///         Check::StreamingReconstruction,
///         Check::StatusMapping(StatusRow::RequestTimeout),
///     ],
/// );
/// ```
pub struct DeclaredRows<W> {
    fixtures: W,
    declarations: Vec<(Check, String)>,
}

impl<W> DeclaredRows<W> {
    /// Wraps `fixtures`, declaring nothing yet.
    ///
    /// With no declarations the wrapper is transparent: every hook delegates,
    /// so wrapping fixtures that need no declaration changes no outcome.
    #[must_use]
    pub const fn new(fixtures: W) -> Self {
        Self {
            fixtures,
            declarations: Vec::new(),
        }
    }

    /// Declares that this deployment cannot produce `check`, and why.
    ///
    /// `check` is a feature row or a
    /// [`StatusMapping`](Check::StatusMapping) row; the wrapper routes it to
    /// whichever hook the harness will ask. The first declaration for a row
    /// wins, so a wrapper cannot contradict itself halfway down a builder
    /// chain.
    #[must_use]
    pub fn not_producible(mut self, check: Check, reason: impl Into<String>) -> Self {
        if !self.declarations.iter().any(|(row, _)| *row == check) {
            self.declarations.push((check, reason.into()));
        }
        self
    }

    /// The declared rows, in declaration order.
    ///
    /// This is what [`accept`] must be given as `allow_skipped`: the rows this
    /// deployment said it cannot exercise are exactly the skips a gate should
    /// tolerate, and no others.
    #[must_use]
    pub fn declared(&self) -> Vec<Check> {
        self.declarations.iter().map(|(check, _)| *check).collect()
    }

    /// The wrapped fixtures.
    #[must_use]
    pub const fn fixtures(&self) -> &W {
        &self.fixtures
    }

    /// The declaration for one row, when there is one.
    fn support(&self, check: Check) -> Option<RowSupport> {
        self.declarations
            .iter()
            .find(|(row, _)| *row == check)
            .map(|(_, reason)| RowSupport::not_producible(reason.clone()))
    }
}

impl<W> fmt::Debug for DeclaredRows<W> {
    /// Names the declared rows. The wrapped fixtures need not be `Debug`, and
    /// the reasons are the report's business rather than a rendering's.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeclaredRows")
            .field(
                "declared",
                &self
                    .declarations
                    .iter()
                    .map(|(check, _)| check.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl<W: WireFixtures> WireFixtures for DeclaredRows<W> {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        self.fixtures.mount(server, scenario).await;
    }

    fn status_support(&self, row: StatusRow) -> RowSupport {
        self.support(Check::StatusMapping(row))
            .unwrap_or_else(|| self.fixtures.status_support(row))
    }

    fn feature_support(&self, check: Check) -> RowSupport {
        self.support(check)
            .unwrap_or_else(|| self.fixtures.feature_support(check))
    }
}

/// Declares a conformance test for one provider-model adapter.
///
/// The macro is declarative on purpose (the workspace ships no proc macro), and
/// it expands to an ordinary `#[test]` that runs the suite on its own runtime —
/// so the adapter crate needs neither an async test attribute nor a runtime.
///
/// ```rust,ignore
/// turnframe_test::provider_conformance_suite! {
///     name: gpt_4o_conforms,
///     factory: OpenAiFactory::default(),
///     fixtures: OpenAiFixtures,
///     // Every row must pass. A row the profile puts out of scope is listed
///     // here, and nowhere else, so "we never tested it" is visible in review.
///     allow_skipped: [StreamingReconstruction],
/// }
/// ```
///
/// `factory` and `fixtures` are expressions evaluated once inside the test.
/// `allow_skipped` is optional and defaults to "no skip is acceptable"; its
/// entries are [`Check`](crate::providers::conformance::Check) variant names,
/// and a per-status row is written the way the variant reads:
/// `StatusMapping(RequestTimeout)`.
///
/// # Rows this deployment cannot put on the wire
///
/// `allow_skipped` tolerates a skip somebody else caused. `not_producible`
/// *causes* it, and says why:
///
/// ```rust,ignore
/// turnframe_test::provider_conformance_suite! {
///     name: the_bare_daemon_conforms,
///     factory: OllamaFactory::default(),
///     fixtures: OllamaFixtures,
///     not_producible: [
///         AuthenticationFailure =>
///             "`ollama serve` authenticates nothing: every request that reaches \
///              /api/chat is served, so no credential is ever rejected",
///         StatusMapping(TooManyRequests) =>
///             "the daemon queues requests behind the runner instead of rejecting \
///              them, so nothing in front of /api/chat ever answers 429",
///     ],
/// }
/// ```
///
/// Each entry is a row and the reason it cannot be produced. The macro builds a
/// [`DeclaredRows`](crate::providers::conformance::DeclaredRows) around the
/// fixtures, so the harness reports those rows as **unproven** with the reason
/// attached, and it feeds the very same rows to
/// [`accept`](crate::providers::conformance::accept) — the row is named once and
/// the gate follows it, instead of a declaration and an `allow_skipped` list
/// that agree only until somebody edits one of them.
///
/// Both lists may appear together, `allow_skipped` first. The harness decides
/// what a declaration is worth: a reason-less one fails its row, and one on a
/// row that describes the adapter rather than the deployment fails it too.
#[macro_export]
macro_rules! provider_conformance_suite {
    (
        name: $name:ident,
        factory: $factory:expr,
        fixtures: $fixtures:expr $(,)?
    ) => {
        $crate::provider_conformance_suite! {
            name: $name,
            factory: $factory,
            fixtures: $fixtures,
            allow_skipped: [],
            not_producible: [],
        }
    };
    (
        name: $name:ident,
        factory: $factory:expr,
        fixtures: $fixtures:expr,
        allow_skipped: [$($check:ident $(($row:ident))?),* $(,)?] $(,)?
    ) => {
        $crate::provider_conformance_suite! {
            name: $name,
            factory: $factory,
            fixtures: $fixtures,
            allow_skipped: [$($check $(($row))?),*],
            not_producible: [],
        }
    };
    (
        name: $name:ident,
        factory: $factory:expr,
        fixtures: $fixtures:expr,
        not_producible: [
            $($declared:ident $(($declared_row:ident))? => $reason:expr),* $(,)?
        ] $(,)?
    ) => {
        $crate::provider_conformance_suite! {
            name: $name,
            factory: $factory,
            fixtures: $fixtures,
            allow_skipped: [],
            not_producible: [$($declared $(($declared_row))? => $reason),*],
        }
    };
    (
        name: $name:ident,
        factory: $factory:expr,
        fixtures: $fixtures:expr,
        allow_skipped: [$($check:ident $(($row:ident))?),* $(,)?],
        not_producible: [
            $($declared:ident $(($declared_row:ident))? => $reason:expr),* $(,)?
        ] $(,)?
    ) => {
        #[test]
        fn $name() {
            let fixtures = $crate::providers::conformance::DeclaredRows::new($fixtures)
                $(
                    .not_producible(
                        $crate::providers::conformance::Check::$declared
                            $(($crate::providers::conformance::StatusRow::$declared_row))?,
                        $reason,
                    )
                )*;
            let report = match $crate::providers::conformance::run_blocking(&$factory, &fixtures) {
                Ok(report) => report,
                Err(error) => panic!("{error}"),
            };
            // The declared rows are the skips this gate tolerates, plus
            // whatever the caller listed on top of them.
            let allowed = {
                let mut rows = fixtures.declared();
                rows.extend_from_slice(&[
                    $(
                        $crate::providers::conformance::Check::$check
                            $(($crate::providers::conformance::StatusRow::$row))?
                    ),*
                ]);
                rows
            };
            if let Err(gaps) = $crate::providers::conformance::accept(&report, &allowed) {
                panic!(
                    "{}",
                    $crate::providers::conformance::describe(&report, &gaps)
                );
            }
        }
    };
}

/// Runs the suite and hands back the report, for a test that judges it itself.
///
/// Panics with the runtime error when a runtime cannot be started; everything
/// else is left to the caller.
///
/// ```rust,ignore
/// let report = turnframe_test::provider_conformance_report!(
///     factory: OpenAiFactory::default(),
///     fixtures: OpenAiFixtures,
/// );
/// assert!(report.result(Check::RateLimit).is_some());
/// ```
#[macro_export]
macro_rules! provider_conformance_report {
    (factory: $factory:expr, fixtures: $fixtures:expr $(,)?) => {
        match $crate::providers::conformance::run_blocking(&$factory, &$fixtures) {
            Ok(report) => report,
            Err(error) => panic!("{error}"),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_provider::ids::{ModelKey, ProviderKey};

    fn report(results: Vec<CheckResult>) -> ConformanceReport {
        let mut report = ConformanceReport::new(ProviderKey::from("p"), ModelKey::from("m"));
        for result in results {
            report.push(result);
        }
        report
    }

    #[test]
    fn a_clean_report_is_accepted() {
        let report = report(vec![
            CheckResult::passed(Check::MalformedJson),
            CheckResult::passed(Check::Timeout),
        ]);
        assert_eq!(accept(&report, &[]), Ok(()));
    }

    #[test]
    fn a_failure_is_reported_with_its_detail() {
        let report = report(vec![CheckResult::failed(Check::Timeout, "answered late")]);
        let gaps = accept(&report, &[]).unwrap_err();
        assert_eq!(
            gaps,
            vec![ConformanceGap::Failed {
                check: Check::Timeout,
                detail: "answered late".to_owned(),
            }]
        );
        assert!(describe(&report, &gaps).contains("check timeout failed"));
    }

    #[test]
    fn a_skip_is_refused_unless_it_was_declared() {
        let report = report(vec![CheckResult::skipped(
            Check::StreamingReconstruction,
            "the profile declares no streaming",
        )]);
        // Undeclared: the row is unproven, so the report is not acceptable...
        assert!(report.passed(), "the suite itself tolerates a skip");
        let gaps = accept(&report, &[]).unwrap_err();
        assert!(matches!(gaps[0], ConformanceGap::UnexpectedSkip { .. }));
        // ...and declaring it makes the intent explicit.
        assert_eq!(accept(&report, &[Check::StreamingReconstruction]), Ok(()));
    }

    /// Fixtures that mount nothing and declare one row on their own.
    struct InnerFixtures;

    #[async_trait]
    impl WireFixtures for InnerFixtures {
        async fn mount(&self, _server: &MockServer, _scenario: Scenario) {}

        fn feature_support(&self, check: Check) -> RowSupport {
            match check {
                Check::Refusal => RowSupport::not_producible("this gateway has no refusal channel"),
                _ => RowSupport::Mounted,
            }
        }

        fn status_support(&self, row: StatusRow) -> RowSupport {
            match row {
                StatusRow::ContentFilter => {
                    RowSupport::not_producible("nothing here inspects a prompt")
                }
                _ => RowSupport::Mounted,
            }
        }
    }

    #[test]
    fn a_declaration_answers_both_hooks_and_reaches_the_gate() {
        let fixtures = DeclaredRows::new(InnerFixtures)
            .not_producible(Check::StreamingIncremental, "no streaming route")
            .not_producible(
                Check::StatusMapping(StatusRow::RequestTimeout),
                "the gateway answers 504",
            );

        assert_eq!(
            fixtures
                .feature_support(Check::StreamingIncremental)
                .reason(),
            Some("no streaming route")
        );
        assert_eq!(
            fixtures.status_support(StatusRow::RequestTimeout).reason(),
            Some("the gateway answers 504")
        );
        // The gate is handed exactly the declared rows, so `allow_skipped` and
        // the declaration cannot say different things.
        assert_eq!(
            fixtures.declared(),
            vec![
                Check::StreamingIncremental,
                Check::StatusMapping(StatusRow::RequestTimeout),
            ]
        );
    }

    #[test]
    fn a_row_the_wrapper_says_nothing_about_falls_through_to_the_fixtures() {
        let fixtures = DeclaredRows::new(InnerFixtures)
            .not_producible(Check::StreamingIncremental, "no streaming route");
        // The wrapped fixtures keep both of their own declarations...
        assert_eq!(
            fixtures.feature_support(Check::Refusal).reason(),
            Some("this gateway has no refusal channel")
        );
        assert_eq!(
            fixtures.status_support(StatusRow::ContentFilter).reason(),
            Some("nothing here inspects a prompt")
        );
        // ...and everything undeclared is still mounted, from either side.
        assert_eq!(
            fixtures.feature_support(Check::RateLimit),
            RowSupport::Mounted
        );
        assert_eq!(
            fixtures.status_support(StatusRow::Forbidden),
            RowSupport::Mounted
        );
        // Those are the fixtures' own words, not the wrapper's table.
        assert_eq!(fixtures.declared(), vec![Check::StreamingIncremental]);
        assert!(format!("{fixtures:?}").contains("streaming_incremental"));
    }

    #[test]
    fn the_first_reason_given_for_a_row_is_the_one_it_keeps() {
        let fixtures = DeclaredRows::new(InnerFixtures)
            .not_producible(Check::RateLimit, "nothing meters this deployment")
            .not_producible(Check::RateLimit, "on second thoughts, something might");
        assert_eq!(
            fixtures.feature_support(Check::RateLimit).reason(),
            Some("nothing meters this deployment")
        );
        assert_eq!(fixtures.declared(), vec![Check::RateLimit]);
    }

    #[test]
    fn wrapping_fixtures_with_nothing_to_declare_changes_nothing() {
        let fixtures = DeclaredRows::new(InnerFixtures);
        assert!(fixtures.declared().is_empty());
        assert_eq!(
            fixtures.fixtures().feature_support(Check::Refusal).reason(),
            Some("this gateway has no refusal channel")
        );
        assert_eq!(
            fixtures.feature_support(Check::Refusal),
            InnerFixtures.feature_support(Check::Refusal)
        );
        assert_eq!(
            fixtures.status_support(StatusRow::ContentFilter),
            InnerFixtures.status_support(StatusRow::ContentFilter)
        );
    }

    #[test]
    fn failures_and_skips_are_reported_together_in_check_order() {
        let report = report(vec![
            CheckResult::skipped(Check::StreamingReconstruction, "no streaming"),
            CheckResult::failed(Check::Timeout, "answered late"),
        ]);
        let gaps = accept(&report, &[]).unwrap_err();
        assert_eq!(gaps.len(), 2);
        assert!(matches!(gaps[0], ConformanceGap::UnexpectedSkip { .. }));
        assert!(matches!(gaps[1], ConformanceGap::Failed { .. }));
    }
}
