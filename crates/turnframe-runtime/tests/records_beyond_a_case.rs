//! A question about the account's own records, which are not cases.
//!
//! Travelers are rows the account owns, and a trip that has been ticketed is
//! no longer a case of anything, so a question about what the account has
//! cannot rest on case state alone. Retrieval is consulted for a question about
//! current state as well as one about the domain: for the domain the sources
//! are the answer, for state they add to it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{FixedKnowledge, Harness};
use turnframe_core::ids::TurnId;
use turnframe_core::plan::AnswerBasis;
use turnframe_core::response::{AnswerStatus, ResponseBlock};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::incomplete_case;

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// Runs one question of `basis` against `knowledge`, returning the answer block
/// and what the answering call was told.
async fn asking(basis: AnswerBasis, knowledge: Option<FixedKnowledge>) -> (AnswerStatus, String) {
    let turn = turn_one();
    let text = "quali sono i miei viaggiatori?";
    let understood = UnderstandingBuilder::of(text)
        .ask_about(basis, None, &[], text)
        .build()
        .unwrap();
    // A question about what is recorded is on the trip in view, so the reply also
    // asks what it needs next; one about the domain is not.
    let mut script =
        ScriptedProvider::builder("scripted", "model-1").answering("Ferri and Bianchi.");
    if basis != AnswerBasis::GeneralDomainKnowledge {
        script = script.acknowledging("What should the name be?");
    }
    let provider = script.build_shared();
    let mut builder = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understood)
        .provider(Arc::clone(&provider));
    if let Some(knowledge) = knowledge {
        builder = builder.knowledge(Arc::new(knowledge));
    }
    let harness = builder.build().await;
    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();

    let status = answered
        .blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer.status),
            _ => None,
        })
        .expect("every question yields a block");
    let asked = provider
        .calls_for(ModelPurpose::Answer)
        .into_iter()
        .next()
        .map(|call| {
            call.request
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
                .filter_map(|part| match part {
                    turnframe_provider::request::ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    (status, asked)
}

/// What the account's own records say reaches the stage that answers.
#[tokio::test]
async fn a_question_about_state_may_rest_on_a_source_too() {
    let (status, asked) = asking(
        AnswerBasis::CurrentCommittedState,
        Some(FixedKnowledge::saying("Travelers: Ferri, Bianchi.")),
    )
    .await;
    assert_eq!(status, AnswerStatus::Answered);
    assert!(
        asked.contains("Travelers: Ferri, Bianchi."),
        "a registry is not a case, and this was its only channel: {asked}"
    );
}

/// A provider with nothing to add leaves the question answerable from state.
///
/// This is the half that makes the change safe. For a question about the domain
/// the sources *are* the answer, so finding none settles it; for a question
/// about what is recorded the state is the answer and a source adds to it.
#[tokio::test]
async fn a_source_with_nothing_to_add_does_not_settle_a_state_question() {
    let (status, _) = asking(
        AnswerBasis::CurrentCommittedState,
        Some(FixedKnowledge::empty()),
    )
    .await;
    assert_eq!(status, AnswerStatus::Answered);
}

/// And a deployment with no provider at all is untouched.
#[tokio::test]
async fn a_state_question_without_any_provider_is_answered_as_before() {
    let (status, _) = asking(AnswerBasis::CurrentCommittedState, None).await;
    assert_eq!(status, AnswerStatus::Answered);
}

/// A question about the domain still needs its sources.
#[tokio::test]
async fn a_domain_question_with_no_source_is_still_unsupported() {
    let (status, _) = asking(
        AnswerBasis::GeneralDomainKnowledge,
        Some(FixedKnowledge::empty()),
    )
    .await;
    assert_eq!(
        status,
        AnswerStatus::Unsupported,
        "nothing approved says anything about it, and inventing one is not the answer"
    );
}

/// The stage that answers a question can see what the question is about.
///
/// A question is the one thing in a turn that can refer to what was *said*
/// rather than to what is recorded, so the answer stage is shown the
/// conversation and not only the facts.
#[tokio::test]
async fn the_answer_stage_sees_the_reply_the_question_is_about() {
    let first = turn_one();
    let opening = "aggiungi un extra da 10 euro";
    let set_subject = UnderstandingBuilder::of(opening)
        .apply(
            turnframe_test::workflows::trip::operations::SET_NAME,
            support::token_for(first, "trip", "trip-1"),
            serde_json::json!({"value": "un extra da 10 euro"}),
            opening,
        )
        .build()
        .unwrap();
    let question = UnderstandingBuilder::of("in che senso?")
        .ask("in che senso?")
        .following_up()
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("Nel senso della descrizione dell'extra.")
        .acknowledging("Ho aggiunto l'extra. Adesso mi serve la descrizione.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(set_subject)
        .understands(question)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    harness.handle(harness.turn(first, opening)).await.unwrap();
    harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(2)), "in che senso?"))
        .await
        .unwrap();

    let asked = provider
        .calls_for(ModelPurpose::Answer)
        .into_iter()
        .next()
        .expect("the answer stage ran")
        .request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|part| match part {
            turnframe_provider::request::ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        asked.contains("Adesso mi serve la descrizione"),
        "the antecedent of the question is what the question is about: {asked}"
    );
}
