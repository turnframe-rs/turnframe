//! An executable statement of the **executor** contract (I13, I14, spec §23.1).
//!
//! A store proves itself against `turnframe_store::conformance` and a provider
//! adapter against [`providers::conformance`](crate::providers::conformance).
//! The third thing an adopter writes is the [`WorkflowExecutor`], and it is
//! where optimistic concurrency and idempotency actually live: a card bound to
//! revision `N` is only safe because some executor refuses a stale write, and a
//! turn replayed after a crash is only harmless because some executor
//! recognises the key it already committed. Those rules are prose on the trait,
//! and prose does not fail a build.
//!
//! The check nobody writes for themselves is **partial-batch recovery**, which
//! is why [`ExecutorFactory::interrupt_after`] exists: the suite cannot
//! half-commit a batch through the trait, so it asks the implementation to.
//! [`docs/recipes.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/recipes.md)
//! describes the two shapes that defect usually takes.
//!
//! Nothing here panics: every check returns a result, [`run_all`] collects them
//! all rather than stopping at the first, and a failure names revisions,
//! digests, error variants and check names — never a case's state, which is the
//! adopter's data and is compared as a digest.
//!
//! # Running it
//!
//! Implement [`ExecutorFactory`] once, then hand it to [`run_all`].
//!
//! ```
//! use turnframe_test::executors;
//! use turnframe_test::workflows::trip::conformance_case;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let report = executors::run_all(&conformance_case()).await;
//! assert!(report.passed(), "{report}");
//! assert_eq!(report.outcomes.len(), executors::CHECK_COUNT);
//! # });
//! # Ok(())
//! # }
//! ```
//!
//! [`InMemoryCase`] is the reference factory: it wires the kit's own
//! [`InMemoryExecutor`](crate::workflows::InMemoryExecutor) to any
//! [`PureWorkflow`](crate::workflows::PureWorkflow), and it is the smallest
//! complete example of what an adopter writes to point the suite at their own
//! executor.
//!
//! While an executor is still being written, run one rule at a time: every
//! `check_*` function is public and takes the same factory.
//!
//! # It never panics
//!
//! Every check returns [`Result<(), ConformanceFailure>`](ConformanceFailure)
//! and [`run_all`] collects the results into a [`ConformanceReport`] without
//! stopping at the first failure — a broken executor usually breaks several
//! rules at once, and seeing all of them is faster to fix than seeing the first
//! one seven times. Nothing here unwraps, asserts or panics, so the suite is
//! usable outside a test harness: in a migration tool, or as a boot-time gate
//! on a freshly written adapter.
//!
//! Failure details name revisions, digests, error variants and check names.
//! They never render a case's state, because that state is the adopter's data:
//! two states are compared through
//! [`canonical_digest`](turnframe_core::hash::canonical_digest), and a
//! mismatch is reported as two digests.
//!
//! # What is covered
//!
//! | Check | Rule |
//! |---|---|
//! | [`check_stale_expected_revision_is_a_conflict`] | a batch planned against a superseded revision is refused, and the case is not overwritten (I13) |
//! | [`check_commit_reports_the_revision_it_reached`] | the revision in the commit is the one a later batch must be planned against |
//! | [`check_repeated_key_replays_the_outcome`] | a key seen before returns the original outcome and repeats no effect (I14) |
//! | [`check_repeated_key_with_another_command_is_refused`] | the same key carrying a different command is a mismatch, never a replay (I14) |
//! | [`check_per_case_batch_is_all_or_nothing`] | one refused envelope discards the whole batch, and a `PerCase` batch may not span two cases |
//! | [`check_interrupted_batch_resumes_to_the_same_state`] | a batch that half-committed resumes to exactly the state an uninterrupted one reaches |
//! | [`check_refused_command_leaves_the_case_byte_identical`] | a refusal writes nothing at all, and stays a refusal when it is retried |

mod batch;
mod fixtures;
mod idempotency;
mod in_memory;
mod revision;

use std::fmt;

use turnframe_core::case::CaseRef;
use turnframe_core::command::CommandBatch;
use turnframe_core::flow::{WorkflowDefinition, WorkflowExecutor};
use turnframe_core::turn::ActorContext;

pub use self::batch::{
    check_interrupted_batch_resumes_to_the_same_state, check_per_case_batch_is_all_or_nothing,
    check_refused_command_leaves_the_case_byte_identical,
};
pub use self::idempotency::{
    check_repeated_key_replays_the_outcome, check_repeated_key_with_another_command_is_refused,
};
pub use self::in_memory::{CONFORMANCE_ACCOUNT, CONFORMANCE_USER, InMemoryCase};
pub use self::revision::{
    check_commit_reports_the_revision_it_reached, check_stale_expected_revision_is_a_conflict,
};

/// The workflow an [`ExecutorFactory`] builds executors for.
pub type WorkflowOf<F> = <F as ExecutorFactory>::Workflow;

