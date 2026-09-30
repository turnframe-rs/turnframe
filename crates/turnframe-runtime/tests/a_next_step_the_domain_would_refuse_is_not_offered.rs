//! A next step is offered only when the domain would accept it now: the runtime dry-runs it
//! against the record before the reply offers it. The trip offers the quoted leg's rebooking,
//! and on a leg the traveler asked to keep the domain refuses it, so it is not offered.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, token_for};
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{TripState, operations, with_offer};

/// What the reply was written from, for a turn naming a complete trip in `state`.
async fn written_from(state: TripState) -> String {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let text = "call it Porto";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({ "value": "Porto" }),
            text,
        )
        .build()
        .unwrap();
    let provider = narrating().build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, state)
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();
    provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .next()
        .expect("the reply was written")
        .user_text()
}

#[tokio::test]
async fn a_next_step_the_domain_would_refuse_is_not_offered() {
    let quoted = written_from(with_offer(1)).await;
    assert!(
        quoted.contains("Rebook"),
        "the quoted leg is offered: {quoted}"
    );

    let mut kept = with_offer(1);
    kept.legs[0].protected = true;
    let kept = written_from(kept).await;
    assert!(kept.contains("Add another extra"), "{kept}");
    assert!(
        !kept.contains("Rebook"),
        "a refused step is not offered: {kept}"
    );
}

#[tokio::test]
async fn a_step_whose_values_are_asked_later_is_tried_with_its_examples() {
    let mut full = with_offer(1);
    while full.extras.len() < 4 {
        let mut another = full.extras[0].clone();
        another.extra_id = uuid::Uuid::from_u128(full.extras.len() as u128 + 100);
        full.extras.push(another);
    }
    let full = written_from(full).await;
    assert!(
        !full.contains("Add another extra"),
        "a trip that takes no more extras is not offered one: {full}"
    );
}
