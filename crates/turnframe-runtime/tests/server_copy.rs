//! Supplying the copy the runtime writes itself.
//!
//! Three families of server-authored copy reach a user: what the composer
//! writes, the notices the reducer raises, and the words on a confirmation
//! card. All three ship English defaults that render, and each is reachable
//! from a builder, so a deployment can answer in its users' language.
//!
//! Supplied copy is **additive**: a locale the deployment added is used, and a
//! reader who asked for a locale it did not add still gets the shipped sentence
//! rather than nothing. That is what makes it safe for the library to add a copy
//! field later.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::interaction::InteractionStatus;
use turnframe_core::locale::Locale;
use turnframe_core::understanding::ActTarget;
use turnframe_runtime::policy::ConfirmationCopy;
use turnframe_runtime::reduce::NoticeCopy;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{complete_case, incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn italian() -> Locale {
    Locale::from("it-IT")
}

/// The confirmation copy a deployment answering in Italian would supply:
/// English kept as the default, Italian added beside it.
fn italian_cards() -> ConfirmationCopy {
    let mut copy = ConfirmationCopy::english();
    copy.confirm_title = copy.confirm_title.with(italian(), "Confermi?");
    copy.confirm_label = copy.confirm_label.with(italian(), "Conferma");
    copy.decline_label = copy.decline_label.with(italian(), "Annulla");
    copy
}

/// The same, for the notices the reducer writes.
fn italian_notices() -> NoticeCopy {
    let mut copy = NoticeCopy::english();
    copy.select_target_title = copy.select_target_title.with(italian(), "Quale intendevi?");
    copy.select_target_cancel = copy
        .select_target_cancel
        .with(italian(), "Nessuna di queste");
    copy
}

/// A turn asking to cancel a complete trip, which the conservative policy
/// holds behind a confirmation card.
async fn confirmation_card(copy: Option<ConfirmationCopy>, locale: Locale) -> Vec<String> {
    let turn = turn_one();
    let text = "Cancel that trip";
    let understood = UnderstandingBuilder::of(text)
        .apply(
            operations::WITHDRAW,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!(null),
            text,
        )
        .build()
        .unwrap();
    let mut builder = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(understood)
        .without_narration();
    if let Some(copy) = copy {
        builder = builder.confirmation_copy(copy);
    }
    let harness = builder.build().await;

    let mut input = harness.turn(turn, text);
    input.locale = locale.clone();
    let answered = harness.handle(input).await.unwrap();

    let card = answered
        .blocks
        .iter()
        .find_map(|block| match block {
            turnframe_core::response::ResponseBlock::Interaction(block) => Some(&block.view),
            _ => None,
        })
        .expect("a confirmation card is on screen");
    assert_eq!(card.status, InteractionStatus::Active);
    let mut labels = vec![card.title.resolve(&locale).to_owned()];
    labels.extend(
        card.options
            .iter()
            .map(|option| option.label.resolve(&locale).to_owned()),
    );
    labels
}

#[tokio::test]
async fn confirmation_card_copy_can_be_supplied() {
    let labels = confirmation_card(Some(italian_cards()), italian()).await;
    assert!(
        labels.iter().any(|label| label == "Conferma"),
        "the confirming button carries the deployment's word: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Annulla"),
        "and so does the declining one: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|label| label == "Confirm" || label == "Cancel"),
        "no English survives on the buttons of an Italian turn: {labels:?}"
    );
}

#[tokio::test]
async fn a_deployment_that_supplies_nothing_still_gets_a_card() {
    // The default is real copy in the languages it ships, English and Italian.
    let labels = confirmation_card(None, italian()).await;
    assert!(
        labels.iter().any(|label| label == "Conferma"),
        "the shipped default speaks the turn's language: {labels:?}"
    );
}