/// The command type of [`WorkflowOf<F>`](WorkflowOf).
pub type CommandOf<F> = <WorkflowOf<F> as WorkflowDefinition>::Command;

/// The batch type the suite hands to the executor under test.
pub type BatchOf<F> = CommandBatch<CommandOf<F>>;

/// The [`SeededCase`] an [`ExecutorFactory`] produces.
pub type SeedOf<F> = SeededCase<WorkflowOf<F>, <F as ExecutorFactory>::Executor>;

/// One rule of the executor contract that an implementation broke.
///
/// `detail` names what the check expected and what it observed. It carries
/// revisions, digests and error variants only — never a rendered case state —
/// so it is safe to log in full.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("executor conformance check `{check}` failed: {detail}")]
pub struct ConformanceFailure {
    /// Name of the failing check, matching its function name.
    pub check: &'static str,
    /// What was expected and what happened.
    pub detail: String,
}

impl ConformanceFailure {
    /// Builds a failure.
    #[must_use]
    pub fn new(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            detail: detail.into(),
        }
    }
}

/// What one check concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    /// Name of the check.
    pub check: &'static str,
    /// The failure, when it failed.
    pub failure: Option<ConformanceFailure>,
}

impl CheckOutcome {
    /// Returns `true` when the check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

/// The result of a whole [`run_all`].
///
/// `Display` renders one line per check, so `assert!(report.passed(), "{report}")`
/// prints a usable diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    /// Every check, in the order [`run_all`] ran them.
    pub outcomes: Vec<CheckOutcome>,
}

impl ConformanceReport {
    /// Returns `true` when every check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(CheckOutcome::passed)
    }

    /// Every failure, in run order.
    pub fn failures(&self) -> impl Iterator<Item = &ConformanceFailure> {
        self.outcomes
            .iter()
            .filter_map(|outcome| outcome.failure.as_ref())
    }

    /// Names of the checks that failed, in run order.
    ///
    /// This is what a falsification test asserts on: break one rule in an
    /// executor and exactly one name must appear here.
    pub fn failed_checks(&self) -> impl Iterator<Item = &'static str> {
        self.failures().map(|failure| failure.check)
    }

    /// Turns the report into a `Result`, keeping the first failure.
    ///
    /// # Errors
    /// * The first [`ConformanceFailure`] in run order.
    pub fn into_result(self) -> Result<(), ConformanceFailure> {
        match self.outcomes.into_iter().find_map(|o| o.failure) {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failed = self.failures().count();
        writeln!(
            f,
            "{} of {} executor conformance checks passed",
            self.outcomes.len() - failed,
            self.outcomes.len()
        )?;
        for outcome in &self.outcomes {
            match &outcome.failure {
                None => writeln!(f, "  pass  {}", outcome.check)?,
                Some(failure) => writeln!(f, "  FAIL  {}: {}", outcome.check, failure.detail)?,
            }
        }
        Ok(())
    }
}

/// A fresh executor, the case seeded in it, and the commands the suite drives
/// it with.
///
/// The suite builds every envelope itself — identifiers, idempotency keys and
/// batch scope are derived, not supplied — so an adopter only has to say *what*
/// to execute, never *how* to address it.
pub struct SeededCase<W: WorkflowDefinition, E: WorkflowExecutor<W>> {
    /// The executor under test, holding exactly one case.
    pub executor: E,
    /// The actor every envelope runs as. Its account owns the seeded case.
    pub actor: ActorContext,
    /// The seeded case, at the revision the seed left it.
    pub case_ref: CaseRef,
    /// A command that applies from the seeded state.
    ///
    /// It must **not** be naturally idempotent: applying it twice has to leave
    /// a state different from applying it once (append a line, do not assign a
    /// field). A suite that used an assignment could not tell a replay from a
    /// second execution, which is the whole point of half the checks here.
    pub first: W::Command,
    /// A command that applies after [`first`](Self::first) and serializes
    /// differently from it.
    pub second: W::Command,
    /// A command the domain refuses, both from the seeded state and from the
    /// state [`first`](Self::first) leaves behind.
    ///
    /// The refusal must come from the domain — [`ExecutionError::Rejected`] —
    /// and not from a revision or an idempotency rule, because the checks that
    /// use it are about what a *refusal* leaves behind.
    ///
    /// [`ExecutionError::Rejected`]: turnframe_core::error::ExecutionError::Rejected
    pub refused: W::Command,
}

impl<W: WorkflowDefinition, E: WorkflowExecutor<W>> SeededCase<W, E> {
    /// Assembles a seeded case.
    pub fn new(
        executor: E,
        actor: ActorContext,
        case_ref: CaseRef,
        first: W::Command,
        second: W::Command,
        refused: W::Command,
    ) -> Self {
        Self {
            executor,
            actor,
            case_ref,
            first,
            second,
            refused,
        }
    }
}

