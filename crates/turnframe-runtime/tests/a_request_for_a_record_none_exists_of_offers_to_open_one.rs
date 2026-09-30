//! A request for something only a record of a workflow can do, when none of its records
//! exists, is not left unread: understanding is offered the operation, the reply says there
//! is none yet and offers to open one, and «yes» next turn takes that offer up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, narration};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::ActTarget;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::operations;

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

#[tokio::test]
async fn a_request_for_a_record_none_exists_of_offers_to_open_one() {
    let text = "add a bag at 40 euros";
    let bag = UnderstandingBuilder::of(text)
        .apply_to(
            operations::ADD_EXTRA,
            ActTarget::NotListed {
                workflow: "trip".into(),
                words: None,
            },
            serde_json::json!({ "description": "Bag", "quantity": 1,
                                "unit_price": { "minor": 4_000, "currency": "EUR" } }),
            text,
        )
        .build()
        .unwrap();
    let yes = UnderstandingBuilder::of("yes").build().unwrap();
    let provider = narrating()
        .acknowledging("There is no trip yet. Open one?")
        .build_shared();
    let harness = Harness::builder()
        .understands(bag)
        .understands(yes)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let first = harness.handle(harness.turn(turn(1), text)).await.unwrap();

    let seen = harness.understander.seen();
    let trips = seen[0]
        .workflows
        .iter()
        .find(|workflow| workflow.key.as_str() == "trip")
        .unwrap();
    assert!(
        trips.spec(&operations::ADD_EXTRA.into()).is_some(),
        "understanding is offered what only a trip can do"
    );
    let said = narration(&first);
    let notices = format!("{:?}", first.blocks);
    assert!(
        notices.contains("There is no trip yet"),
        "{said}\n{notices}"
    );
    let opening = first
        .offers
        .iter()
        .find(|offer| offer.operation.as_str() == operations::OPEN)
        .unwrap_or_else(|| panic!("an offer to open one: {:?}", first.offers));
    assert_eq!(opening.words, "Open a new trip.");

    harness.handle(harness.turn(turn(2), "yes")).await.unwrap();
    let offered = &harness.understander.seen()[1].offers;
    let [brief] = offered.as_slice() else {
        panic!("the offer reaches the next turn: {offered:?}");
    };
    assert_eq!(brief.act.operation.as_str(), operations::OPEN);
    assert_eq!(brief.act.record, None, "a record still to create");
}
