//! A workflow saying that a case has a document.
//!
//! Whether a case has a document is a question about the state its events folded
//! into, not about the events, so a workflow declares it from the projection and
//! the runtime puts it in front of the user: a confirmation step is for seeing
//! what is being authorised.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::locale::Locale;
use turnframe_core::response::{AssistantTurn, ResponseBlock};
use turnframe_core::understanding::Understanding;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{
    awaiting_rebooking_confirmation, incomplete_case, operations, with_offer,
};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// The artifacts a turn put in front of the user, as `(kind, label)`.
fn artifacts(turn: &AssistantTurn) -> Vec<(String, String)> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Artifact(view) => Some((
                view.artifact.kind.clone(),
                view.artifact
                    .label
                    .resolve(&Locale::from("en-GB"))
                    .to_owned(),
            )),
            _ => None,
        })
        .collect()
}

/// The block ids of a turn's artifacts.
fn artifact_ids(turn: &AssistantTurn) -> Vec<String> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Artifact(view) => Some(view.block_id.to_string()),
            _ => None,
        })
        .collect()
}

const REVIEW: &str = "show me the rebooking card";

/// The first turn, asking for the rebooking card on the seeded trip.
fn review() -> Understanding {
    UnderstandingBuilder::of(REVIEW)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(turn_one(), "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            REVIEW,
        )
        .build()
        .unwrap()
}

/// A turn that asks for the rebooking card on the seeded trip.
async fn reviewing(state: turnframe_test::workflows::trip::TripState) -> AssistantTurn {
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, state)
        .understands(review())
        .provider(narrating().build_shared())
        .build()
        .await;
    harness
        .handle(harness.turn(turn_one(), REVIEW))
        .await
        .unwrap()
}

/// The document the case has reaches the reply.
#[tokio::test]
async fn a_case_with_a_document_puts_it_in_front_of_the_user() {
    let found = artifacts(&reviewing(with_offer(1)).await);
    assert_eq!(
        found,
        vec![("itinerary_pdf".to_owned(), "Itinerary preview".to_owned())],
        "the point of a confirmation step is seeing what is being authorised"
    );
}

/// The revision is part of what identifies the block.
///
/// Which is what makes "the same document at the same revision" a thing the
/// runtime can recognise — and what brings the chip back when the document
/// changes, because a changed document is at a new revision and so is a new
/// block.
#[tokio::test]
async fn the_block_names_the_revision_the_document_is_at() {
    let answered = reviewing(with_offer(1)).await;
    let case_ref = answered
        .blocks
        .iter()
        .find_map(|block| match block {
            ResponseBlock::Interaction(card) => Some(card.view.case_ref.clone()),
            _ => None,
        })
        .expect("the rebooking card names the case it is bound to");
    assert_eq!(
        artifact_ids(&answered),
        vec![format!(
            "artifact:trip:trip-1:{}:itinerary:trip-1",
            case_ref.expected_revision
        )],
        "same document, same revision, same block"
    );
}

/// And a case with none says none.
#[tokio::test]
async fn a_case_with_no_document_adds_no_block() {
    // The same act on an incomplete trip is refused, so the phase never
    // reaches the one that has a preview.
    let found = artifacts(&reviewing(incomplete_case()).await);
    assert!(found.is_empty(), "{found:?}");
}

/// A document that has not changed is not announced twice.
///
/// A workflow that declares a document in a phase declares it on every turn of
/// that phase. The identity is the case, its revision and the artifact, so the
/// same document at the same revision is the same block and is sent once.
#[tokio::test]
async fn a_document_that_has_not_changed_is_announced_once() {
    let second = UnderstandingBuilder::of("e allora?")
        .ask("e allora?")
        .build()
        .unwrap();
    let provider = narrating().answering("Nothing has changed.").build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review())
        .understands(second)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let first = harness
        .handle(harness.turn(turn_one(), REVIEW))
        .await
        .unwrap();
    assert_eq!(
        artifacts(&first).len(),
        1,
        "the document is announced when the phase it belongs to is reached"
    );

    let again = harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(2)), "e allora?"))
        .await
        .unwrap();
    assert!(
        artifacts(&again).is_empty(),
        "and not again while it is the same document at the same revision: {:?}",
        artifacts(&again)
    );
}

/// And not on the turn that answers a card, which is the turn it was reported
/// for: a click costs no model call, and still knows what the conversation
/// already carries.
#[tokio::test]
async fn a_document_is_not_re_announced_by_a_turn_that_answers_a_card() {
    let provider = narrating()
        // The click's own turn: nothing runs, and the reply says the
        // instruction was declined, which is material.
        .acknowledging("All right.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review())
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let first = harness
        .handle(harness.turn(turn_one(), REVIEW))
        .await
        .unwrap();
    assert_eq!(artifacts(&first).len(), 1);

    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1");
    let answered = harness
        .handle(harness.click(
            TurnId::from(uuid::Uuid::from_u128(2)),
            card.id,
            turnframe_test::workflows::trip::REBOOK_DECLINE_OPTION,
            revision.0,
        ))
        .await
        .unwrap();
    assert!(
        artifacts(&answered).is_empty(),
        "the same document at the same revision, and a turn with no \
         model call: {:?}",
        artifacts(&answered)
    );
}

/// A case the turn was not about does not put its document on screen.
///
/// The same rule guidance and the unavailable fact follow: a conversation about
/// one record is not an occasion to show another one's document.
#[tokio::test]
async fn a_document_of_a_case_the_turn_ignored_stays_out() {
    let turn = turn_one();
    let text = "Set the name to Lisbon";
    let understood = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        // Waiting on its rebooking card, so it has a preview — and nothing in this
        // turn is about it.
        .trip("trip-2", "Trip 2", 3, awaiting_rebooking_confirmation())
        .understands(understood)
        .provider(narrating().build_shared())
        .build()
        .await;
    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();
    assert!(
        artifacts(&answered).is_empty(),
        "{:?}",
        artifacts(&answered)
    );
}