impl<W: WorkflowDefinition, E: WorkflowExecutor<W>> fmt::Debug for SeededCase<W, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeededCase")
            .field("workflow", &self.case_ref.workflow)
            .field("case_id", &self.case_ref.case_id)
            .field("revision", &self.case_ref.expected_revision)
            .finish_non_exhaustive()
    }
}

/// Builds a fresh executor with one case in it, once per check.
///
/// # The seed must be deterministic
///
/// [`seed`](Self::seed) is called more than once inside a single check —
/// [`check_interrupted_batch_resumes_to_the_same_state`] compares an
/// interrupted run against an uninterrupted one — and the two runs are only
/// comparable if both start from the same state at the same revision, with the
/// same account and case identifier. A factory that mints a random case id per
/// call, or reuses one executor across calls, makes that check meaningless
/// rather than failing it.
///
/// The seeded revision may be anything, including
/// [`CaseRevision::ZERO`](turnframe_core::ids::CaseRevision::ZERO) for a case
/// that does not exist yet, as long as `case_ref.expected_revision` is the
/// revision `load` reports.
#[async_trait::async_trait]
pub trait ExecutorFactory: Send + Sync {
    /// The workflow whose executor is under test.
    type Workflow: WorkflowDefinition;
    /// The executor implementation being proven.
    type Executor: WorkflowExecutor<Self::Workflow> + Send + Sync;

    /// Builds a fresh executor and seeds one case in it.
    ///
    /// # Errors
    /// * A description of what went wrong while building or seeding. The suite
    ///   turns it into a [`ConformanceFailure`] for the check that asked.
    async fn seed(&self) -> Result<SeedOf<Self>, String>;

    /// Commits only the first `applied` envelopes of `batch`, as a process that
    /// died mid-batch would have left them.
    ///
    /// This is the one thing the suite cannot do through [`WorkflowExecutor`],
    /// and the check that matters most needs it. Implement it against your own
    /// tables, with the same code path `execute` uses, stopping after `applied`
    /// envelopes have committed.
    ///
    /// **If a partial batch is impossible in your executor** — every envelope
    /// commits inside one database transaction, so a crash rolls all of them
    /// back — then implement this by executing the *whole* batch and returning
    /// `Ok(())`. That is the honest translation: the state a crash can leave
    /// behind is either "none of it", which needs no resume, or "all of it",
    /// which the resume must recognise as a replay. The checks then prove that
    /// stronger property instead of a weaker one.
    ///
    /// # Errors
    /// * A description of why the prefix could not be committed. `applied` is
    ///   always at least one and never larger than the batch, so a bounds
    ///   complaint is a bug in the suite, not in the implementation.
    async fn interrupt_after(
        &self,
        seeded: &SeedOf<Self>,
        batch: &BatchOf<Self>,
        applied: usize,
    ) -> Result<(), String>;
}

/// How many checks [`run_all`] runs.
///
/// Exported so a caller asserting full coverage cannot drift from the suite: a
/// check added here changes this constant, while an assertion written against a
/// literal would keep passing while covering less.
pub const CHECK_COUNT: usize = 7;

/// Runs every check against a fresh executor each and reports.
///
/// Never panics and never stops early.
pub async fn run_all<F: ExecutorFactory>(factory: &F) -> ConformanceReport {
    let mut outcomes = Vec::with_capacity(CHECK_COUNT);
    macro_rules! run {
        ($($check:path),* $(,)?) => {
            $(
                outcomes.push(CheckOutcome {
                    check: stringify!($check).rsplit("::").next().unwrap_or(stringify!($check)),
                    failure: $check(factory).await.err(),
                });
            )*
        };
    }
    run!(
        check_stale_expected_revision_is_a_conflict,
        check_commit_reports_the_revision_it_reached,
        check_repeated_key_replays_the_outcome,
        check_repeated_key_with_another_command_is_refused,
        check_per_case_batch_is_all_or_nothing,
        check_interrupted_batch_resumes_to_the_same_state,
        check_refused_command_leaves_the_case_byte_identical,
    );
    ConformanceReport { outcomes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_renders_and_keeps_the_first_failure() {
        let report = ConformanceReport {
            outcomes: vec![
                CheckOutcome {
                    check: "check_a",
                    failure: None,
                },
                CheckOutcome {
                    check: "check_b",
                    failure: Some(ConformanceFailure::new("check_b", "expected 1, got 2")),
                },
            ],
        };
        assert!(!report.passed());
        assert_eq!(report.failures().count(), 1);
        assert_eq!(report.failed_checks().collect::<Vec<_>>(), vec!["check_b"]);
        let rendered = report.to_string();
        assert!(rendered.contains("1 of 2 executor conformance checks passed"));
        assert!(rendered.contains("FAIL  check_b: expected 1, got 2"));
        assert_eq!(report.into_result().unwrap_err().check, "check_b");
    }

    #[test]
    fn an_empty_report_passes() {
        let report = ConformanceReport {
            outcomes: Vec::new(),
        };
        assert!(report.passed());
        assert!(report.into_result().is_ok());
    }
}
