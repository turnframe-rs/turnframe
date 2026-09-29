//! The in-memory executor's concurrency and recovery behaviour (I13, I14,
//! spec §23.1).
//!
//! The interesting case is the one a happy-path test never produces: a batch
//! that half-executed because the process died between two of its commands.
//! Recovery has to resume it — replay what committed, run what did not — and
//! land the case exactly where an uninterrupted run would have left it. An
//! executor that keyed its memory on the batch as a whole would have only two
//! answers for that batch, "all of it" or "none of it", and both are wrong.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use turnframe_core::case::CaseRef;
use turnframe_core::command::CommandBatch;
use turnframe_core::error::ExecutionError;
use turnframe_core::flow::{WorkflowDefinition, WorkflowExecutor};
use turnframe_core::ids::{CaseId, CaseRevision};
use turnframe_test::workflows::trip::{
    SAMPLE_NAME, TripCommand, TripExecutor, TripState, TripWorkflow, sample_travel_date,
    sample_traveler,
};

const CASE: &str = "trip-1";

fn case_ref(revision: u64) -> CaseRef {
    CaseRef::new("trip", CASE, CaseRevision(revision))
}

/// The two-command batch every test here interrupts.
fn editing_batch(revision: u64) -> CommandBatch<TripCommand> {
    support::batch(
        support::turn(2),
        &case_ref(revision),
        &support::confirmed_click(),
        vec![
            TripCommand::SetName {
                value: SAMPLE_NAME.to_owned(),
            },
            TripCommand::SetTravelDate {
                value: sample_travel_date(),
            },
        ],
    )
}

