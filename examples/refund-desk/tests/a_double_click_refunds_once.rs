//! Refund clicked twice: the second click is the first one's, and one refund leaves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;

#[tokio::test]
async fn a_double_click_refunds_once() {
    let (_, desk) = runs::double_click().await.unwrap();
    let sent = desk
        .events(381)
        .into_iter()
        .filter(|event| event == "order.refund_sent")
        .count();
    assert_eq!(sent, 1);
    let rows = desk.outbox(2).await.unwrap().len() + desk.outbox(3).await.unwrap().len();
    assert_eq!(rows, 1, "one row in the outbox");
}
