//! What the acknowledgement is shown of the message that just arrived.
//!
//! It is shown it only when nothing the turn asked for was left undone: a refusal
//! anywhere in the turn takes the message away, even when something else committed,
//! because beside a refusal the words that asked for it read as the thing done.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// Runs one turn whose single act sets the name to `value`, and returns the
/// provider that recorded what each stage was asked.
async fn a_turn_setting_the_subject_to(value: &str, text: &str) -> Arc<ScriptedProvider> {
    let turn = turn_one();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({ "value": value }),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Noted.")
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

/// One turn asking for two things: a travel date the domain accepts, and a name
/// it refuses. One receipt, one refusal, one message.
async fn a_turn_writing_and_refusing() -> Arc<ScriptedProvider> {
    let turn = turn_one();
    let text = "Clear the name and make it fly on 2026-12-31";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_TRAVEL_DATE,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({ "value": "2026-12-31" }),
            "make it fly on 2026-12-31",
        )
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({ "value": "" }),
            "Clear the name",
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Noted.")
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

/// What the acknowledgement was written from, as the model read it.
fn acknowledged_from(provider: &Arc<ScriptedProvider>) -> String {
    provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .next()
        .expect("the composer called the acknowledgement")
        .user_text()
}

/// A turn that recorded nothing is not shown the message it might confirm.
///
/// The stage is asked what the turn DID; handed the user's sentence beside a
/// refusal, it writes the sentence back as a confirmation. The conversation still
/// travels: see `the_stage_that_speaks_is_given_the_conversation`.
#[tokio::test]
async fn a_turn_that_recorded_nothing_is_not_shown_what_the_user_said() {
    let provider = a_turn_setting_the_subject_to("", "Clear the name").await;
    let shown = acknowledged_from(&provider);
    assert!(
        shown.contains("\"not_done\": [\n    \""),
        "the turn committed nothing and the refusal is what it has to say: {shown}"
    );
    assert!(
        !shown.contains("Clear the name"),
        "the user's words are not among what it was shown: {shown}"
    );
}

/// A receipt beside a refusal still takes the message away: the receipt
/// authorizes «I have», and the message would supply the refused act as done.
#[tokio::test]
async fn a_turn_with_a_refusal_in_it_is_not_shown_what_the_user_said() {
    let provider = a_turn_writing_and_refusing().await;
    let shown = acknowledged_from(&provider);
    assert!(
        shown.contains("\"done\": [\n    \""),
        "the turn committed one of its two acts: {shown}"
    );
    assert!(
        shown.contains("\"not_done\": [\n    \""),
        "and refused the other: {shown}"
    );
    assert!(
        !shown.contains("Clear the name"),
        "so the words that asked for the refused one do not travel: {shown}"
    );
}

/// A turn that recorded something is shown it, because the message is then the
/// words behind a value the reply may name.
#[tokio::test]
async fn a_turn_that_recorded_something_is_shown_what_the_user_said() {
    let provider = a_turn_setting_the_subject_to("Lisbon", "Set the name to Lisbon").await;
    let shown = acknowledged_from(&provider);
    assert!(
        shown.contains("The user wrote: «Set the name to Lisbon»"),
        "the turn wrote, so what the user said travels: {shown}"
    );
}