#[tokio::test]
async fn supplied_copy_is_additive_rather_than_a_replacement() {
    // The second half of additive, and the half that makes it safe for this
    // library to add a copy field later: a reader asking for a locale the
    // deployment did not translate gets the shipped sentence, not an empty
    // string.
    let labels = confirmation_card(Some(italian_cards()), Locale::from("de-DE")).await;
    assert!(
        labels.iter().any(|label| label == "Confirm"),
        "a locale nobody translated falls back to what was shipped: {labels:?}"
    );
    assert!(
        labels.iter().all(|label| !label.is_empty()),
        "and never to nothing: {labels:?}"
    );
}

/// A selection card, whose title and cancel option come from the reducer's own
/// copy rather than from a workflow.
///
/// Two trips share a name and the turn names it, so both fit and the reducer
/// asks which. The words it asks with are `NoticeCopy`, which is why
/// this is the end-to-end half: a supplied value that never reaches the wiring
/// would still satisfy a test that only read the value back.
async fn selection_card(copy: Option<NoticeCopy>, locale: Locale) -> Vec<String> {
    let turn = turn_one();
    let text = "Set the name on the Ferri trip to Lisbon";
    let either = ActTarget::Ambiguous {
        candidates: vec![
            token_for(turn, "trip", "trip-1"),
            token_for(turn, "trip", "trip-2"),
        ],
    };
    let understood = UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_NAME,
            either,
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let mut builder = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .understands(understood)
        .without_narration();
    if let Some(copy) = copy {
        builder = builder.notice_copy(copy);
    }
    let harness = builder.build().await;

    let mut input = harness.turn(turn, text);
    input.locale = locale.clone();
    let answered = harness.handle(input).await.unwrap();

    let card = answered
        .blocks
        .iter()
        .find_map(|block| match block {
            turnframe_core::response::ResponseBlock::Interaction(block) => Some(&block.view),
            _ => None,
        })
        .expect("an ambiguous target asks rather than guessing");
    let mut labels = vec![card.title.resolve(&locale).to_owned()];
    labels.extend(
        card.options
            .iter()
            .map(|option| option.label.resolve(&locale).to_owned()),
    );
    labels
}

#[tokio::test]
async fn reducer_notice_copy_can_be_supplied() {
    let labels = Box::pin(selection_card(Some(italian_notices()), italian())).await;
    assert!(
        labels.iter().any(|label| label == "Quale intendevi?"),
        "the reducer asks in the deployment's language: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Nessuna di queste"),
        "including the way out of the card: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|label| label == "Which one did you mean?"),
        "no English survives on an Italian turn: {labels:?}"
    );
}

#[tokio::test]
async fn a_deployment_that_supplies_no_notice_copy_still_gets_asked() {
    let labels = Box::pin(selection_card(None, italian())).await;
    assert!(
        labels.iter().any(|label| label == "Quale intendevi?"),
        "the shipped default speaks the turn's language: {labels:?}"
    );
}

#[test]
fn notice_copy_is_additive_in_both_directions() {
    // The reducer's notices reach a user through the same `LocalizedText`, so
    // the property is the same one; asserting it on the value keeps the test
    // honest about what it is checking rather than routing a whole turn to
    // observe one string.
    let copy = italian_notices();
    assert_eq!(
        copy.select_target_title.resolve(&italian()),
        "Quale intendevi?"
    );
    assert_eq!(
        copy.select_target_title.resolve(&Locale::from("en-GB")),
        "Which one did you mean?",
        "the shipped sentence is still there for a reader who asked for it"
    );
    // A field the deployment did not translate keeps its shipped text in every
    // locale, which is what lets this library add one without breaking anybody.
    assert_eq!(
        copy.draft_only.resolve(&italian()),
        NoticeCopy::english().draft_only.resolve(&italian()),
        "an untranslated field is the shipped sentence, not an empty one"
    );
}

#[test]
fn every_copy_family_is_reachable_from_a_builder() {
    // A compile-time check: the failure this guards against is an absent
    // setter, not a wrong string.
    let _ = turnframe_runtime::orchestrator::Orchestrator::builder()
        .notice_copy(NoticeCopy::english())
        .confirmation_copy(ConfirmationCopy::english());
    // The composer's own copy was reachable all along, since an application
    // builds the composer itself and hands it over.
    let _ = turnframe_runtime::compose::Composer::with_copy;
}
