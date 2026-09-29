//! Saying what a card is about, and what a record is called.
//!
//! A confirmation the policy engine raises takes its title from the workflow's
//! confirmation subject when it declares one, so the card names what it
//! confirms instead of drawing the per-kind box. And the writer is given the
//! label of each record the turn is about, so it can name the record the card
//! is already on screen for instead of asking which one was meant.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::locale::Locale;
use turnframe_core::response::ResponseBlock;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn english() -> Locale {
    Locale::from("en-GB")
}

/// A turn that asks to cancel the seeded trip, which policy makes a click.
async fn cancelling() -> (
    turnframe_core::response::AssistantTurn,
    Arc<ScriptedProvider>,
) {
    let turn = turn_one();
    let text = "Cancel it";
    let understood = UnderstandingBuilder::of(text)
        .apply(
            operations::WITHDRAW,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!(null),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("All right.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip for Ferri", 3, incomplete_case())
        .understands(understood)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();
    (answered, provider)
}

/// The card the engine raises says what it is confirming.
#[tokio::test]
async fn a_policy_raised_confirmation_names_what_it_confirms() {
    let (answered, _) = cancelling().await;
    let titles: Vec<String> = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Interaction(view) => {
                Some(view.view.title.resolve(&english()).to_owned())
            }
            _ => None,
        })
        .collect();
    assert!(
        titles.iter().any(|title| title.contains("Marta Bianchi")),
        "the engine knows a confirmation is needed and the domain knows what it \
         is about: {titles:?}"
    );
    assert!(
        !titles.iter().any(|title| title == "Confirm this action"),
        "and the per-kind box is what a card with no subject gets, not this \
         one: {titles:?}"
    );
}

/// A workflow that says nothing keeps the box it always had.
///
/// The same card kind as the test above, so the pair is a clean contrast: one
/// operation the domain describes and one it does not, drawing the named card
/// and the per-kind one.
#[tokio::test]
async fn a_confirmation_the_domain_says_nothing_about_is_unchanged() {
    let turn = turn_one();
    let text = "Address it to the other traveler";
    let understood = UnderstandingBuilder::of(text)
        .apply(
            operations::CHANGE_TRAVELER,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"traveler": "Luca Ferri"}),
            text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("All right.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip for Ferri", 3, incomplete_case())
        .understands(understood)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();
    let titles: Vec<String> = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Interaction(view) => {
                Some(view.view.title.resolve(&english()).to_owned())
            }
            _ => None,
        })
        .collect();
    assert!(
        titles.iter().any(|title| title == "Confirm this action"),
        "the default does not move for an operation the workflow says nothing \
         about: {titles:?}"
    );
}

/// And the writer is given a word for the record.
#[tokio::test]
async fn the_writer_is_told_what_the_record_is_called() {
    let (_, provider) = cancelling().await;
    let written_from = narration_labels(&provider);
    assert!(
        written_from.contains("Trip for Ferri"),
        "everything the writer says about a record it was saying without \
         knowing what the user calls it: {written_from}"
    );
}

/// But only for the records the turn is actually about, as its briefing is.
///
/// Every record the actor may address is in view, and a name the conversation
/// never mentioned is read by the writer as a next step to offer.
#[tokio::test]
async fn a_record_the_turn_is_not_about_is_not_named_to_the_writer() {
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
    let provider = support::narrating().build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip for Ferri", 3, incomplete_case())
        .trip("trip-2", "Trip for Bianchi", 3, incomplete_case())
        .understands(understood)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    let labels = narration_labels(&provider);
    assert!(
        labels.contains("Trip for Ferri"),
        "the record the act reached keeps its name: {labels}"
    );
    assert!(
        !labels.contains("Trip for Bianchi"),
        "and a record nobody in this conversation named is not offered as a \
         word to use: {labels}"
    );
}

/// What the one acknowledgement was written from, as the model read it.
fn narration_labels(provider: &Arc<ScriptedProvider>) -> String {
    provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .next()
        .expect("the writing stage ran")
        .user_text()
}
