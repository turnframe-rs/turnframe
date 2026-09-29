//! A question about where the work stands is answered from the record in view, and the
//! reply then asks for what that record needs next.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::plan::AnswerBasis;
use turnframe_core::response::ResponseBlock;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::incomplete_case;

const TEXT: &str = "where are we with this?";

#[tokio::test]
async fn a_question_about_the_work_in_progress_asks_what_comes_next() {
    // Framing named no record; the question is about what is saved now.
    let understanding = UnderstandingBuilder::of(TEXT)
        .ask_about(AnswerBasis::CurrentCommittedState, None, &[], TEXT)
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("Trip 1 for Aurora is a draft that still needs a name.")
        .acknowledging("Trip 1 for Aurora is a draft. What should the name be?")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let reply = harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), TEXT))
        .await
        .unwrap();

    let answered = provider.calls_for(ModelPurpose::Answer)[0].user_text();
    assert!(
        answered.contains("Trip 1"),
        "the answer is given the record in view: {answered}"
    );
    let asked = provider.calls_for(ModelPurpose::Acknowledge)[0].user_text();
    assert!(
        asked.contains("\"ask\""),
        "the reply is told what to ask next: {asked}"
    );
    let kinds: Vec<&str> = reply
        .blocks
        .iter()
        .map(|block| match block {
            ResponseBlock::Answer(_) => "answer",
            ResponseBlock::Transition(_) => "transition",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        ["transition", "answer"],
        "the one reply, which gives the answer and moves the work on, then the answer as data"
    );
}
