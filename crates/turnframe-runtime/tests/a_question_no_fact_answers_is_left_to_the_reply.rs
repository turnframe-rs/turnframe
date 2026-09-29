//! A question no fact answers is not pasted into the reply as a refusal: the one reply is
//! told it went unanswered, and says so in its own words, or owns a mistake the user points
//! at.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{FixedKnowledge, Harness};
use turnframe_core::ids::TurnId;
use turnframe_core::plan::AnswerBasis;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::incomplete_case;

#[tokio::test]
async fn a_question_no_fact_answers_is_left_to_the_reply() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let text = "why did you ask me that again?";
    let understanding = UnderstandingBuilder::of(text)
        .ask_about(AnswerBasis::GeneralDomainKnowledge, None, &[], text)
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Sorry, that was my mistake.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .knowledge(Arc::new(FixedKnowledge::unavailable()))
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();

    let written_from = provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .next()
        .expect("the reply is written")
        .user_text();
    assert!(
        written_from.contains("Questions no fact answers:\n- «why did you ask me that again?»"),
        "{written_from}"
    );
    assert!(!written_from.contains("Answers to give"), "{written_from}");
    let reply = answered
        .blocks
        .iter()
        .find_map(|block| match block {
            turnframe_core::response::ResponseBlock::Transition(reply) => Some(reply.text.as_str()),
            _ => None,
        })
        .unwrap();
    assert_eq!(reply, "Sorry, that was my mistake.");
}
