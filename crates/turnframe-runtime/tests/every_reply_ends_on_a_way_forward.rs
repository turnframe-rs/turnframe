//! Every reply the runtime writes ends on a way forward: the ask, the card, the next steps,
//! or a question to go on. A reply that only reports leaves the user with nothing to say.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narration, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::response::{AssistantTurn, ResponseBlock};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{
    ScriptedProvider, ScriptedProviderBuilder, ScriptedReply, UnderstandingBuilder,
};
use turnframe_test::workflows::trip::{SAMPLE_NAME, complete_case, operations};

const GO_ON: &str = "What would you like to do next?";

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// The reply the user reads: the turn's transition.
fn reply(turn: &AssistantTurn) -> String {
    turn.blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Transition(transition) => Some(transition.text.clone()),
            _ => None,
        })
        .expect("the turn has a reply")
}

#[tokio::test]
async fn a_reply_the_review_refused_still_says_what_was_not_done_and_what_comes_next() {
    let text = "call it Lisbon offsite";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_one(), "trip", "trip-1"),
            serde_json::json!({ "value": SAMPLE_NAME }),
            text,
        )
        .build()
        .unwrap();
    let refused = |provider: ScriptedProviderBuilder| {
        provider
            .reply_to(ModelPurpose::Acknowledge, ScriptedReply::written("Done."))
            .reply_to(ModelPurpose::Review, ScriptedReply::review_fails())
    };
    let provider =
        refused(refused(ScriptedProvider::builder("scripted", "model-1"))).build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let turn = harness
        .handle(harness.turn(turn_one(), text))
        .await
        .unwrap();

    let said = narration(&turn);
    assert!(said.contains("already called"), "what was not done: {said}");
    assert!(said.contains(GO_ON), "what comes next: {said}");
    assert!(said.contains("Add another extra"), "the next steps: {said}");
}

#[tokio::test]
async fn an_answer_alone_ends_on_the_question_to_go_on() {
    let text = "what is it called?";
    let understanding = UnderstandingBuilder::of(text).ask(text).build().unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("It is called Lisbon offsite.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let turn = harness
        .handle(harness.turn(turn_one(), text))
        .await
        .unwrap();

    let said = reply(&turn);
    assert!(said.contains("It is called Lisbon offsite."), "{said}");
    assert!(said.contains(GO_ON), "{said}");
}

#[tokio::test]
async fn a_turn_with_nothing_to_report_still_asks_what_comes_next() {
    let text = "thanks";
    let understanding = UnderstandingBuilder::of(text)
        .chitchat(text)
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(understanding)
        .provider(support::silent().build_shared())
        .build()
        .await;

    let turn = harness
        .handle(harness.turn(turn_one(), text))
        .await
        .unwrap();

    assert_eq!(narration(&turn).trim(), GO_ON);
}
