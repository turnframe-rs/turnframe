//! A command refused at execution, and whether the writer is told.
//!
//! Some refusals can only be decided at execution: resolving a name against a
//! registry is a database read, and `compile_act` and `validate_command` are
//! pure. Such a refusal reaches the writer as a fact carrying the domain's own
//! explanation, exactly as a refusal in the reducer does, so the prose cannot
//! claim the write; and the reader gets that explanation rather than the
//! generic notice.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::error::{DomainRejection, ExecutionError};
use turnframe_core::ids::TurnId;
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::response::{NarratableFact, ResponseBlock};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// A turn whose one act is refused by the executor, with the domain's own
/// sentence on the rejection.
async fn refused_at_execution() -> (
    turnframe_core::response::AssistantTurn,
    Arc<ScriptedProvider>,
) {
    let turn = turn_one();
    let text = "a mario ferri";
    let provider = noting();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(naming_a_traveler(turn, text))
        .trip_fails(ExecutionError::Rejected(
            DomainRejection::new("trip.recipient_unresolved", "recipient_unresolved")
                .with_explanation(LocalizedText::new(
                    "No traveler of that name is in the registry.",
                )),
        ))
        .provider(Arc::clone(&provider))
        .build()
        .await;
    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();
    (answered, provider)
}

/// The one act of the turn: a name the executor will look up and not find.
fn naming_a_traveler(turn: TurnId, text: &str) -> turnframe_core::understanding::Understanding {
    UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "mario ferri"}),
            text,
        )
        .build()
        .unwrap()
}

fn noting() -> Arc<ScriptedProvider> {
    ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Noted.")
        .build_shared()
}

/// The writer is told the act was refused, and why.
#[tokio::test]
async fn a_refusal_at_execution_reaches_the_writer() {
    let (_, provider) = refused_at_execution().await;
    let brief = provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .next()
        .expect("the writing stage ran")
        .request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|part| match part {
            turnframe_provider::request::ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        brief.contains("\"not_done\": [\n    \""),
        "the fact that stops the prose claiming the write: {brief}"
    );
    assert!(
        brief.contains("No traveler of that name is in the registry."),
        "and the domain's own sentence, which used to be dropped: {brief}"
    );
}

/// And the reader gets that sentence rather than the generic one.
#[tokio::test]
async fn the_reader_is_given_the_domain_s_own_reason() {
    let (answered, _) = refused_at_execution().await;
    let notices: Vec<String> = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Notice(notice) => {
                Some(notice.text.resolve(&Locale::from("en-GB")).to_owned())
            }
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|text| text.contains("No traveler of that name")),
        "{notices:?}"
    );
}

/// A rejection with nothing written on it still gets the generic notice, which
/// is what that copy was written for.
#[tokio::test]
async fn a_rejection_with_no_explanation_keeps_the_generic_notice() {
    let turn = turn_one();
    let text = "a mario ferri";
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(naming_a_traveler(turn, text))
        .trip_fails(ExecutionError::Rejected(DomainRejection::new(
            "trip.recipient_unresolved",
            "recipient_unresolved",
        )))
        .provider(noting())
        .build()
        .await;
    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();
    let codes: Vec<String> = answered
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Notice(notice) => Some(notice.code.clone()),
            _ => None,
        })
        .collect();
    assert!(
        codes
            .iter()
            .any(|code| code == "turnframe.notice.command_failed"),
        "{codes:?}"
    );
    // And the fact still travels, so the prose cannot claim the write.
    let refused = answered.blocks.iter().any(|block| match block {
        ResponseBlock::Transition(block) => block
            .facts_used
            .iter()
            .any(|fact| matches!(fact, NarratableFact::ActRefused { .. })),
        _ => false,
    });
    assert!(refused, "a refusal with no sentence is still a refusal");
}
