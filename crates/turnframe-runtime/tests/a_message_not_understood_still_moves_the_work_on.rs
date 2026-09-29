//! A message nothing on offer does is reported, and the reply still asks for the next
//! thing a record in view needs, so the conversation does not stop on «I did not
//! understand».
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::NotUnderstoodReason;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::incomplete_case;

const TEXT: &str = "you should have done that already";

#[tokio::test]
async fn a_message_not_understood_still_moves_the_work_on() {
    let understanding = UnderstandingBuilder::of(TEXT)
        .not_understood(NotUnderstoodReason::NoOperation, TEXT)
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("I did not follow that. What should the name be?")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), TEXT))
        .await
        .unwrap();

    let asked = provider.calls_for(ModelPurpose::Acknowledge)[0].user_text();
    assert!(
        asked.contains("\"ask\"") && asked.contains("Trip 1"),
        "the reply is told what the trip needs next: {asked}"
    );
}
