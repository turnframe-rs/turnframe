//! Checks for [`OutboxStore`](crate::outbox::OutboxStore).

use turnframe_core::event::OutboxStatus;
use turnframe_core::ids::{CommandId, OutboxId};

use super::ConformanceFailure;
use super::fixtures::{
    at, ensure, ensure_code, ensure_eq, ensure_error, ensure_ok, epoch, outbox_entry,
};
use crate::error::{StoreError, codes};
use crate::stores::Stores;

/// A claimed row belongs to one dispatcher until it is settled or rescheduled
/// (spec §16.4).
///
/// Two dispatchers claiming the same row would send the same external request
/// twice, which is precisely the failure the outbox exists to prevent. The
/// check also covers the reaper that recovers rows a dispatcher died holding,
/// and the `(destination, idempotency_key)` uniqueness that stops the same
/// external action being enqueued twice.
pub async fn check_outbox_claim_exclusivity_and_reschedule(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_outbox_claim_exclusivity_and_reschedule";
    let outbox = stores.outbox();
    let command = CommandId::new();
    let first = OutboxId::new();
    let second = OutboxId::new();

    ensure_ok(
        CHECK,
        "enqueueing the first row",
        outbox
            .enqueue(outbox_entry(first, command, "first", epoch()))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "enqueueing the second row",
        outbox
            .enqueue(outbox_entry(second, command, "second", at(1)))
            .await,
    )?;
    ensure_error(
        CHECK,
        "enqueueing the same destination and key twice",
        outbox
            .enqueue(outbox_entry(OutboxId::new(), command, "first", at(2)))
            .await,
        &StoreError::Conflict,
    )?;
    let mut already_dispatching = outbox_entry(OutboxId::new(), command, "third", at(2));
    already_dispatching.status = OutboxStatus::Dispatching;
    ensure_code(
        CHECK,
        "enqueueing a row that is not Pending",
        outbox.enqueue(already_dispatching).await,
        codes::INVALID_RECORD,
    )?;

    let listed = ensure_ok(
        CHECK,
        "listing the rows of the command",
        outbox.list_for_command(&command).await,
    )?;
    ensure_eq(
        CHECK,
        "rows of the command, oldest first",
        &listed
            .iter()
            .map(|record| record.entry.outbox_id)
            .collect::<Vec<_>>(),
        &vec![first, second],
    )?;

    let claimed = ensure_ok(
        CHECK,
        "claiming the due rows",
        outbox.claim_due(at(10), 10, "worker-a").await,
    )?;
    ensure_eq(
        CHECK,
        "rows claimed by the first worker",
        &claimed
            .iter()
            .map(|entry| entry.outbox_id)
            .collect::<Vec<_>>(),
        &vec![first, second],
    )?;
    ensure_eq(
        CHECK,
        "status of a claimed row",
        &claimed[0].status,
        &OutboxStatus::Dispatching,
    )?;
    ensure_eq(
        CHECK,
        "attempt count after the first claim",
        &claimed[0].attempt_count,
        &1,
    )?;

    let contended = ensure_ok(
        CHECK,
        "claiming again from another worker",
        outbox.claim_due(at(11), 10, "worker-b").await,
    )?;
    ensure(
        CHECK,
        contended.is_empty(),
        "a claimed row must not be handed to a second worker",
    )?;

    // Rescheduling releases the claim and puts the row back in the queue.
    ensure_ok(
        CHECK,
        "rescheduling the first row",
        outbox.reschedule(&first, at(20)).await,
    )?;
    let too_early = ensure_ok(
        CHECK,
        "claiming before the row is due",
        outbox.claim_due(at(15), 10, "worker-b").await,
    )?;
    ensure(
        CHECK,
        too_early.is_empty(),
        "a rescheduled row must stay invisible until it is due",
    )?;
    let reclaimed = ensure_ok(
        CHECK,
        "claiming once the row is due",
        outbox.claim_due(at(20), 10, "worker-b").await,
    )?;
    ensure_eq(
        CHECK,
        "the rescheduled row is claimable again",
        &reclaimed
            .iter()
            .map(|entry| entry.outbox_id)
            .collect::<Vec<_>>(),
        &vec![first],
    )?;
    ensure_eq(
        CHECK,
        "attempt count after the second claim",
        &reclaimed[0].attempt_count,
        &2,
    )?;

    // Settling a row takes it out of the queue for good.
    ensure_ok(
        CHECK,
        "completing the second row",
        outbox.mark_completed(&second).await,
    )?;
    ensure_ok(
        CHECK,
        "completing it again",
        outbox.mark_completed(&second).await,
    )?;
    let completed = ensure_ok(
        CHECK,
        "reading the completed row",
        outbox.get(&second).await,
    )?;
    ensure_eq(
        CHECK,
        "status of the completed row",
        &completed.entry.status,
        &OutboxStatus::Completed,
    )?;
    ensure(
        CHECK,
        completed.claim.is_none(),
        "a settled row must not keep its claim",
    )?;
    ensure_error(
        CHECK,
        "rescheduling a settled row",
        outbox.reschedule(&second, at(30)).await,
        &StoreError::Conflict,
    )?;

    // An unknown outcome is a state of its own: it must be reconciled, never
    // blindly retried (I15, spec §16.5).
    ensure_ok(
        CHECK,
        "recording an unknown outcome",
        outbox
            .mark_outcome_unknown(&first, Some("remote-123".to_owned()))
            .await,
    )?;
    let unknown = ensure_ok(CHECK, "reading the row", outbox.get(&first).await)?;
    ensure_eq(
        CHECK,
        "status after an unknown outcome",
        &unknown.entry.status,
        &OutboxStatus::OutcomeUnknown,
    )?;
    ensure_eq(
        CHECK,
        "the remote reference kept for reconciliation",
        &unknown.remote_ref,
        &Some("remote-123".to_owned()),
    )?;
    let after_unknown = ensure_ok(
        CHECK,
        "claiming a row whose outcome is unknown",
        outbox.claim_due(at(40), 10, "worker-c").await,
    )?;
    ensure(
        CHECK,
        after_unknown.is_empty(),
        "a row with an unknown outcome must not be dispatched again by a sweep",
    )?;
    ensure_ok(
        CHECK,
        "reconciling the unknown outcome",
        outbox.mark_completed(&first).await,
    )?;

    // A dispatcher that dies holding a claim must not block the row forever.
    let third = OutboxId::new();
    ensure_ok(
        CHECK,
        "enqueueing the row to abandon",
        outbox
            .enqueue(outbox_entry(third, command, "abandoned", at(50)))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "claiming the row to abandon",
        outbox.claim_due(at(60), 10, "worker-d").await,
    )?;
    let untouched = ensure_ok(
        CHECK,
        "reaping claims younger than the cut-off",
        outbox.release_expired_claims(at(60)).await,
    )?;
    ensure(
        CHECK,
        untouched.is_empty(),
        "a fresh claim must not be reaped",
    )?;
    let released = ensure_ok(
        CHECK,
        "reaping stale claims",
        outbox.release_expired_claims(at(120)).await,
    )?;
    ensure_eq(CHECK, "reaped rows", &released, &vec![third])?;
    let recovered = ensure_ok(CHECK, "reading the reaped row", outbox.get(&third).await)?;
    ensure_eq(
        CHECK,
        "status of the reaped row",
        &recovered.entry.status,
        &OutboxStatus::Pending,
    )?;
    ensure(
        CHECK,
        recovered.claim.is_none(),
        "a reaped row must have no claim",
    )?;

    // A definite failure ends the row; a failure with a retry time requeues it.
    ensure_ok(
        CHECK,
        "claiming the reaped row",
        outbox.claim_due(at(130), 10, "worker-e").await,
    )?;
    ensure_ok(
        CHECK,
        "failing it with a retry time",
        outbox
            .mark_failed(&third, "remote_busy".to_owned(), Some(at(140)))
            .await,
    )?;
    let retrying = ensure_ok(CHECK, "reading the retrying row", outbox.get(&third).await)?;
    ensure_eq(
        CHECK,
        "status after a retryable failure",
        &retrying.entry.status,
        &OutboxStatus::Pending,
    )?;
    ensure_eq(
        CHECK,
        "the failure code recorded on the row",
        &retrying.last_failure,
        &Some("remote_busy".to_owned()),
    )?;
    ensure_ok(
        CHECK,
        "claiming it once more",
        outbox.claim_due(at(140), 10, "worker-e").await,
    )?;
    ensure_ok(
        CHECK,
        "failing it definitively",
        outbox
            .mark_failed(&third, "remote_refused".to_owned(), None)
            .await,
    )?;
    let failed = ensure_ok(CHECK, "reading the failed row", outbox.get(&third).await)?;
    ensure_eq(
        CHECK,
        "status after a definite failure",
        &failed.entry.status,
        &OutboxStatus::Failed,
    )?;

    ensure_error(
        CHECK,
        "reading a row that does not exist",
        outbox.get(&OutboxId::new()).await,
        &StoreError::NotFound,
    )
}

