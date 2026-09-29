//! The turn where the user answers something the assistant said.
//!
//! A turn whose acts reached no record, such as a «yes», stays on the record the
//! last reply was about, and asks its next thing there: the conversation does not
//! stall. A turn with a record of its own is about that one only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::Understanding;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// What the `n`th acknowledgement was written from, as the model read it.
fn acknowledged_from(provider: &ScriptedProvider, n: usize) -> String {
    provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .nth(n)
        .expect("the composer called the acknowledgement stage")
        .user_text()
}

/// «yes», understood as asking for nothing and doing nothing.
fn nothing_of_its_own() -> Understanding {
    UnderstandingBuilder::of("yes").build().unwrap()
}

/// Two turns: one that writes, then one that does nothing at all.
async fn a_write_then_a_yes(second: Understanding) -> (Harness, Arc<ScriptedProvider>) {
    let first = turn(1);
    let text = "call the name Lisbon";
    let wrote = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(first, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let provider = narrating()
        .acknowledging("Here is where that leaves things.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(wrote)
        .understands(second)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(first, text)).await.unwrap();
    (harness, provider)
}

/// A turn with nothing of its own asks the next thing of the record the last reply
/// was about, by name.
#[tokio::test]
async fn a_turn_with_no_subject_of_its_own_is_still_on_the_last_one() {
    let (harness, provider) = a_write_then_a_yes(nothing_of_its_own()).await;

    harness.handle(harness.turn(turn(2), "yes")).await.unwrap();

    let second = acknowledged_from(&provider, 1);
    assert!(
        second.contains("\"record\": \"Trip 1\""),
        "the record the previous turn wrote to is what this turn asks about: {second}"
    );
    assert!(
        !second.contains("\"done\": [\n    \""),
        "and nothing is reported done: {second}"
    );
}

/// A turn with a name of its own does NOT inherit the previous one, or a
/// record the conversation has left would be offered back to the writer.
#[tokio::test]
async fn a_turn_about_something_else_does_not_carry_the_old_subject() {
    let first = turn(1);
    let second = turn(2);
    let one = "call the name Lisbon";
    let two = "and on Trip 2, put Training";
    let wrote_one = UnderstandingBuilder::of(one)
        .apply(
            operations::SET_NAME,
            token_for(first, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            one,
        )
        .build()
        .unwrap();
    let wrote_two = UnderstandingBuilder::of(two)
        .apply(
            operations::SET_NAME,
            token_for(second, "trip", "trip-2"),
            serde_json::json!({"value": "Training"}),
            two,
        )
        .build()
        .unwrap();
    let provider = narrating().acknowledging("Done.").build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip("trip-2", "Trip 2", 3, incomplete_case())
        .understands(wrote_one)
        .understands(wrote_two)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    harness.handle(harness.turn(first, one)).await.unwrap();
    harness.handle(harness.turn(second, two)).await.unwrap();

    let second = acknowledged_from(&provider, 1);
    assert!(
        second.contains("\"record\": \"Trip 2\""),
        "the turn's own subject is what it asks about: {second}"
    );
    assert!(
        !second.contains("Trip 1"),
        "and the record the conversation left is not offered back: {second}"
    );
}

/// The stage that speaks is given the conversation: what is being talked about lives
/// only there.
#[tokio::test]
async fn the_stage_that_speaks_is_given_the_conversation() {
    let (harness, provider) = a_write_then_a_yes(nothing_of_its_own()).await;
    harness.handle(harness.turn(turn(2), "yes")).await.unwrap();

    let second = acknowledged_from(&provider, 1);
    assert!(
        second.contains("- user: call the name Lisbon"),
        "the user's earlier message is there: {second}"
    );
    assert!(
        second.contains("- assistant: Right, here is where that leaves things."),
        "and so is what we replied: {second}"
    );
}
