//! What happens to prose longer than a deployment allows.
//!
//! Nothing is capped unless a deployment says so. When it does, an answer past the
//! cap is sent back once to be said more briefly, and one still too long is withheld
//! whole, with the reason in its own block: never cut.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::error::{DomainRejection, ExecutionError};
use turnframe_core::ids::TurnId;
use turnframe_core::plan::AnswerBasis;
use turnframe_core::response::{AnswerStatus, ResponseBlock};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_runtime::config::NarrationConfig;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// A long answer, and what the turn does with it under a given configuration.
async fn answer_under(narration: NarrationConfig) -> (AnswerStatus, String, usize) {
    let turn = turn_one();
    let text = "Set the name to Lisbon. What is the total?";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .ask_about(
            AnswerBasis::CurrentCommittedState,
            None,
            &["total_amount"],
            "What is the total?",
        )
        .build()
        .unwrap();
    let long = "word ".repeat(60);
    // The first answer, and the one written again when the first was too long.
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering(long.clone())
        .answering(long)
        .acknowledging("Noted.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip_fails(ExecutionError::Rejected(DomainRejection::new(
            "trip.locked",
            "trip.error.locked",
        )))
        .understands(understanding)
        .provider(std::sync::Arc::clone(&provider))
        .narration(narration)
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();
    let answer = answered
        .blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer),
            _ => None,
        })
        .expect("the question gets a block either way");
    let attempts = provider.calls_for(ModelPurpose::Answer).len();
    (answer.status, answer.text.clone(), attempts)
}

/// What ships: no cap, so a long answer arrives whole and unedited.
#[tokio::test]
async fn an_answer_is_not_shortened_when_nothing_was_configured() {
    let (status, text, _) = answer_under(NarrationConfig::conservative()).await;
    assert_eq!(status, AnswerStatus::Answered);
    assert_eq!(
        text.chars().count(),
        "word ".repeat(60).trim().chars().count(),
        "every word the model wrote is there, and no ellipsis was added"
    );
    assert!(!text.ends_with('…'), "{text}");
}

/// A deployment that set one gets the answer written again, then a refusal, never a
/// cut sentence.
#[tokio::test]
async fn an_answer_past_a_configured_cap_is_refused_whole() {
    let (status, text, attempts) =
        answer_under(NarrationConfig::conservative().with_max_answer_chars(Some(40))).await;
    assert_eq!(
        attempts, 2,
        "the model was asked once to say it more briefly"
    );
    assert_eq!(
        status,
        AnswerStatus::Withheld,
        "there was an answer and it is not being given"
    );
    assert!(
        !text.starts_with("word word"),
        "and it is not the model's words cut short: {text}"
    );
}
