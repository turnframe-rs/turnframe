//! An act the domain accepted, and that changed nothing, reaches the writer.
//!
//! A workflow that compiles no commands is giving a legitimate answer: «that is
//! already so». Like a refusal, it must reach the writing stage as a fact, or
//! the writer gets a brief indistinguishable from a turn that asked for nothing
//! and the user reads silence as success.
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

const NAME: &str = "Offsite di settembre";

/// Everything the writing stage was told, as one string, for a turn asking for
/// the name the draft already carries.
async fn narration_brief() -> String {
    let turn = turn_one();
    let text = "chiama il viaggio: Offsite di settembre";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({ "value": NAME }),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Va bene.")
        .build_shared();
    let mut already = incomplete_case();
    already.name = Some(NAME.to_owned());
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, already)
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .flat_map(|call| {
            call.request
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
                .filter_map(|part| match part {
                    turnframe_provider::request::ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn an_act_that_compiles_no_commands_reaches_the_writer() {
    let brief = narration_brief().await;
    assert!(
        brief.contains("\"not_done\": [\n    \""),
        "the writer was not told the act did nothing: {brief}"
    );
    assert!(
        !brief.contains("\"done\": [\n    \""),
        "and nothing is reported done: {brief}"
    );
}

/// And it is not filed among the refusals: those are counted as refusals, and a
/// no-op is the server saying «that is already so», not «no».
#[tokio::test]
async fn an_act_that_changes_nothing_is_not_a_refusal() {
    let brief = narration_brief().await;
    assert!(
        !brief.contains("act_refused"),
        "a no-op was reported as a refusal: {brief}"
    );
}

/// And it says why, in the workflow's own words: the value already there. Only the
/// workflow knows it. Saying the same value again asks for no other one.
#[tokio::test]
async fn the_writer_is_told_why_nothing_changed() {
    let brief = narration_brief().await;
    assert!(
        brief.contains(NAME),
        "the reason does not name the value that is already there: {brief}"
    );
    assert!(
        !brief.contains("instead") && !brief.contains("Dimmi quale"),
        "the same value said again is read as a wish for another: {brief}"
    );
}
