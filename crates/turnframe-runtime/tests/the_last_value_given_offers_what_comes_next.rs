//! A turn that gives a record the last value it owed ends by offering what the workflow says
//! comes next, instead of stopping at an acknowledgement.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{complete_case, operations};

#[tokio::test]
async fn the_last_value_given_offers_what_comes_next() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let text = "I would rather fly on 30 November 2026";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_TRAVEL_DATE,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "2026-11-30"}),
            text,
        )
        .build()
        .unwrap();
    let mut owing_a_date = complete_case();
    owing_a_date.travel_date = None;
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Noted. Add another extra, or rebook the quoted flight?")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, owing_a_date)
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    let written_from = provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .next()
        .expect("the reply was written")
        .user_text();
    assert!(written_from.contains("\"next\""), "{written_from}");
    assert!(
        written_from.contains("Add another extra."),
        "{written_from}"
    );
    assert!(
        written_from.contains("Rebook the quoted flight"),
        "{written_from}"
    );
}
