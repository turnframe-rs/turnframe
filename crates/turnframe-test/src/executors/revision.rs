//! Checks about the compare-and-set on the case revision (I13).

use turnframe_core::error::ExecutionError;
use turnframe_core::flow::WorkflowExecutor;

use super::fixtures::{
    batch, ensure, ensure_commit, ensure_eq, ensure_refused, ensure_unchanged, seed, snapshot,
};
use super::{ConformanceFailure, ExecutorFactory};

/// A batch planned against a revision that is no longer current is refused, and
/// the case is left exactly as it was (I13).
///
/// This is the rule that makes a card safe. An interaction is bound to the
/// revision the case had when the card was rendered; if an executor accepted a
/// write planned against a superseded revision, every card in the system would
/// be a promise to apply an answer to content the user never saw.
///
/// The check exercises both directions, because they catch different mistakes:
///
/// * a revision **behind** the current one is what a stale card produces, and
///   an executor that compares with `>=` accepts it;
/// * a revision **ahead** of the current one is what a corrupted or
///   hand-written plan produces, and an executor that compares with `<=`
///   accepts that instead. The rule is equality, not an ordering.
///
/// In both cases the refusal has to be a
/// [`RevisionConflict`](ExecutionError::RevisionConflict) carrying the revision
/// actually found, since that is the number the caller re-plans against, and
/// the stored case must be byte-identical afterwards.
pub async fn check_stale_expected_revision_is_a_conflict<F: ExecutorFactory>(
    factory: &F,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_stale_expected_revision_is_a_conflict";
    let seeded = seed(CHECK, factory).await?;
    let seeded_at = seeded.case_ref.expected_revision;

    let first = batch::<F>(
        CHECK,
        &seeded,
        "revision.first",
        &seeded.case_ref,
        std::slice::from_ref(&seeded.first),
    )?;
    ensure_commit(
        CHECK,
        "the first batch, planned against the current revision",
        seeded.executor.execute(first).await,
    )?;
    let committed = snapshot(CHECK, "after the first batch", &seeded).await?;
    ensure(
        CHECK,
        committed.1 != seeded_at,
        format!("a committed batch must move the case off revision {seeded_at}"),
    )?;

    // Behind: the very shape a card rendered one turn ago produces.
    let stale = batch::<F>(
        CHECK,
        &seeded,
        "revision.stale",
        &seeded.case_ref,
        std::slice::from_ref(&seeded.second),
    )?;
    let refused = ensure_refused(
        CHECK,
        "a batch planned against the superseded revision",
        seeded.executor.execute(stale).await,
    )?;
    match refused {
        ExecutionError::RevisionConflict(conflict) => {
            ensure_eq(
                CHECK,
                "the revision the conflict reports as current",
                &conflict.current_revision,
                &committed.1,
            )?;
            ensure_eq(
                CHECK,
                "the revision the conflict says was expected",
                &conflict.expected.expected_revision,
                &seeded_at,
            )?;
        }
        other => {
            return Err(ConformanceFailure::new(
                CHECK,
                format!("a stale revision must be a RevisionConflict, got {other:?}"),
            ));
        }
    }
    ensure_unchanged(
        CHECK,
        "after the stale batch was refused",
        &seeded,
        &committed,
    )
    .await?;

    // Ahead: a revision the case has never reached is equally not the current
    // one, and equally not a licence to write.
    let ahead_ref = seeded.case_ref.with_revision(committed.1.next());
    let ahead = batch::<F>(
        CHECK,
        &seeded,
        "revision.ahead",
        &ahead_ref,
        std::slice::from_ref(&seeded.second),
    )?;
    let refused = ensure_refused(
        CHECK,
        "a batch planned against a revision the case never reached",
        seeded.executor.execute(ahead).await,
    )?;
    ensure(
        CHECK,
        matches!(refused, ExecutionError::RevisionConflict(_)),
        format!("a revision ahead of the current one must be a RevisionConflict, got {refused:?}"),
    )?;
    ensure_unchanged(
        CHECK,
        "after the ahead batch was refused",
        &seeded,
        &committed,
    )
    .await
}

/// The revision a commit reports is the revision the case actually reached, and
/// the one the next batch must be planned against.
///
/// A commit is the caller's only chance to learn where the case landed: the
/// runtime binds the next card to that number, and the next batch carries it as
/// its expected revision. An executor that reports the revision it *started*
/// from, or that reports a number it never stored, hands the caller a value
/// that is guaranteed to conflict — and the symptom appears one turn later, in
/// code that did nothing wrong.
///
/// So the check reads the number back two ways: `load` must agree with the
/// commit, and a batch planned against the reported revision must be accepted.
pub async fn check_commit_reports_the_revision_it_reached<F: ExecutorFactory>(
    factory: &F,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_commit_reports_the_revision_it_reached";
    let seeded = seed(CHECK, factory).await?;
    let before = snapshot(CHECK, "before anything ran", &seeded).await?;
    ensure_eq(
        CHECK,
        "the revision the factory says it seeded",
        &before.1,
        &seeded.case_ref.expected_revision,
    )?;

    let first = batch::<F>(
        CHECK,
        &seeded,
        "reported.first",
        &seeded.case_ref,
        std::slice::from_ref(&seeded.first),
    )?;
    let commit = ensure_commit(
        CHECK,
        "the first batch",
        seeded.executor.execute(first).await,
    )?;
    ensure(
        CHECK,
        commit.new_revision != before.1,
        format!(
            "a batch that committed must report a revision other than {}, reported {}",
            before.1, commit.new_revision
        ),
    )?;
    let loaded = snapshot(CHECK, "after the first batch", &seeded).await?;
    ensure_eq(
        CHECK,
        "the revision `load` reports against the one the commit reported",
        &loaded.1,
        &commit.new_revision,
    )?;

    // The reported revision is not decoration: it is what the next batch is
    // planned against, so plan one and require it to be accepted.
    let next_ref = seeded.case_ref.with_revision(commit.new_revision);
    let second = batch::<F>(
        CHECK,
        &seeded,
        "reported.second",
        &next_ref,
        std::slice::from_ref(&seeded.second),
    )?;
    let second_commit = ensure_commit(
        CHECK,
        "a batch planned against the revision the first commit reported",
        seeded.executor.execute(second).await,
    )?;
    let after = snapshot(CHECK, "after the second batch", &seeded).await?;
    ensure_eq(
        CHECK,
        "the revision `load` reports after the second batch",
        &after.1,
        &second_commit.new_revision,
    )
}
