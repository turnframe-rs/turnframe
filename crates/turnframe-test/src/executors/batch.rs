//! Checks about what a batch commits, and what it leaves behind when it does
//! not (spec §16.3, §23.1).

use turnframe_core::error::ExecutionError;
use turnframe_core::flow::WorkflowExecutor;
use turnframe_core::ids::CaseId;

use super::fixtures::{
    batch, ensure, ensure_commit, ensure_eq, ensure_refused, ensure_unchanged, seed, snapshot,
};
use super::{ConformanceFailure, ExecutorFactory};

/// A [`PerCase`](turnframe_core::command::AtomicityScope::PerCase) batch
/// commits entirely or not at all, and never spans two cases.
///
/// Both halves are the same promise read from two sides. The scope says the
/// envelopes share one revision and one fate: if the third of five is refused,
/// the first two must leave no trace, because the caller was told the batch
/// failed and will re-plan from the revision it still believes is current. An
/// executor that commits envelope by envelope satisfies every individual
/// command and breaks the batch.
///
/// The second half — a `PerCase` batch carrying envelopes for two different
/// cases — has to be refused *before* anything is written, since no single
/// case revision can own the result.
pub async fn check_per_case_batch_is_all_or_nothing<F: ExecutorFactory>(
    factory: &F,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_per_case_batch_is_all_or_nothing";
    let seeded = seed(CHECK, factory).await?;
    let before = snapshot(CHECK, "before the batch ran", &seeded).await?;

    // A batch whose first envelope applies and whose second one does not.
    let doomed = batch::<F>(
        CHECK,
        &seeded,
        "atomicity.doomed",
        &seeded.case_ref,
        &[seeded.first.clone(), seeded.refused.clone()],
    )?;
    let refused = ensure_refused(
        CHECK,
        "a batch whose second envelope the domain refuses",
        seeded.executor.execute(doomed.clone()).await,
    )?;
    ensure(
        CHECK,
        matches!(refused, ExecutionError::Rejected(_)),
        format!("the batch had to fail on the domain's refusal, failed with {refused:?}"),
    )?;
    ensure_unchanged(
        CHECK,
        "after the batch was refused: the first envelope must have left no trace",
        &seeded,
        &before,
    )
    .await?;

    // Nothing of a failed batch may be remembered as done, so running it again
    // has to reach the same refusal rather than resuming a phantom prefix.
    let refused_again = ensure_refused(
        CHECK,
        "the same doomed batch, executed again",
        seeded.executor.execute(doomed).await,
    )?;
    ensure(
        CHECK,
        matches!(refused_again, ExecutionError::Rejected(_)),
        format!("the second attempt at the doomed batch failed with {refused_again:?}"),
    )?;
    ensure_unchanged(
        CHECK,
        "after the doomed batch was retried",
        &seeded,
        &before,
    )
    .await?;

    // A per-case batch that names two cases cannot have one revision.
    let mut mixed = batch::<F>(
        CHECK,
        &seeded,
        "atomicity.mixed",
        &seeded.case_ref,
        &[seeded.first.clone(), seeded.second.clone()],
    )?;
    let elsewhere = seeded.case_ref.clone();
    let other_case = CaseId::from(format!("{}-conformance-other", elsewhere.case_id.as_str()));
    if let Some(envelope) = mixed.envelopes.get_mut(1) {
        envelope.case_ref.case_id = other_case;
    }
    let refused = ensure_refused(
        CHECK,
        "a per-case batch spanning two cases",
        seeded.executor.execute(mixed).await,
    )?;
    ensure(
        CHECK,
        matches!(refused, ExecutionError::ScopeViolation),
        format!("a per-case batch over two cases must be a ScopeViolation, got {refused:?}"),
    )?;
    ensure_unchanged(CHECK, "after the mixed batch was refused", &seeded, &before).await
}

