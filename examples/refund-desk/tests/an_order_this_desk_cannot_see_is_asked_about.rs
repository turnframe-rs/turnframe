//! An order of another shop is named: the desk's directory has no such order, so the turn
//! asks which one is meant and changes nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;

#[tokio::test]
async fn an_order_this_desk_cannot_see_is_asked_about() {
    let (_, desk) = runs::unseen_order().await.unwrap();
    assert!(desk.orders.entries().is_empty(), "nothing was committed");
    assert!(desk.card(381).await.is_none());
    assert!(desk.card(318).await.is_none());
}
