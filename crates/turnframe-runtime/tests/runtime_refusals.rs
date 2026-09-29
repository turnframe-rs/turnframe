//! What a user reads when the runtime itself refuses an act.
//!
//! A domain rejection can carry the workflow's own words; a refusal the
//! *runtime* decided has only the runtime's. Each such refusal has copy of its
//! own, so an adopter translates a sentence per situation, and raises a notice
//! under its **own** code, so a turn that failed for two reasons reaches the
//! user as two sentences: notices are deduplicated by code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::locale::Locale;
use turnframe_core::response::{AssistantTurn, ResponseBlock};
use turnframe_core::understanding::{ActTarget, Understanding};
use turnframe_runtime::reduce::{NoticeCopy, rejection};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn english() -> Locale {
    Locale::from("en-GB")
}

const CANCEL_THAT: &str = "cancel that";

/// A cancellation aimed at a record it would create. Cancelling takes only a
/// record in view, so the runtime refuses the target shape.
fn cancelling_a_new_record() -> Understanding {
    UnderstandingBuilder::of(CANCEL_THAT)
        .open(
            operations::WITHDRAW,
            "trip",
            serde_json::json!(null),
            CANCEL_THAT,
        )
        .build()
        .unwrap()
}

/// Every notice the turn raised, as `(code, text)` in the turn's locale.
fn notices(turn: &AssistantTurn, locale: &Locale) -> Vec<(String, String)> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Notice(notice) => {
                Some((notice.code.clone(), notice.text.resolve(locale).to_owned()))
            }
            _ => None,
        })
        .collect()
}

/// The notices of a turn aiming a cancellation at a record that does not exist.
async fn aiming_a_cancellation_at_a_record_that_does_not_exist(
    copy: Option<NoticeCopy>,
    locale: Locale,
) -> Vec<(String, String)> {
    let turn = turn_one();
    let mut builder = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .understands(cancelling_a_new_record())
        .without_narration();
    if let Some(copy) = copy {
        builder = builder.notice_copy(copy);
    }
    let harness = builder.build().await;

    let mut input = harness.turn(turn, CANCEL_THAT);
    input.locale = locale.clone();
    let answered = harness.handle(input).await.unwrap();
    notices(&answered, &locale)
}

/// The refusal reaches the user under its own name, saying its own thing.
#[tokio::test]
async fn a_runtime_refusal_says_which_refusal_it_is() {
    let raised = Box::pin(aiming_a_cancellation_at_a_record_that_does_not_exist(
        None,
        english(),
    ))
    .await;
    let (_, text) = raised
        .iter()
        .find(|(code, _)| code == rejection::TARGET_POLICY_MISMATCH)
        .expect("the refusal is reported under its own code");
    assert!(
        text.contains("which record"),
        "and in words about this refusal rather than about refusals in general: {text}"
    );
    assert!(
        !raised
            .iter()
            .any(|(_, text)| text.contains("could not be done")),
        "the catch-all is not what the user reads: {raised:?}"
    );
}

/// The half the copy fields exist for: an adopter can translate them.
#[tokio::test]
async fn a_runtime_refusal_can_be_translated() {
    let mut copy = NoticeCopy::english();
    copy.target_policy_mismatch = copy.target_policy_mismatch.with(
        Locale::from("it-IT"),
        "Non ho capito su quale pratica dovevo scrivere.",
    );
    let raised = Box::pin(aiming_a_cancellation_at_a_record_that_does_not_exist(
        Some(copy),
        Locale::from("it-IT"),
    ))
    .await;
    assert!(
        raised
            .iter()
            .any(|(_, text)| text == "Non ho capito su quale pratica dovevo scrivere."),
        "{raised:?}"
    );
}

/// A locale the deployment did not translate still reads a sentence, and it is
/// still the specific one.
#[tokio::test]
async fn an_untranslated_locale_keeps_the_shipped_sentence() {
    let raised = Box::pin(aiming_a_cancellation_at_a_record_that_does_not_exist(
        None,
        Locale::from("it-IT"),
    ))
    .await;
    assert!(
        raised
            .iter()
            .any(|(code, text)| code == rejection::TARGET_POLICY_MISMATCH && !text.is_empty()),
        "{raised:?}"
    );
}

/// Two acts, two different reasons, two sentences: one shared notice code would
/// deduplicate the second refusal away.
#[tokio::test]
async fn two_different_refusals_reach_the_user_as_two_sentences() {
    let turn = turn_one();
    let text = "Set the name on the Bianchi trip to Lisbon, and yes to the question";
    let understanding = UnderstandingBuilder::of(text)
        // No trip in view is the Bianchi one, so nothing resolves.
        .apply_to(
            operations::SET_NAME,
            ActTarget::NotListed {
                workflow: "trip".into(),
                words: None,
            },
            serde_json::json!({"value": "Lisbon"}),
            "Set the name on the Bianchi trip to Lisbon",
        )
        // And this one aims a cancellation at a record it would create, so it
        // fails for an unrelated reason.
        .open(
            operations::WITHDRAW,
            "trip",
            serde_json::json!(null),
            "yes to the question",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();
    let raised = notices(&answered, &english());
    let codes: Vec<&str> = raised.iter().map(|(code, _)| code.as_str()).collect();
    assert!(codes.contains(&rejection::TARGET_MISSING), "{codes:?}");
    assert!(
        codes.contains(&rejection::TARGET_POLICY_MISMATCH),
        "{codes:?}"
    );
    let texts: std::collections::BTreeSet<&str> =
        raised.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(
        texts.len(),
        raised.len(),
        "two failures, two different sentences: {raised:?}"
    );
}

/// A refusal the *domain* decided keeps the notice code
/// `turnframe.notice.act_refused`; the runtime's own codes are a family beside it.
#[tokio::test]
async fn a_domain_rejection_keeps_its_own_notice_code() {
    let turn = turn_one();
    let text = "Set the name to nothing at all";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": ""}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();
    let raised = notices(&answered, &english());
    assert!(
        raised
            .iter()
            .any(|(code, _)| code == "turnframe.notice.act_refused"),
        "{raised:?}"
    );
    assert!(
        raised
            .iter()
            .all(|(code, _)| !code.starts_with("turnframe.target.")),
        "a domain refusal is not filed as one of the runtime's own: {raised:?}"
    );
}