/// A refused settlement writes nothing at all (spec §16.4).
///
/// `Conflict` has to mean the row is untouched. A store that records the
/// failure reason and only then notices the row was already settled leaves a
/// terminal entry carrying the text of an attempt the same store reports as
/// never having happened, and an operator reading that row cannot tell which
/// statement is true. The reference in-memory store had exactly this defect
/// and no check caught it, which is why this one exists.
pub async fn check_refused_settlement_writes_nothing(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_refused_settlement_writes_nothing";
    let outbox = stores.outbox();
    let command = CommandId::new();
    let row = OutboxId::new();

    ensure_ok(
        CHECK,
        "enqueueing the row",
        outbox
            .enqueue(outbox_entry(row, command, "refused-settlement-1", epoch()))
            .await,
    )?;
    ensure_ok(
        CHECK,
        "claiming it",
        outbox.claim_due(at(10), 10, "worker-a").await,
    )?;
    ensure_ok(
        CHECK,
        "settling it definitively",
        outbox
            .mark_failed(&row, "remote_refused".to_owned(), None)
            .await,
    )?;
    let settled = ensure_ok(CHECK, "reading the settled row", outbox.get(&row).await)?;

    // The row is terminal, so asking to retry it must be refused.
    ensure_error(
        CHECK,
        "asking a terminal row to retry",
        outbox
            .mark_failed(
                &row,
                "a reason that must not be stored".to_owned(),
                Some(at(60)),
            )
            .await,
        &StoreError::Conflict,
    )?;

    let after = ensure_ok(CHECK, "reading the row again", outbox.get(&row).await)?;
    ensure_eq(
        CHECK,
        "status after a refused settlement",
        &after.entry.status,
        &settled.entry.status,
    )?;
    ensure_eq(
        CHECK,
        "failure reason after a refused settlement",
        &after.last_failure,
        &settled.last_failure,
    )?;
    ensure_eq(
        CHECK,
        "next attempt after a refused settlement",
        &after.entry.next_attempt_at,
        &settled.entry.next_attempt_at,
    )?;
    ensure(
        CHECK,
        after == settled,
        "the refused call left the whole row untouched",
    )
}
