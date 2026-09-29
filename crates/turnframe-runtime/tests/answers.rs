//! Several questions in one turn get one answer each, in the order asked, and none
//! of them disappears (spec §19.4).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::response::{AnswerStatus, GeneratedAnswer, ResponseBlock};
use turnframe_core::understanding::Understanding;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, ScriptedReply, UnderstandingBuilder};
use turnframe_test::workflows::trip::incomplete_case;

const TEXT: &str = "What is the name, when does it fly, and who is the traveler?";

fn turn_id() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn three_questions() -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .ask("What is the name")
        .ask("when does it fly")
        .ask("who is the traveler")
        .build()
        .unwrap()
}

/// The answer blocks of a turn, in order.
fn answers(turn: &turnframe_core::response::AssistantTurn) -> Vec<&GeneratedAnswer> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer),
            _ => None,
        })
        .collect()
}

async fn harness_answering(
    script: turnframe_test::providers::ScriptedProviderBuilder,
) -> (Harness, Arc<ScriptedProvider>) {
    // The questions are about the trip, so the reply also asks for what it needs next.
    let provider = script.build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(three_questions())
        .provider(Arc::clone(&provider))
        .build()
        .await;
    (harness, provider)
}

#[tokio::test]
async fn three_questions_get_three_answers_in_the_order_asked() {
    let (harness, provider) = harness_answering(
        ScriptedProvider::builder("scripted", "model-1")
            .answering("The name is Lisbon.")
            .answering("It has no travel date yet.")
            .answering("The traveler is Luca Ferri.")
            .acknowledging("What should the name be?"),
    )
    .await;

    let turn = harness.handle(harness.turn(turn_id(), TEXT)).await.unwrap();

    assert_eq!(
        provider.calls_for(ModelPurpose::Answer).len(),
        3,
        "one small task per question"
    );
    let answered = answers(&turn);
    let texts: Vec<&str> = answered.iter().map(|answer| answer.text.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "The name is Lisbon.",
            "It has no travel date yet.",
            "The traveler is Luca Ferri."
        ],
        "each question got its own answer, in its own order"
    );
    for answer in &answered {
        assert_eq!(answer.status, AnswerStatus::Answered);
        assert_eq!(
            answer.basis,
            turnframe_core::plan::AnswerBasis::CurrentCommittedState,
            "the basis is per question"
        );
    }
    let ids: Vec<String> = answered
        .iter()
        .filter_map(|answer| answer.question_id.as_ref())
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        ids,
        vec!["u1", "u2", "u3"],
        "every block names the question it answers"
    );
    provider.verify().expect("the script was followed exactly");
}

#[tokio::test]
async fn a_question_left_unanswered_says_so() {
    let (harness, _) = harness_answering(
        ScriptedProvider::builder("scripted", "model-1")
            .answering("The name is Lisbon.")
            .reply_to(
                ModelPurpose::Answer,
                ScriptedReply::cannot_answer("No travel date is recorded."),
            )
            // The third answer finds the reply's step instead: no model writes it.
            .acknowledging("What should the name be?"),
    )
    .await;

    let turn = harness.handle(harness.turn(turn_id(), TEXT)).await.unwrap();

    let answered = answers(&turn);
    assert_eq!(answered.len(), 3, "a question does not disappear");
    assert_eq!(answered[0].status, AnswerStatus::Answered);
    assert_eq!(answered[1].status, AnswerStatus::Unsupported);
    assert_eq!(answered[2].status, AnswerStatus::NotWritten);
    for answer in &answered[1..] {
        assert!(
            !answer.text.trim().is_empty(),
            "an unanswered question still says something"
        );
    }
}
