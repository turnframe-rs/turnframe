//! The transition is not an answer.
//!
//! A model handed a question answers it, so an acknowledgement shown the turn's
//! questions writes the answer beside the answer block's own. The acknowledgement is
//! given neither the questions nor their words in the user's message: it is told
//! only how many have blocks of their own.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::plan::AnswerBasis;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// Runs a turn on `text` that records `value` and asks «and what is the total?»,
/// and returns the provider that recorded what each stage was asked.
async fn a_value_and_a_question(text: &str, value: &str) -> Arc<ScriptedProvider> {
    let turn = turn_one();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": value}),
            value,
        )
        .ask_about(
            AnswerBasis::CurrentCommittedState,
            None,
            &["total_amount"],
            "and what is the total?",
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("It is 30.")
        .acknowledging("Noted. It is 30.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();
    provider
}

/// What the first call of `purpose` was written from, as the model read it.
fn written_from(provider: &ScriptedProvider, purpose: ModelPurpose) -> String {
    provider
        .calls_for(purpose)
        .into_iter()
        .next()
        .expect("the stage ran")
        .user_text()
}

/// The acknowledgement is not shown the questions, only their answers, so it gives the
/// answers and cannot answer the questions again.
#[tokio::test]
async fn the_transition_is_not_given_the_questions() {
    let provider = a_value_and_a_question("Lisbon, and what is the total?", "Lisbon").await;
    let shown = written_from(&provider, ModelPurpose::Acknowledge);
    assert!(
        shown.contains("Answers to give in your reply:\n- It is 30."),
        "it is given the answer to give: {shown}"
    );
    assert!(
        !shown.contains("what is the total"),
        "and the question's words are nowhere in what it was asked to do: {shown}"
    );
}

/// Nor does it see the question in the message the user typed: the words the
/// questions are made of are cut out of its copy, and only that one.
#[tokio::test]
async fn the_transition_does_not_see_the_question_in_the_users_own_message() {
    let provider =
        a_value_and_a_question("Porto offsite, and what is the total?", "Porto offsite").await;
    let acknowledging = written_from(&provider, ModelPurpose::Acknowledge);
    assert!(
        acknowledging.contains("The user wrote: «Porto offsite»"),
        "the acknowledgement keeps what the user said and loses what they asked: \
         {acknowledging}"
    );
    let answering = written_from(&provider, ModelPurpose::Answer);
    assert!(
        answering.contains("Question: «and what is the total?»"),
        "the stage whose job it is gets the question: {answering}"
    );
}