/// A batch that half-committed resumes to exactly the state an uninterrupted
/// one reaches (spec §23.1).
///
/// **This is the check the suite exists for.** Every other rule here is one an
/// implementer plausibly writes a test for on their own; this one needs a
/// half-committed batch, which needs deliberate effort to produce, which is why
/// [`ExecutorFactory::interrupt_after`] is part of the contract.
///
/// Two defects hide here, and they look nothing alike:
///
/// * The executor remembers idempotency per **batch** instead of per
///   **envelope**. A batch that half-committed then has only two possible
///   answers, "all of it" and "none of it": the first skips the commands that
///   never ran, the second runs the ones that already did.
/// * The executor re-checks `current_revision == expected_revision` on the
///   resume, sees the revision its own half-commit produced, and reports a
///   conflict. The user is told their turn collided with itself, and the case
///   stays half-written for ever because every retry conflicts the same way.
///
/// So the check runs the same batch two ways — interrupted after its first
/// envelope, and straight through on a second executor from the same factory —
/// and requires the two to end in the same place: the same revision, the same
/// state, the same number of events. It also requires the resumed commit to
/// carry `idempotency_replay`, because a caller rendering a receipt has to know
/// that part of what it is about to describe came out of the journal rather
/// than out of the domain.
///
/// If a partial batch is impossible in your executor, read
/// [`ExecutorFactory::interrupt_after`]: committing the whole batch is the
/// honest implementation, and this check then proves the stronger property.
pub async fn check_interrupted_batch_resumes_to_the_same_state<F: ExecutorFactory>(
    factory: &F,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_interrupted_batch_resumes_to_the_same_state";
    let interrupted = seed(CHECK, factory).await?;
    let uninterrupted = seed(CHECK, factory).await?;

    // The two runs are only comparable if the factory seeded them identically.
    ensure_eq(
        CHECK,
        "the case the two seeded executors hold",
        &interrupted.case_ref,
        &uninterrupted.case_ref,
    )?;
    ensure_eq(
        CHECK,
        "the account the two seeded executors run as",
        &interrupted.actor.account_id,
        &uninterrupted.actor.account_id,
    )?;
    let seeded_at = snapshot(CHECK, "the state the factory seeded", &interrupted).await?;
    ensure_unchanged(
        CHECK,
        "the second executor the factory built: `seed` must be deterministic",
        &uninterrupted,
        &seeded_at,
    )
    .await?;

    let work = batch::<F>(
        CHECK,
        &interrupted,
        "resume.batch",
        &interrupted.case_ref,
        &[interrupted.first.clone(), interrupted.second.clone()],
    )?;

    factory
        .interrupt_after(&interrupted, &work, 1)
        .await
        .map_err(|detail| {
            ConformanceFailure::new(
                CHECK,
                format!("the factory could not interrupt the batch: {detail}"),
            )
        })?;
    let half = snapshot(CHECK, "after the batch was interrupted", &interrupted).await?;
    ensure(
        CHECK,
        half.1 != seeded_at.1 || half.0 != seeded_at.0,
        "`interrupt_after` left the case exactly as it was: an implementation that \
         commits nothing makes this check vacuous rather than passing it",
    )?;

    let straight = ensure_commit(
        CHECK,
        "the same batch, executed straight through on a second executor",
        uninterrupted.executor.execute(work.clone()).await,
    )?;
    // The sharp one: resuming must not be mistaken for a conflict.
    let resumed = ensure_commit(
        CHECK,
        "the interrupted batch, resumed",
        interrupted.executor.execute(work).await,
    )?;

    ensure(
        CHECK,
        resumed.idempotency_replay,
        "the resumed commit did not report `idempotency_replay`, so a caller cannot \
         tell that part of it came back from the journal instead of from the domain",
    )?;
    ensure_eq(
        CHECK,
        "the revision a resumed batch reaches against the one an uninterrupted batch reaches",
        &resumed.new_revision,
        &straight.new_revision,
    )?;
    ensure_eq(
        CHECK,
        "the number of events a resumed batch reports against an uninterrupted one",
        &resumed.events.len(),
        &straight.events.len(),
    )?;

    let landed = snapshot(CHECK, "after the interrupted batch resumed", &uninterrupted).await?;
    ensure_unchanged(
        CHECK,
        "the resumed executor against the uninterrupted one",
        &interrupted,
        &landed,
    )
    .await
}

/// A command the domain refuses leaves the case byte-identical, and stays a
/// refusal when it is retried.
///
/// A refusal is a decision, not an accident: the domain looked at the state and
/// said no. Nothing may be written — not a partial field, not a revision bump,
/// not an idempotency entry that would turn the refusal into a remembered
/// outcome. An executor that bumps the revision on a refusal is the worst of
/// the three, because every card the user is holding silently becomes stale
/// while nothing at all has changed underneath it.
///
/// The check therefore ends by planning a *valid* batch against the same
/// revision it started with: the caller must not have to re-plan because
/// something was refused.
pub async fn check_refused_command_leaves_the_case_byte_identical<F: ExecutorFactory>(
    factory: &F,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_refused_command_leaves_the_case_byte_identical";
    let seeded = seed(CHECK, factory).await?;
    let before = snapshot(CHECK, "before the refused command", &seeded).await?;

    let doomed = batch::<F>(
        CHECK,
        &seeded,
        "refusal.batch",
        &seeded.case_ref,
        std::slice::from_ref(&seeded.refused),
    )?;
    for attempt in ["the first time", "the second time"] {
        let refused = ensure_refused(
            CHECK,
            &format!("the refused command, executed {attempt}"),
            seeded.executor.execute(doomed.clone()).await,
        )?;
        ensure(
            CHECK,
            matches!(refused, ExecutionError::Rejected(_)),
            format!(
                "the domain's refusal must surface as ExecutionError::Rejected {attempt}, \
                 got {refused:?}"
            ),
        )?;
        ensure_unchanged(
            CHECK,
            &format!("after the command was refused {attempt}"),
            &seeded,
            &before,
        )
        .await?;
    }

    // The revision the caller planned against is still the current one.
    let valid = batch::<F>(
        CHECK,
        &seeded,
        "refusal.after",
        &seeded.case_ref,
        std::slice::from_ref(&seeded.first),
    )?;
    ensure_commit(
        CHECK,
        "a valid batch planned against the revision the refusal did not consume",
        seeded.executor.execute(valid).await,
    )?;
    Ok(())
}
