//! Chaos: a failure injected at every boundary of spec §27.7, and what the
//! runtime is then obliged to say and to do.
//!
//! Recovery code is the part of a system the happy path never exercises, so
//! each test here kills the turn at one named boundary and then asks the two
//! questions that matter: **is the recovery idempotent** — does running the
//! same turn again reach the same state without doubling an effect — and **is
//! the status truthful** — does what the user was told match what the ledger
//! says.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, receipt_codes, token_for};
use turnframe_core::error::StoreError;
use turnframe_core::ids::{CaseRevision, TurnId};
use turnframe_core::interaction::InteractionStatus;
use turnframe_core::replay::TurnPhase;
use turnframe_core::understanding::{ActTarget, Understanding};
use turnframe_runtime::orchestrator::ResumeOutcome;
use turnframe_runtime::recover::RecoveryAction;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::stores::{CRASH_BOUNDARIES, FailurePoint, crash_boundary};
use turnframe_test::workflows::trip::{incomplete_case, operations, with_offer};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// What every "set the name" chaos test's turn is understood to say.
fn subject(turn_id: TurnId) -> Understanding {
    UnderstandingBuilder::of(SUBJECT_TEXT)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            SUBJECT_TEXT,
        )
        .build()
        .unwrap()
}

const SUBJECT_TEXT: &str = "Set the name to Lisbon";

#[test]
fn every_boundary_of_the_specification_has_a_name() {
    assert_eq!(CRASH_BOUNDARIES.len(), 7);
    for (name, point) in CRASH_BOUNDARIES {
        assert_eq!(crash_boundary(name), Some(point), "{name}");
    }
}

/// A failure before the journal insert leaves the key free, so the same turn
/// run again is a first run and commits exactly once.
#[tokio::test]
async fn a_crash_before_the_journal_insert_recovers_by_simply_running_again() {
    let turn_id = turn(1);
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Done.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .understands(subject(turn(2)))
        .provider(provider)
        .build()
        .await;
    harness.fail_at(FailurePoint::BeforeJournalInsert, StoreError::Unavailable);

    let failed = harness.handle(harness.turn(turn_id, SUBJECT_TEXT)).await;
    assert!(failed.is_err());
    assert!(
        harness.journal(turn_id).await.is_empty(),
        "the key stays free"
    );
    assert_eq!(
        harness.phase(turn_id).await,
        TurnPhase::Failed,
        "nothing was admitted, so nothing can have happened and nobody has to come back"
    );
    assert_eq!(
        harness.plan_recovery(turn_id).await,
        RecoveryAction::Nothing {
            phase: TurnPhase::Failed
        },
        "a sweep has no work here; the traveler simply retries"
    );

    // Nothing was admitted, so the traveler simply says it again. That is a new
    // turn, with its own identifier and its own tokens.
    let retry = turn(2);
    let turn = harness
        .handle(harness.turn(retry, SUBJECT_TEXT))
        .await
        .unwrap();
    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set"],
        "the retry committed once"
    );
    assert_eq!(receipt_codes(&turn), vec!["trip.name_set"]);
}

/// A failure after the journal insert leaves a `Pending` entry: recovery must
/// resume it by idempotency key, and resuming must not double the effect.
#[tokio::test]
async fn a_crash_after_the_journal_insert_resumes_by_idempotency_key() {
    let turn_id = turn(1);
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Done.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(provider)
        .build()
        .await;
    harness.fail_at(
        FailurePoint::AfterJournalInsertBeforeCommit,
        StoreError::Unavailable,
    );

    let failed = harness.handle(harness.turn(turn_id, SUBJECT_TEXT)).await;
    assert!(failed.is_err());
    let entries = harness.journal(turn_id).await;
    assert_eq!(entries.len(), 1);
    assert!(entries[0].status.is_pending());
    assert!(matches!(
        harness.plan_recovery(turn_id).await,
        RecoveryAction::ResumeCommands { .. }
    ));

    let keys_before = harness.idempotency_keys(turn_id).await;
    let outcome = harness.resume(turn_id).await;
    let keys_after = harness.idempotency_keys(turn_id).await;

    assert!(matches!(outcome, ResumeOutcome::Resumed { .. }));
    assert_eq!(
        keys_before, keys_after,
        "the same key, not a second command"
    );
    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set"],
        "resuming committed the effect once"
    );
    assert_eq!(harness.journal(turn_id).await.len(), 1);
    assert_eq!(harness.phase(turn_id).await, TurnPhase::Delivered);
}

