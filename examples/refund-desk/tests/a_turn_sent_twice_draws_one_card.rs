//! The app resends the same turn: it is the same turn, so one refund is asked for, one card
//! drawn, and one refund leaves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;

#[tokio::test]
async fn a_turn_sent_twice_draws_one_card() {
    let (_, desk) = runs::sent_twice().await.unwrap();
    let events = desk.events(381);
    let requested = events
        .iter()
        .filter(|e| *e == "order.refund_requested")
        .count();
    let sent = events.iter().filter(|e| *e == "order.refund_sent").count();
    assert_eq!((requested, sent), (1, 1), "{events:?}");
}
