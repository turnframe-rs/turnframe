//! A change the user says is wrong is asked for again: the reply owns it and asks what
//! it should be instead, naming the change as the last reply showed it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::response::ResponseBlock;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, ScriptedReply, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

#[tokio::test]
async fn a_disputed_change_is_asked_for_again() {
    let first = turn(1);
    let set = "call the name Lisbon";
    let wrote = UnderstandingBuilder::of(set)
        .apply(
            operations::SET_NAME,
            token_for(first, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            set,
        )
        .build()
        .unwrap();
    let wrong = "no, that is not what I said";
    let disputed = UnderstandingBuilder::of(wrong)
        .dispute(Some("r1"), wrong)
        .build()
        .unwrap();
    // The second acknowledgement fails its review twice, so the question code wrote
    // stands in for it.
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Done.")
        .reply_to(ModelPurpose::Acknowledge, ScriptedReply::written("Sorry."))
        .reply_to(ModelPurpose::Review, ScriptedReply::review_fails())
        .reply_to(ModelPurpose::Acknowledge, ScriptedReply::written("Sorry."))
        .reply_to(ModelPurpose::Review, ScriptedReply::review_fails())
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(wrote)
        .understands(disputed)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(first, set)).await.unwrap();
    let reply = harness.handle(harness.turn(turn(2), wrong)).await.unwrap();

    let written_from = provider.calls_for(ModelPurpose::Acknowledge)[1].user_text();
    assert!(
        written_from.contains("\"what\": \"Trip named: The trip is now called \\\"Lisbon\\\".\""),
        "the ask is the change as it was shown: {written_from}"
    );
    let asked = reply
        .blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Transition(transition) => Some(transition.text.clone()),
            _ => None,
        })
        .expect("the reply asks");
    assert!(
        asked.ends_with("What should it be instead?"),
        "and with no model to word it, the server asks: {asked}"
    );
}
