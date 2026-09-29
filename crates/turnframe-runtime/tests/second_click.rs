//! A card clicked twice: the second click authorizes nothing, and the turn still has
//! something true to say.
//!
//! The compare-and-set on the card is what keeps the effect from repeating (I14).
//! `ResponseAdmission::AlreadyAnswered` carries the first answer back so the turn repeats its
//! *result*: a fact and a notice that the card was already answered and, when the first answer
//! committed, its events as this turn's receipts.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::response::{AnswerProgress, NarratableFact, ResponseBlock};
use turnframe_runtime::policy::CONFIRM_OPTION_ID;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{complete_case, operations};

const CANCEL_TEXT: &str = "Cancel that trip";

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// Opens the confirmation card the conservative policy holds a cancellation
/// behind, answers it once, and returns the harness ready for a second click.
async fn answered_once() -> (Harness, turnframe_core::ids::InteractionId, u64) {
    let first = turn(1);
    let cancel = UnderstandingBuilder::of(CANCEL_TEXT)
        .apply(
            operations::WITHDRAW,
            support::token_for(first, "trip", "trip-1"),
            serde_json::json!(null),
            CANCEL_TEXT,
        )
        .build()
        .unwrap();
    // Narration is on, because what is under test is what the stage that speaks is given:
    // a lead-in for each of the three turns.
    let mut provider = support::narrating();
    for _ in 0..2 {
        provider = provider.acknowledging("Right.");
    }
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(cancel)
        .provider(provider.build_shared())
        .build()
        .await;
    let asked = harness
        .handle(harness.turn(first, CANCEL_TEXT))
        .await
        .unwrap();
    let card = asked.interactions().next().expect("a confirmation").id;
    let revision = harness.trip_revision("trip-1").value();
    harness
        .handle(harness.click(turn(2), card, CONFIRM_OPTION_ID, revision))
        .await
        .expect("the first click answers the card");
    (harness, card, revision)
}

/// Every fact a turn's blocks rest on.
fn facts(turn: &turnframe_core::response::AssistantTurn) -> Vec<&NarratableFact> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Transition(block) => Some(&block.facts_used),
            _ => None,
        })
        .flatten()
        .collect()
}

/// The second click leaves the turn a fact to rest on and a notice to show.
#[tokio::test]
async fn a_second_click_gives_the_turn_a_fact_and_a_notice() {
    let (harness, card, revision) = answered_once().await;
    let answered = harness
        .handle(harness.click(turn(3), card, CONFIRM_OPTION_ID, revision))
        .await
        .expect("a second click is not an error");

    let replayed = facts(&answered)
        .into_iter()
        .find_map(|fact| match fact {
            NarratableFact::InteractionAlreadyAnswered {
                interaction_id,
                progress,
                ..
            } => Some((*interaction_id, *progress)),
            _ => None,
        })
        .expect("the turn knows which card was clicked again");
    assert_eq!(replayed.0, card);
    assert_eq!(
        replayed.1,
        AnswerProgress::Done,
        "the first answer had committed, so the sentence is 'already done'"
    );
    assert!(
        answered
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Notice(notice)
                if notice.code == turnframe_runtime::reduce::notice::ALREADY_ANSWERED)),
        "and the user is told deterministically, whether or not a model runs"
    );
}

/// The effect is not repeated, which is the half that already worked.
#[tokio::test]
async fn a_second_click_still_executes_nothing() {
    let (harness, card, revision) = answered_once().await;
    let before = harness.events("trip", "trip-1").await.len();
    harness
        .handle(harness.click(turn(3), card, CONFIRM_OPTION_ID, revision))
        .await
        .unwrap();
    assert_eq!(
        harness.events("trip", "trip-1").await.len(),
        before,
        "the compare-and-set is the part of this worth leaving alone"
    );
    let record = harness.replay(turn(3)).await;
    assert!(
        record.command_outcomes.is_empty() && record.event_ids.is_empty(),
        "nothing was executed and nothing was committed: {record:?}"
    );
}

/// The result is repeated: the first click's events are this turn's receipts, so prose
/// saying the work is done rests on something this turn can show.
#[tokio::test]
async fn the_first_answers_receipts_come_back_with_it() {
    let (harness, card, revision) = answered_once().await;
    let answered = harness
        .handle(harness.click(turn(3), card, CONFIRM_OPTION_ID, revision))
        .await
        .unwrap();
    assert!(
        answered.receipts().next().is_some(),
        "the events the first click committed are what the user came back for"
    );
}
