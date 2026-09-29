//! A question about what the user can do is answered from the operations on offer,
//! never from a set of values a subject happens to declare.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::response::{AnswerStatus, ResponseBlock};
use turnframe_core::understanding::QuestionTopic;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::incomplete_case;

const ASKED: &str = "what can I do?";
const ANSWER: &str = "You can set the name, add an extra or set the travel date.";

fn turn() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

#[tokio::test]
async fn the_answer_is_written_from_the_operations_on_offer() {
    let understanding = UnderstandingBuilder::of(ASKED)
        .ask(ASKED)
        .about(QuestionTopic::Capabilities)
        .build()
        .unwrap();
    // A turn that only asks writes no transition: the answer is its one model call.
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering(ANSWER)
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(std::sync::Arc::clone(&provider))
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn(), ASKED)).await.unwrap();

    let answer = turn
        .blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer),
            _ => None,
        })
        .expect("the question is answered");
    assert_eq!(answer.status, AnswerStatus::Answered);
    assert_eq!(answer.text, ANSWER);
    assert!(
        answer.enumerations.is_empty(),
        "no declared set stands in for it"
    );

    let asked = provider.calls_for(ModelPurpose::Answer);
    let brief = format!("{:?}", asked[0].request.messages);
    assert!(
        brief.contains("operation_available") && brief.contains("trip.set_name"),
        "the model is given what can be done: {brief}"
    );
    assert!(
        !brief.contains("trip.acknowledge_card"),
        "an operation only a card on screen can take is not on offer without one"
    );
}

#[tokio::test]
async fn what_is_on_offer_is_said_in_the_turns_language() {
    let asked = "cosa posso fare?";
    let understanding = UnderstandingBuilder::of(asked)
        .ask(asked)
        .about(QuestionTopic::Capabilities)
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("Puoi impostare l'oggetto.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(std::sync::Arc::clone(&provider))
        .build()
        .await;
    let mut input = harness.turn(turn(), asked);
    input.locale = "it-IT".into();

    harness.handle(input).await.unwrap();

    let brief = format!(
        "{:?}",
        provider.calls_for(ModelPurpose::Answer)[0].request.messages
    );
    assert!(
        brief.contains("Dà un nome al viaggio"),
        "the summary is the one written for the turn's language: {brief}"
    );
}
