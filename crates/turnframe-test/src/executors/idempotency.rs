//! Checks about the idempotency key (I14).

use turnframe_core::error::ExecutionError;
use turnframe_core::flow::WorkflowExecutor;

use super::fixtures::{
    batch, ensure, ensure_commit, ensure_eq, ensure_refused, ensure_unchanged, seed, snapshot,
};
use super::{ConformanceFailure, ExecutorFactory};

/// A key the executor has already committed returns the original outcome and
/// repeats no effect (I14).
///
/// The key is derived from the turn, the case reference, the origin and the
/// command, so a turn replayed after a crash — the client retried, the process
/// restarted, the queue redelivered — arrives with exactly the keys that
/// already committed. An executor that does not recognise them applies the
/// user's turn twice, and the second application is invisible in every log that
/// only records requests.
///
/// The check runs the same batch three times. Twice is enough to catch an
/// executor that never remembers; the third run catches the one that remembers
/// only the previous call, which is a real shape when the memory is a
/// last-write-wins cache rather than a journal.
pub async fn check_repeated_key_replays_the_outcome<F: ExecutorFactory>(
    factory: &F,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_repeated_key_replays_the_outcome";
    let seeded = seed(CHECK, factory).await?;
    let work = batch::<F>(
        CHECK,
        &seeded,
        "idempotency.batch",
        &seeded.case_ref,
        &[seeded.first.clone(), seeded.second.clone()],
    )?;

    let first = ensure_commit(
        CHECK,
        "the batch, executed for the first time",
        seeded.executor.execute(work.clone()).await,
    )?;
    ensure(
        CHECK,
        !first.idempotency_replay,
        "a batch whose keys the executor has never seen must not be reported as a replay",
    )?;
    let committed = snapshot(CHECK, "after the batch committed", &seeded).await?;

    for attempt in ["the second time", "the third time"] {
        let again = ensure_commit(
            CHECK,
            &format!("the same batch, executed {attempt}"),
            seeded.executor.execute(work.clone()).await,
        )?;
        ensure(
            CHECK,
            again.idempotency_replay,
            format!(
                "the batch executed {attempt} came back without `idempotency_replay`: \
                 the caller cannot tell a fresh commit from a replay"
            ),
        )?;
        ensure_eq(
            CHECK,
            &format!("the revision reported {attempt}"),
            &again.new_revision,
            &first.new_revision,
        )?;
        ensure_eq(
            CHECK,
            &format!("the number of events reported {attempt}"),
            &again.events.len(),
            &first.events.len(),
        )?;
        ensure_unchanged(
            CHECK,
            &format!("after the batch was executed {attempt}"),
            &seeded,
            &committed,
        )
        .await?;
    }
    Ok(())
}

/// A key that arrives with a different command is a mismatch, never a replay
/// (I14).
///
/// This is the case where replaying would be worse than failing. The key says
/// "this exact work already happened"; the payload says it did not. Returning
/// the memorized outcome would report success for a command that never ran, and
/// executing the new payload would let a caller overwrite a committed effect by
/// reusing its key. The only safe answer is to refuse and name the envelope.
///
/// The refusal has to be stable, so the check makes it twice: an executor that
/// satisfies the rule by discarding the key it just refused would execute the
/// forged payload for real on the retry.
pub async fn check_repeated_key_with_another_command_is_refused<F: ExecutorFactory>(
    factory: &F,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_repeated_key_with_another_command_is_refused";
    let seeded = seed(CHECK, factory).await?;

    let original = batch::<F>(
        CHECK,
        &seeded,
        "idempotency.mismatch",
        &seeded.case_ref,
        std::slice::from_ref(&seeded.first),
    )?;
    ensure_commit(
        CHECK,
        "the original batch",
        seeded.executor.execute(original.clone()).await,
    )?;
    let committed = snapshot(CHECK, "after the original batch", &seeded).await?;

    // The same envelope identity — key, command id, turn, case, origin — with
    // another command inside it.
    let mut forged = original.clone();
    let target = forged
        .envelopes
        .first()
        .map(|e| e.command_id)
        .ok_or_else(|| {
            ConformanceFailure::new(CHECK, "the suite built a batch with no envelopes")
        })?;
    let first_json = serde_json::to_value(&seeded.first).ok();
    let second_json = serde_json::to_value(&seeded.second).ok();
    ensure(
        CHECK,
        first_json.is_some() && first_json != second_json,
        "the factory's `first` and `second` commands must serialize differently, \
         otherwise reusing a key with `second` is not a different command at all",
    )?;
    if let Some(envelope) = forged.envelopes.first_mut() {
        envelope.command = seeded.second.clone();
    }

    // Twice, because an executor that satisfies the rule by forgetting the key
    // it just refused has traded one defect for another: the second attempt
    // would then execute the forged payload for real.
    for attempt in ["the first time", "the second time"] {
        let refused = ensure_refused(
            CHECK,
            &format!("a known key carrying a different command, {attempt}"),
            seeded.executor.execute(forged.clone()).await,
        )?;
        match refused {
            ExecutionError::IdempotencyMismatch { command_id } => {
                ensure_eq(
                    CHECK,
                    &format!("the envelope the mismatch names {attempt}"),
                    &command_id,
                    &target,
                )?;
            }
            other => {
                return Err(ConformanceFailure::new(
                    CHECK,
                    format!(
                        "a key reused with another command must be an IdempotencyMismatch \
                         {attempt}, got {other:?}"
                    ),
                ));
            }
        }
        ensure_unchanged(
            CHECK,
            &format!("after the mismatch was refused {attempt}"),
            &seeded,
            &committed,
        )
        .await?;
    }
    Ok(())
}