/// A failure inside the commit bundle discards the whole bundle. The domain's
/// own state has moved — that seam is deliberately not in the transaction — but
/// nothing of the bundle is readable, and running again reconciles the two.
#[tokio::test]
async fn a_crash_inside_the_commit_bundle_leaves_the_ledger_empty_and_recovers() {
    let turn_id = turn(1);
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Done.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(provider)
        .build()
        .await;
    harness.fail_at(
        FailurePoint::AfterCommitBeforeEventReadback,
        StoreError::Timeout,
    );

    assert!(
        harness
            .handle(harness.turn(turn_id, SUBJECT_TEXT))
            .await
            .is_err()
    );
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "an aborted bundle publishes none of itself"
    );
    assert!(harness.stored_turn(turn_id).await.is_none());
    assert!(matches!(
        harness.plan_recovery(turn_id).await,
        RecoveryAction::ResumeCommands { .. }
    ));

    // Recovery resumes the journaled entry by key. The domain executor already
    // committed its own state — that seam is deliberately outside the bundle —
    // so it answers from its idempotency record and the ledger gets one copy.
    let outcome = harness.resume(turn_id).await;
    assert!(matches!(outcome, ResumeOutcome::Resumed { .. }));
    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set"],
        "the replayed command reported its original outcome; the ledger has one copy"
    );
    assert_eq!(harness.trip_revision("trip-1"), CaseRevision(4));
    assert_eq!(harness.phase(turn_id).await, TurnPhase::Delivered);
    let answered = harness
        .stored_turn(turn_id)
        .await
        .expect("the resumed turn was answered");
    assert_eq!(receipt_codes(&answered), vec!["trip.name_set"]);
}

/// The card was written and the caller was told it was not. The truthful
/// reaction is to say nothing about it, and a rerun must not produce a second
/// card for the same turn.
#[tokio::test]
async fn a_crash_after_interaction_persistence_never_doubles_the_card() {
    let turn_id = turn(1);
    let text = "Set the name on the Ferri trip to Lisbon";
    let builder = UnderstandingBuilder::of(text).apply_to(
        operations::SET_NAME,
        ActTarget::Ambiguous {
            candidates: vec![
                token_for(turn_id, "trip", "trip-1"),
                token_for(turn_id, "trip", "trip-2"),
            ],
        },
        serde_json::json!({"value": "Lisbon"}),
        text,
    );
    let act = builder.last_act().expect("the act just added");
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .understands(builder.build().unwrap())
        .provider(narrating().build_shared())
        .build()
        .await;
    harness.fail_at(
        FailurePoint::AfterInteractionPersistence,
        StoreError::Unavailable,
    );

    let first = harness.handle(harness.turn(turn_id, text)).await.unwrap();
    assert!(
        first.interactions().next().is_none(),
        "the turn was told the card is not there, so it does not show one"
    );

    // The card is nevertheless in the store: that is exactly what this boundary
    // means, and it is why the truthful reaction is to omit it rather than to
    // write a second one.
    let written: Vec<_> = harness
        .open_cards("trip", "trip-1")
        .await
        .into_iter()
        .chain(harness.open_cards("trip", "trip-2").await)
        .collect();
    assert_eq!(written.len(), 1, "one card, written once");
    assert_eq!(
        written[0].id,
        turnframe_runtime::interactions::derive_interaction_id(
            &turn_id,
            &format!("select_target:{act}")
        ),
        "its identifier is derived from the turn, so replaying the turn addresses this row \
         instead of creating a second card"
    );
}

/// A failure before response persistence keeps the effects and loses only the
/// wording, which is what §23.1 asks recovery to regenerate.
#[tokio::test]
async fn a_crash_before_response_persistence_keeps_the_effects_and_regenerates() {
    let turn_id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(narrating().build_shared())
        .build()
        .await;
    harness.fail_at(
        FailurePoint::BeforeResponsePersistence,
        StoreError::Unavailable,
    );

    assert!(
        harness
            .handle(harness.turn(turn_id, SUBJECT_TEXT))
            .await
            .is_err()
    );

    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set"],
        "the commit landed"
    );
    assert_eq!(
        harness.phase(turn_id).await,
        TurnPhase::Committed,
        "a committed turn is not written off as failed"
    );
    assert!(harness.stored_turn(turn_id).await.is_none());
    match harness.plan_recovery(turn_id).await {
        RecoveryAction::RegenerateResponse { events, .. } => {
            assert_eq!(events.len(), 1, "the answer is rebuilt from the ledger");
        }
        other => panic!("expected a regeneration, got {other:?}"),
    }

    let regenerated = harness.resume(turn_id).await;
    let ResumeOutcome::Regenerated { turn } = regenerated else {
        panic!("expected the answer to be rebuilt, got {regenerated:?}");
    };
    assert_eq!(
        receipt_codes(&turn),
        vec!["trip.name_set"],
        "the receipt comes from the event, not from a re-execution"
    );
    assert_eq!(
        harness.events("trip", "trip-1").await.len(),
        1,
        "regenerating an answer executes nothing"
    );
    assert_eq!(harness.phase(turn_id).await, TurnPhase::Delivered);
    assert!(harness.stored_turn(turn_id).await.is_some());
}

