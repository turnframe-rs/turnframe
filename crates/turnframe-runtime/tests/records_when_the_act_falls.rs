//! The account's records never reach the acknowledgement, whether its act fell or
//! landed.
//!
//! Retrieval hangs off a question: no question, no sources. The acknowledgement is
//! written from the turn's outcome alone, and the less it is handed the less it
//! invents; a turn whose act fell says so and asks what the record needs next.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{FixedKnowledge, Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

const RECORDS: &str = "Le bozze ancora aperte di questo utente, 1 in tutto: ABC-FA/004-2026.";

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// Runs one turn whose single act either falls or lands, and returns everything
/// the acknowledgement was told.
async fn acknowledgement_brief(text: &str, name: &str) -> String {
    let turn = turn_one();
    let understood = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({ "value": name }),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Noted.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understood)
        .provider(Arc::clone(&provider))
        .knowledge(Arc::new(FixedKnowledge::saying(RECORDS)))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    provider
        .calls_for(ModelPurpose::Acknowledge)
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
        .unwrap_or_default()
}

/// The act fell: the refusal and the next question are the reply, not the registry.
#[tokio::test]
async fn the_records_stay_out_when_the_act_falls() {
    // Blank is the value this domain refuses, so the turn ends with a refusal
    // and no receipt.
    let brief = acknowledgement_brief("ho altre bozze?", "   ").await;
    assert!(
        !brief.contains(RECORDS),
        "the acknowledgement is written from the outcome; it was told:\n{brief}"
    );
    assert!(
        brief.contains("\"not_done\": [\n    \""),
        "and the outcome says what fell: {brief}"
    );
}

/// The act landed, so the turn has its own news: the records stay out.
#[tokio::test]
async fn a_turn_that_wrote_keeps_the_records_out_of_the_acknowledgement() {
    // A value the domain accepts, so the act lands.
    let brief = acknowledgement_brief("nome: offsite a Lisbona", "offsite a Lisbona").await;
    assert!(
        !brief.contains(RECORDS),
        "a turn that wrote something has news of its own and must not be handed \
         the whole registry to write about; it was told:\n{brief}"
    );
}
