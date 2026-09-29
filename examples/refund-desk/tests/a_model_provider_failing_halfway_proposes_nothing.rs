//! The model provider fails halfway through the reading: the part it failed on is not
//! understood, nothing is proposed, nothing is committed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;

#[tokio::test]
async fn a_model_provider_failing_halfway_proposes_nothing() {
    let (_, desk) = runs::model_down().await.unwrap();
    assert!(desk.orders.entries().is_empty(), "nothing was committed");
    assert!(desk.card(381).await.is_none());
}
