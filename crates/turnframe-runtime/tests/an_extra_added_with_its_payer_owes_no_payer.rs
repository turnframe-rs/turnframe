//! «Add a checked bag at 40 euros, paid by the airline» is one act in the trip sample: the
//! extra is added with its payer, owes none, and the receipt says who pays.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::response::ResponseBlock;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{Payer, TripObligation, complete_case, operations};

const TEXT: &str = "add a checked bag at 40 euros, paid by the airline";

#[tokio::test]
async fn an_extra_added_with_its_payer_owes_no_payer() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let understanding = UnderstandingBuilder::of(TEXT)
        .apply(
            operations::ADD_EXTRA,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"description": "Checked bag", "quantity": 1,
                               "unit_price": {"minor": 4_000, "currency": "EUR"},
                               "payer": "airline"}),
            TEXT,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(understanding)
        .without_narration()
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    let before = complete_case().extras.len();
    let state = harness.trip_state("trip-1").unwrap();
    assert_eq!(state.extras.len(), before + 1, "the extra is added");
    assert_eq!(state.extras.last().unwrap().payer, Some(Payer::Airline));
    assert!(
        !state
            .open_obligations()
            .iter()
            .any(|owed| matches!(owed, TripObligation::AssignPayer { .. })),
        "{:?}",
        state.open_obligations()
    );
    let receipts: Vec<String> = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Receipt(receipt) => Some(receipt.receipt.body.default.clone()),
            _ => None,
        })
        .collect();
    assert!(
        receipts
            .iter()
            .any(|body| body.ends_with("each, paid by the airline.")),
        "{receipts:?}"
    );
}
