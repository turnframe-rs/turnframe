//! A stage with nothing to say is not asked to speak.
//!
//! The acknowledgement is requested only when the turn has material of its own: a
//! receipt, a refusal, a notice, or something outstanding on a case it engaged.
//! The runtime knows there is none before the call, so it does not make the call.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::{TargetToken, TurnId};
use turnframe_core::response::ResponseBlock;
use turnframe_core::understanding::QuestionTopic;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_runtime::config::NarrationConfig;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// The turn's replies, and how many times the acknowledgement stage ran.
async fn acknowledgements(narration: NarrationConfig, with_an_act: bool) -> (Vec<String>, usize) {
    let turn = turn_one();
    let text = "What can you do? Also set the name to Lisbon";
    let mut understanding = UnderstandingBuilder::of(text)
        .ask("What can you do?")
        .about(QuestionTopic::Capabilities);
    if with_an_act {
        understanding = understanding.apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "set the name to Lisbon",
        );
    }
    let mut builder = ScriptedProvider::builder("scripted", "model-1").answering("Quite a lot.");
    if with_an_act {
        builder = builder.acknowledging("Right. Quite a lot.");
    }
    let provider = builder.build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding.build().unwrap())
        .provider(Arc::clone(&provider))
        .narration(narration)
        .build()
        .await;
    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();

    let replies = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Transition(reply) => Some(reply.text.clone()),
            _ => None,
        })
        .collect();
    (replies, provider.calls_for(ModelPurpose::Acknowledge).len())
}

/// A turn with nothing of its own does not get asked for a sentence about it: its one
/// reply is its answer as written, and the question to go on that ends every reply. A
/// question about what can be done proposes in its own answer, so the obligations of the
/// trip in view stay background.
#[tokio::test]
async fn a_turn_with_nothing_of_its_own_writes_no_acknowledgement() {
    let (replies, calls) = acknowledgements(NarrationConfig::conservative(), false).await;
    assert_eq!(
        replies,
        ["Quite a lot.\n\nWhat would you like to do next?"],
        "the answer is the reply, and it ends on a way forward"
    );
    assert_eq!(
        calls, 0,
        "and the decision is taken before the call, not hoped for after it"
    );
}

/// And a turn that did something is acknowledged, in the one reply that also gives
/// the answer.
#[tokio::test]
async fn a_turn_that_did_something_is_still_acknowledged() {
    let (replies, calls) = acknowledgements(NarrationConfig::conservative(), true).await;
    assert_eq!(
        replies,
        ["Right. Quite a lot."],
        "a receipt is material and always was"
    );
    assert_eq!(calls, 1);
}

/// A refusal beside an answer is said once: by the acknowledgement, which owns what
/// the turn did and did not do. The answer is written from its own facts.
#[tokio::test]
async fn a_refusal_beside_an_answer_is_said_once() {
    let turn = turn_one();
    let text = "Add a new traveler";
    // A record token that names nothing: the act is refused at its target.
    let understanding = UnderstandingBuilder::of(text)
        .ask(text)
        .apply(
            operations::SET_NAME,
            TargetToken::from("tok_a_document_nobody_has"),
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("To open a traveler record I need to know who it is.")
        .acknowledging("That could not be done.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();

    let acknowledged = provider.calls_for(ModelPurpose::Acknowledge);
    assert!(
        acknowledged[0].user_text().contains("not_done"),
        "the acknowledgement carries the refusal"
    );
    let answering = provider.calls_for(ModelPurpose::Answer)[0].user_text();
    assert!(
        !answering.contains("act_refused"),
        "and the answer is not handed a second copy of it: {answering}"
    );
    // The deterministic notice stands beside both.
    assert!(
        answered
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Notice(_))),
        "the notice that says the act was refused is still there: {:?}",
        answered.blocks
    );
}

/// A question the turn answers is given to the acknowledgement with its answer, so the
/// one reply says it once and moves the conversation on.
#[tokio::test]
async fn what_a_case_still_needs_is_said_once_too() {
    let turn = turn_one();
    let text = "what is still missing?";
    let understanding = UnderstandingBuilder::of(text)
        .ask(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": ""}),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("The extra is still missing: tell me what and how much.")
        .acknowledging("The extra is still missing. What is it, and for how much?")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();

    let acknowledging = provider.calls_for(ModelPurpose::Acknowledge)[0].user_text();
    assert!(
        acknowledging.contains(
            "Answers to give in your reply:\n- The extra is still missing: tell me what and how much."
        ),
        "the acknowledgement is given the answer to give: {acknowledging}"
    );
    assert_eq!(
        answered
            .blocks
            .iter()
            .filter(|block| matches!(block, ResponseBlock::Transition(_)))
            .count(),
        1,
        "{:?}",
        answered.blocks
    );
}
