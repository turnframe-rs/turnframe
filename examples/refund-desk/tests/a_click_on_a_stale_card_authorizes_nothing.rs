//! A colleague refunds part of the order while the card is open: the click on the card drawn
//! before authorizes nothing, and the card drawn again shows what is left.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;

#[tokio::test]
async fn a_click_on_a_stale_card_authorizes_nothing() {
    let (run, desk) = runs::stale_card().await.unwrap();
    assert!(!desk.events(381).contains(&"order.refund_sent".to_owned()));
    let card = desk.card(381).await.expect("a card is open again");
    assert_eq!(card.case_ref.expected_revision, desk.revision(381));
    let redrawn = run
        .frames
        .iter()
        .filter_map(|frame| frame.card.as_ref())
        .next_back()
        .expect("the card drawn again is recorded");
    assert_eq!(
        redrawn.entries.len(),
        1,
        "it shows the amount it was capped to"
    );
}
