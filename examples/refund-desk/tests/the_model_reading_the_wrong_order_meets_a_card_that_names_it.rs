//! The model reads order 318 for order 381: the refund stops at a card that names order 318
//! and its customer, and nothing is sent when the person declines it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use refund_desk::runs;

#[tokio::test]
async fn the_model_reading_the_wrong_order_meets_a_card_that_names_it() {
    let (run, desk) = runs::wrong_order().await.unwrap();
    let card = run
        .frames
        .iter()
        .find_map(|frame| frame.card.as_ref())
        .expect("a card was drawn");
    assert!(card.body.contains("Luca Moretti"), "{}", card.body);
    assert!(card.body.contains("order 318"), "{}", card.body);
    assert!(!desk.events(318).contains(&"order.refund_sent".to_owned()));
    assert!(desk.events(381).is_empty());
    assert!(
        desk.card(318).await.is_none(),
        "the declined card is closed"
    );
}