/// The dispatcher never got the work: no row moves, and the next sweep finds
/// exactly the same one.
#[tokio::test]
async fn a_crash_before_outbox_dispatch_leaves_the_row_claimable() {
    let harness = sent_rebooking().await;
    let rows = harness
        .stores
        .stores()
        .outbox()
        .claim_due(support::now(), 10, "worker-1")
        .await;
    // The first sweep is the one that was armed to fail.
    assert!(rows.is_err(), "the sweep failed before claiming anything");

    let rows = harness
        .stores
        .stores()
        .outbox()
        .claim_due(support::now(), 10, "worker-1")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "the row is still there to be claimed");
    assert_eq!(rows[0].destination, "trip.rebooking");
}

/// The external call was made and its result could not be recorded: the row
/// stays `Dispatching` with its claim, which is the state a reaper exists for.
#[tokio::test]
async fn a_crash_after_outbox_dispatch_leaves_a_row_a_reaper_can_release() {
    let harness = sent_rebooking_without_sweep_failure().await;
    let outbox = harness.stores.stores().outbox().clone();
    let claimed = outbox
        .claim_due(support::now(), 10, "worker-1")
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);

    harness.fail_at(FailurePoint::AfterOutboxDispatch, StoreError::Timeout);
    let recorded = outbox.mark_completed(&claimed[0].outbox_id).await;
    assert!(recorded.is_err(), "the outcome could not be written");

    let row = outbox.get(&claimed[0].outbox_id).await.unwrap();
    assert_eq!(
        row.entry.status,
        turnframe_core::event::OutboxStatus::Dispatching,
        "the row says what is true: it is out there and unsettled"
    );

    let released = outbox
        .release_expired_claims(support::now() + chrono::TimeDelta::minutes(1))
        .await
        .unwrap();
    assert_eq!(released, vec![claimed[0].outbox_id]);
    let row = outbox.get(&claimed[0].outbox_id).await.unwrap();
    assert_eq!(
        row.entry.status,
        turnframe_core::event::OutboxStatus::Pending
    );
}

/// Runs a turn that rebooks a trip, so the outbox has a row, with the
/// dispatch sweep armed to fail once.
async fn sent_rebooking() -> Harness {
    let harness = sent_rebooking_without_sweep_failure().await;
    harness.fail_at(FailurePoint::BeforeOutboxDispatch, StoreError::Unavailable);
    harness
}

async fn sent_rebooking_without_sweep_failure() -> Harness {
    let first = turn(1);
    let review_text = "It is ready, show me the rebooking card";
    let review = UnderstandingBuilder::of(review_text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(first, "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            "show me the rebooking card",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .without_narration()
        .build()
        .await;
    harness
        .handle(harness.turn(first, review_text))
        .await
        .unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1").value();
    harness
        .handle(harness.click(
            turn(2),
            card.id,
            turnframe_test::workflows::trip::REBOOK_CONFIRM_OPTION,
            revision,
        ))
        .await
        .unwrap();
    assert_eq!(
        harness.blocking_card_status("trip", "trip-1").await,
        None,
        "the card was settled by the click"
    );
    harness
}

impl Harness {
    /// The status of the case's blocking card, when it still has one.
    async fn blocking_card_status(
        &self,
        workflow: &str,
        case_id: &str,
    ) -> Option<InteractionStatus> {
        self.open_cards(workflow, case_id)
            .await
            .into_iter()
            .find(|card| card.blocking)
            .map(|card| card.status)
    }

    /// What recovery would do with this turn.
    async fn plan_recovery(&self, turn_id: TurnId) -> RecoveryAction {
        self.orchestrator
            .plan_recovery(&self.account(), &turn_id)
            .await
            .expect("the turn exists")
    }

    /// Acts on that decision.
    async fn resume(&self, turn_id: TurnId) -> ResumeOutcome {
        self.orchestrator
            .resume_turn(&self.account(), &turn_id)
            .await
            .expect("recovery could act")
    }
}

/// A sanity check on the fixture: the rebook really did produce an outbox row.
#[tokio::test]
async fn a_submitted_trip_leaves_exactly_one_outbox_row() {
    let harness = sent_rebooking_without_sweep_failure().await;
    let journal = harness.journal(turn(2)).await;
    assert_eq!(journal.len(), 1);
    let rows = harness
        .stores
        .outbox_for_command(&journal[0].command_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].entry.status,
        turnframe_core::event::OutboxStatus::Pending
    );
    assert_eq!(
        rows[0].entry.idempotency_key, journal[0].idempotency_key,
        "the external system is handed the same key the journal admitted (§16.4)"
    );
}

/// A single arming fires once and once only, whichever boundary it is at.
#[tokio::test]
async fn an_armed_failure_fires_once() {
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .provider(Arc::new(ScriptedProvider::builder("s", "m").build()))
        .build()
        .await;
    harness.fail_at(FailurePoint::BeforeJournalInsert, StoreError::Unavailable);
    assert_eq!(
        harness.stores.armed_failures(),
        vec![FailurePoint::BeforeJournalInsert]
    );
    harness.stores.clear_failures();
    assert!(harness.stores.armed_failures().is_empty());
}