/// An executor holding one case with a traveler, at revision 2.
async fn with_draft() -> TripExecutor {
    let executor = TripExecutor::default();
    for (step, command) in [
        TripCommand::Open,
        TripCommand::ChangeTraveler {
            traveler: sample_traveler(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let batch = support::batch(
            support::turn(u8::try_from(step).unwrap()),
            &case_ref(step as u64),
            &support::confirmed_click(),
            vec![command],
        );
        executor.execute(batch).await.expect("the setup commits");
    }
    executor
}

async fn state_of(executor: &TripExecutor) -> Option<TripState> {
    executor
        .load(&support::account(), &CaseId::from(CASE))
        .await
        .expect("the case loads")
        .value
}

#[tokio::test]
async fn a_batch_interrupted_halfway_resumes_where_it_stopped() {
    let executor = with_draft().await;
    let batch = editing_batch(2);

    let partial = executor
        .execute_prefix(&batch, 1)
        .expect("the first command commits");
    assert_eq!(partial.events.len(), 1, "only the first command ran");
    assert_eq!(partial.new_revision, CaseRevision(3));
    assert!(!partial.idempotency_replay, "nothing was replayed yet");
    let after_prefix = state_of(&executor).await.expect("the case exists");
    assert_eq!(after_prefix.name.as_deref(), Some(SAMPLE_NAME));
    assert_eq!(
        after_prefix.travel_date, None,
        "the second command never ran"
    );
    assert_eq!(executor.replayed_prefix_of(&batch), 1);

    let resumed = executor
        .execute(batch.clone())
        .await
        .expect("the batch resumes");
    assert_eq!(
        resumed.events.len(),
        2,
        "the replayed event and the new one, in order"
    );
    assert_eq!(
        resumed.events[0].event_id, partial.events[0].event_id,
        "the replayed half keeps the identifiers it already had"
    );
    assert!(
        resumed.idempotency_replay,
        "part of the batch came back from the journal, not from the domain"
    );
    assert_eq!(
        resumed.new_revision,
        CaseRevision(3),
        "a batch owns one revision however often it is resumed"
    );

    let final_state = state_of(&executor).await.expect("the case exists");
    assert_eq!(final_state.name.as_deref(), Some(SAMPLE_NAME));
    assert_eq!(final_state.travel_date, Some(sample_travel_date()));
    assert_eq!(executor.replayed_prefix_of(&batch), 2);
}

#[tokio::test]
async fn a_resumed_batch_is_indistinguishable_from_one_that_never_stopped() {
    let interrupted = with_draft().await;
    let uninterrupted = with_draft().await;
    let batch = editing_batch(2);

    interrupted.execute_prefix(&batch, 1).expect("half of it");
    let resumed = interrupted
        .execute(batch.clone())
        .await
        .expect("the rest of it");
    let straight = uninterrupted
        .execute(batch)
        .await
        .expect("all of it at once");

    assert_eq!(resumed.new_revision, straight.new_revision);
    assert_eq!(resumed.state, straight.state);
    assert_eq!(
        resumed
            .events
            .iter()
            .map(|e| e.event_id)
            .collect::<Vec<_>>(),
        straight
            .events
            .iter()
            .map(|e| e.event_id)
            .collect::<Vec<_>>(),
        "the same events, with the same identifiers, in the same order"
    );
    // The one honest difference: the resumed commit says so.
    assert!(resumed.idempotency_replay);
    assert!(!straight.idempotency_replay);
}

#[tokio::test]
async fn a_whole_batch_replayed_repeats_no_effect() {
    let executor = with_draft().await;
    let batch = editing_batch(2);
    let first = executor.execute(batch.clone()).await.expect("it commits");

    let again = executor.execute(batch.clone()).await.expect("it replays");
    assert!(again.idempotency_replay);
    assert_eq!(again.new_revision, first.new_revision);
    assert_eq!(
        again.events.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        first.events.iter().map(|e| e.event_id).collect::<Vec<_>>()
    );
    assert_eq!(again.state, first.state);
    assert_eq!(executor.replayed_prefix_of(&batch), 2);
    for envelope in &batch.envelopes {
        assert!(executor.has_executed(&envelope.idempotency_key));
    }
}

#[tokio::test]
async fn a_key_reused_with_another_command_is_refused() {
    let executor = with_draft().await;
    let batch = editing_batch(2);
    executor.execute_prefix(&batch, 1).expect("half of it");

    // The same envelope identity, a different command.
    let mut forged = batch.clone();
    forged.envelopes[0].command = TripCommand::SetName {
        value: "Something else".to_owned(),
    };

    let refused = executor.execute(forged).await.unwrap_err();
    assert!(matches!(
        refused,
        ExecutionError::IdempotencyMismatch { .. }
    ));
}

#[tokio::test]
async fn a_batch_whose_second_command_ran_without_its_first_is_refused() {
    let executor = with_draft().await;
    let batch = editing_batch(2);
    // Execute the second command on its own, under its own key.
    let tail = support::batch(
        support::turn(2),
        &case_ref(2),
        &support::confirmed_click(),
        vec![TripCommand::SetTravelDate {
            value: sample_travel_date(),
        }],
    );
    // The tail batch derives the same idempotency key for that command, which
    // is exactly the situation: the key executed, but not as part of `batch`.
    assert_eq!(
        tail.envelopes[0].idempotency_key,
        batch.envelopes[1].idempotency_key
    );
    executor.execute(tail).await.expect("the tail commits");

    let refused = executor.execute(batch).await.unwrap_err();
    assert!(
        matches!(refused, ExecutionError::IdempotencyMismatch { .. }),
        "a replayed key in the middle of an unexecuted batch is not a resume: {refused:?}"
    );
}

#[tokio::test]
async fn resuming_after_somebody_else_wrote_is_a_revision_conflict() {
    let executor = with_draft().await;
    let batch = editing_batch(2);
    executor.execute_prefix(&batch, 1).expect("half of it");

    // Another turn moves the case forward while the batch is interrupted.
    let intruder = support::batch(
        support::turn(9),
        &case_ref(3),
        &support::confirmed_click(),
        vec![TripCommand::ChangeTraveler {
            traveler: sample_traveler(),
        }],
    );
    executor
        .execute(intruder)
        .await
        .expect("the intruder commits");

    let refused = executor.execute(batch).await.unwrap_err();
    match refused {
        ExecutionError::RevisionConflict(conflict) => {
            assert_eq!(conflict.current_revision, CaseRevision(4));
            assert_eq!(conflict.expected.expected_revision, CaseRevision(2));
        }
        other => panic!("expected a revision conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn an_untouched_batch_still_checks_the_revision() {
    let executor = with_draft().await;
    let stale = editing_batch(1);
    let refused = executor.execute(stale).await.unwrap_err();
    assert!(matches!(refused, ExecutionError::RevisionConflict(_)));
    assert_eq!(executor.replayed_prefix_of(&editing_batch(2)), 0);
}

#[tokio::test]
async fn a_prefix_outside_the_batch_is_refused() {
    let executor = with_draft().await;
    let batch = editing_batch(2);
    assert!(matches!(
        executor.execute_prefix(&batch, 0),
        Err(ExecutionError::ScopeViolation)
    ));
    assert!(matches!(
        executor.execute_prefix(&batch, 3),
        Err(ExecutionError::ScopeViolation)
    ));
    assert_eq!(executor.replayed_prefix_of(&batch), 0, "nothing ran");
    assert_eq!(TripWorkflow::default().key(), executor.definition().key());
}
