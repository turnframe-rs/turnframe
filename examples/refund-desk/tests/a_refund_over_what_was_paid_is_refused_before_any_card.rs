//! The model reads €1,290 for €129: the domain refuses more than was paid, before any card.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;

#[tokio::test]
async fn a_refund_over_what_was_paid_is_refused_before_any_card() {
    let (run, desk) = runs::wrong_amount().await.unwrap();
    assert!(desk.orders.entries().is_empty(), "nothing was committed");
    assert!(desk.card(381).await.is_none());
    assert!(
        run.frames.iter().all(|frame| frame.card.is_none()),
        "no card was drawn"
    );
}
