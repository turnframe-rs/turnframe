//! With no attack, the refund waits on its card, the click sends it, and the provider's
//! answer records it: one refund, recorded once.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::desk::{Desk, Provider, token};
use refund_desk::order::{OrderCommand, REFUND_OPTION, operations};
use turnframe::event::ExternalStatus;
use turnframe::testing::providers::UnderstandingBuilder;

#[tokio::test]
async fn the_clean_run_refunds_once() {
    let text = "Refund order 381 for €129";
    let understood = UnderstandingBuilder::of(text)
        .apply(
            operations::REQUEST_REFUND,
            token(1, 381),
            serde_json::json!({ "amount": { "minor": 12_900, "currency": "EUR" } }),
            text,
        )
        .build()
        .unwrap();
    let desk = Desk::scripted(vec![understood], &[]).await.unwrap();

    desk.orchestrator
        .handle_turn(desk.send(1, text))
        .await
        .unwrap();
    let card = desk.card(381).await.expect("the refund waits on its card");
    desk.orchestrator
        .handle_turn(desk.click(2, &card, REFUND_OPTION))
        .await
        .unwrap();
    desk.dispatch(Provider::Accepts).await.unwrap();
    desk.outside(
        381,
        "answer-1",
        OrderCommand::RecordProviderOutcome {
            status: ExternalStatus::Accepted,
            reference: Some("re_381".to_owned()),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        desk.events(381),
        [
            "order.refund_requested",
            "order.refund_sent",
            "order.provider_outcome_recorded"
        ]
    );
}
