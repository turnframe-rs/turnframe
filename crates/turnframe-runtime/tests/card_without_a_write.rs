//! A card the view requires is created on the turn that requires it, whether or
//! not the turn wrote anything.
//!
//! The first turn of a collection flow changes nothing by construction — it asks
//! a question — and so does a turn whose value the domain refused. For a yes/no
//! whose only writer is a card, a question asked without the card is one the
//! user cannot answer: there is nothing to press. So cards are built from every
//! case in view, as the narration is: the projection taken after its commands
//! ran for a case the turn changed, the one the turn opened with for the rest.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::Understanding;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{REBOOK_CONFIRM_OPTION, awaiting_rebooking_confirmation};

const TEXT: &str = "what is the total?";

fn asking_the_total() -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .ask_about(
            turnframe_core::plan::AnswerBasis::CurrentCommittedState,
            None,
            &["total_amount"],
            TEXT,
        )
        .build()
        .unwrap()
}

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn turn_two() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(2))
}

/// A turn that writes nothing against a case whose phase requires a card.
///
/// The trip sample requires the rebooking card while it is awaiting one,
/// and a turn that only asks a question commits nothing at all.
#[tokio::test]
async fn a_turn_that_writes_nothing_still_gets_the_card_the_view_requires() {
    let turn = turn_one();
    let harness = Harness::builder()
        // The phase requires the card, and the previous turn's card is not
        // there: this is the state a first turn of a flow is in.
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(asking_the_total())
        .without_narration()
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "the turn wrote nothing, which is the whole point"
    );
    assert!(
        answered.blocks.iter().any(|block| matches!(
            block,
            turnframe_core::response::ResponseBlock::Interaction(_)
        )),
        "the question the phase asks arrives with something to press"
    );
    let card = harness.blocking_card("trip", "trip-1").await;
    assert!(
        card.payload
            .options
            .iter()
            .any(|option| option.id.as_str() == REBOOK_CONFIRM_OPTION),
        "and it is the card this phase asks with"
    );
}

/// And a case that already holds a blocking card does not get a second one:
/// every turn looks at every case in view, and I5 is what keeps that from
/// becoming a pile of cards.
#[tokio::test]
async fn a_case_already_holding_a_card_is_not_given_another() {
    let turn = turn_one();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(asking_the_total())
        .understands(asking_the_total())
        .without_narration()
        .build()
        .await;

    // The first turn writes nothing and is given the card the phase requires.
    harness.handle(harness.turn(turn, TEXT)).await.unwrap();
    let first_card = harness.blocking_card("trip", "trip-1").await.id;

    // The second writes nothing either, and the case is already waiting.
    harness
        .handle(harness.turn(turn_two(), TEXT))
        .await
        .unwrap();
    let open = harness.open_cards("trip", "trip-1").await;
    assert_eq!(
        open.iter().filter(|card| card.blocking).count(),
        1,
        "one card, not one per turn that looked at the case"
    );
    assert_eq!(
        harness.blocking_card("trip", "trip-1").await.id,
        first_card,
        "and it is the same card: a replacement keeps the count at one while \
         taking the one the user is looking at out from under them, and any \
         answer already bound to it goes stale"
    );
}
