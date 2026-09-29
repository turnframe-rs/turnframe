//! The executor conformance suite, run against the kit's own executor — and
//! against two executors broken on purpose.
//!
//! A suite that only ever runs against a correct implementation proves that it
//! compiles, not that it checks anything. The two wrappers below are the
//! control group: each breaks exactly one rule of the executor contract, and
//! each test asserts that exactly one named check notices. The remaining checks
//! were falsified the same way by mutating
//! [`InMemoryExecutor`](turnframe_test::workflows::InMemoryExecutor) itself,
//! which is where the rest of the defects live.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use turnframe_core::case::Versioned;
use turnframe_core::command::CommandBatch;
use turnframe_core::error::{ExecutionError, RevisionConflict, StoreError};
use turnframe_core::event::Commit;
use turnframe_core::flow::WorkflowExecutor;
use turnframe_core::ids::{AccountId, CaseId, CaseRevision};
use turnframe_core::turn::ActorContext;
use turnframe_test::executors::{
    BatchOf, CHECK_COUNT, ExecutorFactory, SeedOf, SeededCase, run_all,
};
use turnframe_test::workflows::trip::{
    TripCommand, TripEvent, TripExecutor, TripState, TripWorkflow, conformance_case,
};

#[tokio::test]
async fn the_in_memory_executor_passes_the_whole_suite() {
    let report = run_all(&conformance_case()).await;
    assert!(report.passed(), "{report}");
}

