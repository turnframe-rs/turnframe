//! A case the directory says is addressable but not silently writable.
//!
//! Some records an actor may legitimately reach are not the one they meant: a
//! document another conversation is building, a record of a workspace they are
//! not in right now. They belong in the candidate list, and an act that lands on
//! one by accident must not write. Reading the message cannot tell the two
//! apart, so the application declares which records are cheap to reach and
//! expensive to get wrong, and the runtime turns a silent write on one of them
//! into a card that names it. A card and not a refusal: a refusal on a record the
//! actor may reach comes back every time they ask for it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::case::CaseKey;
use turnframe_core::ids::TurnId;
use turnframe_core::interaction::InteractionKind;
use turnframe_core::response::{AssistantTurn, ResponseBlock};
use turnframe_runtime::orchestrator::{CaseCandidate, StaticCaseDirectory};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "Set the name to Lisbon";

/// A directory listing the one trip, marked or not.
fn directory(confirm_every_write: bool) -> Arc<StaticCaseDirectory> {
    let candidate = CaseCandidate::new(CaseKey::new("trip", "trip-1"), "Trip 1");
    let candidate = if confirm_every_write {
        candidate.confirming_every_write()
    } else {
        candidate
    };
    Arc::new(StaticCaseDirectory::new().with_case(candidate))
}

/// The turn's reply, and whether it carries a card.
async fn subject_turn(confirm_every_write: bool) -> AssistantTurn {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let understood = UnderstandingBuilder::of(TEXT)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            TEXT,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understood)
        .case_directory(directory(confirm_every_write))
        .without_narration()
        .build()
        .await;
    harness.handle(harness.turn(turn, TEXT)).await.unwrap()
}

fn card_kind(turn: &AssistantTurn) -> Option<InteractionKind> {
    turn.blocks.iter().find_map(|block| match block {
        ResponseBlock::Interaction(card) => Some(card.view.kind),
        _ => None,
    })
}

/// Marked: the write that needed no click gets one, on a card.
#[tokio::test]
async fn a_write_on_a_case_the_directory_protects_asks_first() {
    let turn = subject_turn(true).await;
    assert_eq!(
        card_kind(&turn),
        Some(InteractionKind::ConfirmCommand),
        "a silent write on a case the application protects has to become a question: {:?}",
        turn.blocks
    );
    assert!(
        !turn
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Receipt(_))),
        "and nothing is written before the answer arrives"
    );
}

/// Unmarked: nothing changes, which is what keeps every other turn fluid.
#[tokio::test]
async fn the_same_write_on_an_ordinary_case_still_just_happens() {
    let turn = subject_turn(false).await;
    assert_eq!(
        card_kind(&turn),
        None,
        "an ordinary case is written without a card, exactly as before"
    );
    assert!(
        turn.blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Receipt(_))),
        "and the write leaves its receipt: {:?}",
        turn.blocks
    );
}
