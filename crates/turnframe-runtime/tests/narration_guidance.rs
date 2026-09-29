//! What a workflow gets to say to the stage that speaks.
//!
//! A workflow briefs each writing stage separately, per phase: the acknowledgement
//! through `transition_briefing`, each answer through `answer_briefing`. Guidance
//! goes to the records a stage is about: the acknowledgement's are the records the
//! turn's acts reached, an answer's the records its question is about. A record in
//! view that neither is about gives no instruction.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::plan::AnswerBasis;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{
    awaiting_rebooking_confirmation, incomplete_case, operations,
};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// What the first call of one stage was written from, as the model read it.
fn input_of(provider: &Arc<ScriptedProvider>, purpose: ModelPurpose) -> String {
    provider
        .calls_for(purpose)
        .into_iter()
        .next()
        .expect("the stage ran")
        .user_text()
}

/// The inputs of both stages, on a turn that sets the name and asks about the
/// same trip.
async fn both_stages() -> (String, String) {
    let turn = turn_one();
    let text = "Set the name to Lisbon, and what is the total?";
    let trip = token_for(turn, "trip", "trip-1");
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            trip.clone(),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .ask_about(
            AnswerBasis::CurrentCommittedState,
            Some(trip),
            &["total_amount"],
            "and what is the total?",
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("Thirty euro.")
        .acknowledging("Noted. Thirty euro.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();
    (
        input_of(&provider, ModelPurpose::Acknowledge),
        input_of(&provider, ModelPurpose::Answer),
    )
}

/// The workflow's instruction for the phase reaches the stage that speaks.
#[tokio::test]
async fn a_workflow_briefs_the_stage_that_speaks() {
    let (acknowledging, _) = both_stages().await;
    assert!(
        acknowledging.contains("one thing at a time"),
        "{acknowledging}"
    );
    assert!(
        !acknowledging.contains("confirmation card"),
        "and it is the phase's, not the workflow's in general: {acknowledging}"
    );
}

/// The two stages are addressed separately, and each gets its own.
#[tokio::test]
async fn each_writing_stage_gets_the_guidance_addressed_to_it() {
    let (acknowledging, answering) = both_stages().await;
    assert!(answering.contains("currency"), "{answering}");
    assert!(
        !answering.contains("one thing at a time"),
        "an instruction about asking has no business in the block that \
         answers: {answering}"
    );
    assert!(
        !acknowledging.contains("currency"),
        "and the reverse: {acknowledging}"
    );
}

/// A record the turn never touched does not instruct the acknowledgement.
///
/// The bystander waits on its rebooking card, whose phase has a guidance of its own.
#[tokio::test]
async fn a_case_the_turn_did_not_engage_sends_no_guidance() {
    let turn = turn_one();
    let text = "Set the name to Lisbon";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Noted.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip("trip-2", "Trip 2", 3, awaiting_rebooking_confirmation())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    let acknowledging = input_of(&provider, ModelPurpose::Acknowledge);
    assert!(
        acknowledging.contains("one thing at a time"),
        "the case the act reached instructs the writer: {acknowledging}"
    );
    assert!(
        !acknowledging.contains("confirmation card"),
        "and the bystander does not: {acknowledging}"
    );
    assert!(
        !acknowledging.contains("Trip 2"),
        "nor is what it waits for put in front of the writer: {acknowledging}"
    );
}

/// A question naming no record gets no instruction from a record of another thread of
/// work, and one of this thread still briefs it: the pair differs only in the
/// declaration.
#[tokio::test]
async fn a_record_of_another_thread_sends_no_guidance() {
    async fn answering(reachable_only: bool) -> String {
        let text = "what is the total of this one?";
        let understanding = UnderstandingBuilder::of(text)
            .ask_about(
                AnswerBasis::CurrentCommittedState,
                None,
                &["total_amount"],
                text,
            )
            .build()
            .unwrap();
        let provider = ScriptedProvider::builder("scripted", "model-1")
            .answering("Thirty euro.")
            .build_shared();
        let seeded = Harness::builder().trip("trip-1", "Trip 1", 3, incomplete_case());
        let seeded = if reachable_only {
            seeded.reachable_only()
        } else {
            seeded
        };
        let harness = seeded
            .understands(understanding)
            .provider(Arc::clone(&provider))
            .build()
            .await;
        harness
            .handle(harness.turn(turn_one(), text))
            .await
            .unwrap();
        input_of(&provider, ModelPurpose::Answer)
    }
    let other_thread = answering(true).await;
    assert!(
        !other_thread.contains("currency"),
        "a record this turn has only because it is reachable gives no orders about \
         how to speak: {other_thread}"
    );
    let this_thread = answering(false).await;
    assert!(this_thread.contains("currency"), "{this_thread}");
}

/// And the record is still in view for understanding, so a turn that names it can
/// make it its name: scoping the writer is not hiding the record.
#[tokio::test]
async fn the_record_of_another_thread_is_reachable_by_name() {
    let turn = turn_one();
    let text = "what can I do with Trip 1?";
    let understanding = UnderstandingBuilder::of(text)
        .ask_about(
            AnswerBasis::CurrentCommittedState,
            None,
            &["total_amount"],
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("Thirty euro.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .reachable_only()
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    let seen = harness.understander.seen();
    let shown = seen.first().expect("the turn was understood");
    let labels: Vec<&str> = shown
        .workflows
        .iter()
        .flat_map(|workflow| workflow.records.iter())
        .map(|record| record.label.as_str())
        .collect();
    assert!(
        labels.contains(&"Trip 1"),
        "a turn that names it reaches it, or nobody could ever pick it up \
         again: {labels:?}"
    );
}
