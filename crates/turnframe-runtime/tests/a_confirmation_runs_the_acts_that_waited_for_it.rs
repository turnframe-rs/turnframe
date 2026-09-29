//! An act that needs what a confirmation will make waits on that confirmation's
//! card, and runs, under its own policy, once the click has committed (spec §6.7).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::Harness;
use turnframe_core::ids::{CaseId, InteractionId, TurnId};
use turnframe_core::interaction::InteractionKind;
use turnframe_core::understanding::{ActTarget, ConstraintKind};
use turnframe_runtime::policy::{CONFIRM_OPTION_ID, DECLINE_OPTION_ID};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::operations;

const ASKED: &str = "open a trip and set the name to Porto, but ask me first";

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// Opens the trip behind a confirmation, with the name waiting on it, and
/// returns the card and the case it would create.
async fn asked() -> (Harness, InteractionId, CaseId) {
    let opening = UnderstandingBuilder::of(ASKED).open(
        operations::OPEN,
        "trip",
        serde_json::json!(null),
        "open a trip",
    );
    let opener = opening.last_act().unwrap();
    let understanding = opening
        .apply_to(
            operations::SET_NAME,
            ActTarget::SameTurn { act: opener },
            serde_json::json!({"value": "Porto"}),
            "set the name to Porto",
        )
        .constrain(ConstraintKind::AskBeforeApplying, "ask me first")
        .build()
        .unwrap();
    let harness = Harness::builder()
        .understands(understanding)
        .without_narration()
        .build()
        .await;
    let answered = harness.handle(harness.turn(turn(1), ASKED)).await.unwrap();
    let card = answered.interactions().next().expect("a confirmation card");
    assert_eq!(card.kind, InteractionKind::ConfirmCommand);
    let case_id = card.case_ref.case_id.clone();
    assert_eq!(
        harness.trip_name(case_id.as_str()),
        None,
        "nothing is written before the click"
    );
    (harness, card.id, case_id)
}

#[tokio::test]
async fn confirming_runs_the_act_that_waited_for_it() {
    let (harness, card, case_id) = asked().await;
    harness
        .handle(harness.click(turn(2), card, CONFIRM_OPTION_ID, 0))
        .await
        .unwrap();
    assert_eq!(
        harness.trip_name(case_id.as_str()).as_deref(),
        Some("Porto"),
        "the name waited for the trip it belongs to, and then ran"
    );
    assert_eq!(
        harness.understander.remaining(),
        0,
        "the click was understood without a model call"
    );
}

#[tokio::test]
async fn declining_runs_neither() {
    let (harness, card, case_id) = asked().await;
    harness
        .handle(harness.click(turn(2), card, DECLINE_OPTION_ID, 0))
        .await
        .unwrap();
    assert!(harness.events("trip", case_id.as_str()).await.is_empty());
    assert_eq!(harness.trip_name(case_id.as_str()), None);
}
