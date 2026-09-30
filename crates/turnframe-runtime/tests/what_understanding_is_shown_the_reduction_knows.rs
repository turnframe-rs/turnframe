//! Every operation understanding is shown is one the reduction knows: with none of a
//! workflow's records in view, an act asking what one could do reaches the domain, which
//! decides, and is never refused as asking for something unknown.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, notice_codes};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::ActTarget;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::operations;

const TEXT: &str = "open a trip and call it Lisbon offsite";

#[tokio::test]
async fn what_understanding_is_shown_the_reduction_knows() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let opening = UnderstandingBuilder::of(TEXT).open(
        operations::OPEN,
        "trip",
        serde_json::json!({}),
        "open a trip",
    );
    let opened = opening.last_act().unwrap();
    let mut understanding = opening
        .apply_to(
            operations::SET_NAME,
            ActTarget::SameTurn { act: opened },
            serde_json::json!({"value": "Lisbon offsite"}),
            "call it Lisbon offsite",
        )
        .build()
        .unwrap();
    understanding.acts[1].depends_on.push(opened);
    let harness = Harness::builder()
        .understands(understanding)
        .without_narration()
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    let shown = &harness.understander.seen()[0];
    let trips = shown
        .workflows
        .iter()
        .find(|workflow| workflow.key.as_str() == "trip")
        .unwrap();
    assert!(trips.spec(&operations::SET_NAME.into()).is_some());
    let codes = notice_codes(&answered);
    assert!(
        !codes
            .iter()
            .any(|code| code == "turnframe.operation.unknown"),
        "{codes:?}"
    );
}