#[tokio::test]
async fn the_suite_runs_every_check_it_declares() {
    let report = run_all(&conformance_case()).await;

    assert_eq!(
        report.outcomes.len(),
        CHECK_COUNT,
        "the runner and the exported count must not drift: {report}"
    );
    let names: BTreeSet<&str> = report
        .outcomes
        .iter()
        .map(|outcome| outcome.check)
        .collect();
    assert_eq!(names.len(), CHECK_COUNT, "a check name appeared twice");
    assert!(
        names.iter().all(|name| name.starts_with("check_")),
        "a check reported an unexpected name: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// Control group: two executors that each break exactly one rule
// ---------------------------------------------------------------------------

/// An executor that reports a revision it never reached.
///
/// Everything it stores is correct; only the number it hands back to the caller
/// is wrong. That is the shape of the defect: the case is fine, and the turn
/// *after* this one conflicts for no visible reason.
struct MisreportsRevision(TripExecutor);

#[async_trait::async_trait]
impl WorkflowExecutor<TripWorkflow> for MisreportsRevision {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<TripState>>, StoreError> {
        self.0.load(account, case_id).await
    }

    async fn execute(
        &self,
        batch: CommandBatch<TripCommand>,
    ) -> Result<Commit<TripState, TripEvent>, ExecutionError> {
        let mut commit = self.0.execute(batch).await?;
        commit.new_revision = CaseRevision::ZERO;
        Ok(commit)
    }
}

/// An executor that recognises a batch only when *all* of it already committed,
/// and re-checks the revision otherwise.
///
/// This is the partial-batch defect in its most plausible form. The idempotency
/// memory is keyed on the batch rather than on the envelope, so a batch that
/// half-committed is "not done"; the revision is then re-checked against the
/// one the executor's own half-commit produced, and the resume is reported as a
/// conflict. Every individual rule looks satisfied — stale revisions are
/// refused, repeated keys replay, refusals write nothing — and a turn
/// interrupted by a restart can never be finished.
struct ConflictsOnResume(TripExecutor);

#[async_trait::async_trait]
impl WorkflowExecutor<TripWorkflow> for ConflictsOnResume {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<TripState>>, StoreError> {
        self.0.load(account, case_id).await
    }

    async fn execute(
        &self,
        batch: CommandBatch<TripCommand>,
    ) -> Result<Commit<TripState, TripEvent>, ExecutionError> {
        let whole_batch_is_known = batch
            .envelopes
            .iter()
            .all(|envelope| self.0.has_executed(&envelope.idempotency_key));
        if let (false, Some(first)) = (whole_batch_is_known, batch.envelopes.first()) {
            let current = self
                .0
                .revision_of(first.account_id(), &first.case_ref.case_id);
            if current != first.case_ref.expected_revision {
                return Err(ExecutionError::RevisionConflict(RevisionConflict {
                    expected: first.case_ref.clone(),
                    current_revision: current,
                }));
            }
        }
        self.0.execute(batch).await
    }
}

/// Builds a factory around a wrapper of the kit's trip executor.
struct BrokenCase<W> {
    wrap: fn(TripExecutor) -> W,
}

#[async_trait::async_trait]
impl<W> ExecutorFactory for BrokenCase<W>
where
    W: WorkflowExecutor<TripWorkflow> + AsRef<TripExecutor> + Send + Sync,
{
    type Workflow = TripWorkflow;
    type Executor = W;

    async fn seed(&self) -> Result<SeedOf<Self>, String> {
        let sound = conformance_case().seed().await?;
        let SeededCase {
            executor,
            actor,
            case_ref,
            first,
            second,
            refused,
        } = sound;
        Ok(SeededCase::new(
            (self.wrap)(executor),
            actor,
            case_ref,
            first,
            second,
            refused,
        ))
    }

    async fn interrupt_after(
        &self,
        seeded: &SeedOf<Self>,
        batch: &BatchOf<Self>,
        applied: usize,
    ) -> Result<(), String> {
        seeded
            .executor
            .as_ref()
            .execute_prefix(batch, applied)
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    }
}

impl AsRef<TripExecutor> for MisreportsRevision {
    fn as_ref(&self) -> &TripExecutor {
        &self.0
    }
}

impl AsRef<TripExecutor> for ConflictsOnResume {
    fn as_ref(&self) -> &TripExecutor {
        &self.0
    }
}

/// The suite must name the broken rule, and only the broken rule.
async fn only_this_check_fails<F: ExecutorFactory>(factory: &F, expected: &str) {
    let report = run_all(factory).await;
    let failed: Vec<&str> = report.failed_checks().collect();
    assert_eq!(failed, vec![expected], "{report}");
    assert_eq!(
        report.outcomes.len(),
        CHECK_COUNT,
        "the runner stopped early instead of reporting every check: {report}"
    );
}

#[tokio::test]
async fn an_executor_that_misreports_its_revision_fails_exactly_one_check() {
    only_this_check_fails(
        &BrokenCase {
            wrap: MisreportsRevision,
        },
        "check_commit_reports_the_revision_it_reached",
    )
    .await;
}

#[tokio::test]
async fn an_executor_that_cannot_resume_a_half_committed_batch_fails_exactly_one_check() {
    only_this_check_fails(
        &BrokenCase {
            wrap: ConflictsOnResume,
        },
        "check_interrupted_batch_resumes_to_the_same_state",
    )
    .await;
}

#[tokio::test]
async fn a_factory_that_cannot_seed_fails_every_check_without_panicking() {
    struct Unbuildable;

    #[async_trait::async_trait]
    impl ExecutorFactory for Unbuildable {
        type Workflow = TripWorkflow;
        type Executor = TripExecutor;

        async fn seed(&self) -> Result<SeedOf<Self>, String> {
            Err("no database".to_owned())
        }

        async fn interrupt_after(
            &self,
            _seeded: &SeedOf<Self>,
            _batch: &BatchOf<Self>,
            _applied: usize,
        ) -> Result<(), String> {
            Err("no database".to_owned())
        }
    }

    let report = run_all(&Unbuildable).await;
    assert_eq!(report.failures().count(), CHECK_COUNT, "{report}");
    assert!(
        report
            .failures()
            .all(|failure| failure.detail.contains("no database")),
        "the seed failure must reach the report verbatim: {report}"
    );
}

/// The seeded case really is the one the trip sample describes, so a suite
/// failure is about the executor and not about a fixture that drifted.
#[tokio::test]
async fn the_conformance_case_is_seeded_where_it_says_it_is() {
    let case = conformance_case();
    let seeded = case.seed().await.expect("the in-memory case always seeds");
    let loaded = seeded
        .executor
        .load(&seeded.actor.account_id, &seeded.case_ref.case_id)
        .await
        .expect("the seeded case loads");

    assert_eq!(loaded.revision, seeded.case_ref.expected_revision);
    assert!(loaded.value.is_some(), "the factory seeds a complete draft");
    assert_eq!(
        seeded.actor,
        ActorContext::new(
            turnframe_test::executors::CONFORMANCE_ACCOUNT,
            turnframe_test::executors::CONFORMANCE_USER,
        )
    );
}
