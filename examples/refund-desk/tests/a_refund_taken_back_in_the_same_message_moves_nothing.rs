//! «Refund it. Actually don't, just tell me whether it's eligible»: the refund is taken back
//! within the message, and the question is answered from the order.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::record::Kind;
use refund_desk::runs;

#[tokio::test]
async fn a_refund_taken_back_in_the_same_message_moves_nothing() {
    let (run, desk) = runs::take_back().await.unwrap();
    assert!(desk.orders.entries().is_empty(), "nothing was committed");
    assert!(desk.card(381).await.is_none());
    assert!(
        run.frames.iter().any(|frame| frame.kind == Kind::Reply),
        "the question is answered"
    );
}
