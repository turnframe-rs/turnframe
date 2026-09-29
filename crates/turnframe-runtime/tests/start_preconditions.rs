//! Whether a workflow may start can be a fact about another case.
//!
//! A projector is pure and cannot read another case, so a workflow declares what
//! it needs to start and the runtime, which has the other cases in hand, decides
//! whether it is there. Understanding is told before it proposes; a start
//! proposed anyway is refused carrying the workflow's own reason, and the writer
//! hears that reason only on a turn that asked for the workflow.
//!
//! The sample's rule is artificial and its shape is not: a traveler may only be
//! started while a trip is still being filled in.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, token_for};
use turnframe_core::ids::{OperationKey, TurnId, WorkflowKey};
use turnframe_core::understanding::Understanding;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::traveler::operations as traveler_ops;
use turnframe_test::workflows::trip::{
    TripState, awaiting_rebooking_confirmation, incomplete_case, operations,
};
use turnframe_understand::{UnderstandingInput, WorkflowBrief};

const SET_NAME: &str = "Set the name to Lisbon";
const ADD_TRAVELER: &str = "Add a new traveler";
const REASON: &str = "only be added while a trip is open";

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn set_subject() -> Understanding {
    UnderstandingBuilder::of(SET_NAME)
        .apply(
            operations::SET_NAME,
            token_for(turn_one(), "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            SET_NAME,
        )
        .build()
        .unwrap()
}

fn add_traveler() -> Understanding {
    UnderstandingBuilder::of(ADD_TRAVELER)
        .start("traveler", ADD_TRAVELER)
        .build()
        .unwrap()
}

/// What understanding was shown for a turn against a trip in `state`.
async fn offered(state: TripState) -> UnderstandingInput {
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, state)
        .understands(set_subject())
        .without_narration()
        .build()
        .await;
    let _ = harness.handle(harness.turn(turn_one(), SET_NAME)).await;
    harness
        .understander
        .seen()
        .into_iter()
        .next()
        .expect("the turn was understood")
}

fn traveler(input: &UnderstandingInput) -> &WorkflowBrief {
    input
        .workflow(&WorkflowKey::from("traveler"))
        .expect("the traveler workflow is registered")
}

/// The prerequisite is in place, so a start is on offer as it always was.
#[tokio::test]
async fn a_workflow_whose_prerequisite_holds_may_still_be_started() {
    let input = offered(incomplete_case()).await;
    let traveler = traveler(&input);
    assert!(
        traveler.startable,
        "a trip is open, which is what the traveler workflow asked for"
    );
    assert!(
        traveler
            .spec(&OperationKey::from(traveler_ops::CREATE_DRAFT))
            .is_some(),
        "and the operation that opens one is offered"
    );
}

/// It is not, so understanding is not offered a way to start.
#[tokio::test]
async fn a_workflow_whose_prerequisite_is_missing_is_not_offered_a_start() {
    let input = offered(awaiting_rebooking_confirmation()).await;
    let traveler = traveler(&input);
    assert!(
        !traveler.startable,
        "the start cannot be proposed, so it cannot be refused later somewhere \
         with a database"
    );
    assert!(
        traveler
            .spec(&OperationKey::from(traveler_ops::CREATE_DRAFT))
            .is_none(),
        "nor can the operation that opens one on a new record: {:?}",
        traveler
            .operations
            .iter()
            .map(|spec| spec.key.to_string())
            .collect::<Vec<_>>()
    );
}

/// A start proposed anyway is refused, carrying the workflow's own reason.
#[tokio::test]
async fn starting_a_workflow_whose_prerequisite_is_missing_is_refused_with_its_reason() {
    let harness = Harness::builder()
        // Not `Collecting`, so the traveler workflow's prerequisite is unmet.
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(add_traveler())
        .without_narration()
        .build()
        .await;

    let answered = harness
        .handle(harness.turn(turn_one(), ADD_TRAVELER))
        .await
        .unwrap();
    let notices = notice_texts(&answered);
    assert!(
        notices.iter().any(|(_, text)| text.contains(REASON)),
        "the workflow's own reason reaches the user, rather than depending on \
         the sentence a writing stage happens to produce: {notices:?}"
    );
}

/// Opening a record is starting its workflow, whichever act does it: an operation
/// on a new record is refused like a start.
#[tokio::test]
async fn an_operation_opening_a_record_of_a_workflow_that_cannot_start_is_refused() {
    let opening = UnderstandingBuilder::of(ADD_TRAVELER)
        .open(
            traveler_ops::CREATE_DRAFT,
            "traveler",
            serde_json::json!(null),
            ADD_TRAVELER,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(opening)
        .without_narration()
        .build()
        .await;

    let answered = harness
        .handle(harness.turn(turn_one(), ADD_TRAVELER))
        .await
        .unwrap();
    assert!(
        answered.receipts().next().is_none(),
        "no traveler was created"
    );
    let notices = notice_texts(&answered);
    assert!(
        notices.iter().any(|(_, text)| text.contains(REASON)),
        "{notices:?}"
    );
    assert_eq!(harness.replay(turn_one()).await.act_outcomes, ["rejected"]);
}

/// And when the prerequisite holds, that door opens as it always did.
#[tokio::test]
async fn starting_a_workflow_whose_prerequisite_holds_is_not_refused() {
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(add_traveler())
        .without_narration()
        .build()
        .await;

    let answered = harness
        .handle(harness.turn(turn_one(), ADD_TRAVELER))
        .await
        .unwrap();
    let notices = notice_texts(&answered);
    assert!(
        !notices.iter().any(|(code, text)| {
            code == turnframe_runtime::reduce::rejection::PRECONDITION_UNMET
                || text.contains(REASON)
        }),
        "{notices:?}"
    );
}

/// What the case in view already offers is untouched: the rule is about
/// starting, not about continuing.
#[tokio::test]
async fn a_case_already_in_view_keeps_its_own_operations() {
    let input = offered(awaiting_rebooking_confirmation()).await;
    let (_, record) = input
        .record(&token_for(turn_one(), "trip", "trip-1"))
        .expect("the trip is in view");
    assert!(
        record.offers(&OperationKey::from(operations::SET_NAME)),
        "{:?}",
        record.operations
    );
}

fn notice_texts(turn: &turnframe_core::response::AssistantTurn) -> Vec<(String, String)> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            turnframe_core::response::ResponseBlock::Notice(notice) => Some((
                notice.code.clone(),
                notice
                    .text
                    .resolve(&turnframe_core::locale::Locale::from("en-GB"))
                    .to_owned(),
            )),
            _ => None,
        })
        .collect()
}

/// The brief the writing stage ran under, for a turn understood as `understanding`.
async fn narration_brief(text: &str, understanding: Understanding) -> String {
    let provider = support::narrating()
        .answering("Here you go.")
        .build_shared();
    let harness = Harness::builder()
        // Not `Collecting`, so the traveler workflow's prerequisite is unmet.
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness
        .handle(harness.turn(turn_one(), text))
        .await
        .unwrap();

    provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .next()
        .expect("the writing stage ran")
        .user_text()
}

/// A turn that asks for a blocked workflow tells the writer which, and why: the
/// refusal is what says what the turn was about.
#[tokio::test]
async fn a_turn_that_asks_for_a_blocked_workflow_tells_the_writer_why() {
    let brief = narration_brief(ADD_TRAVELER, add_traveler()).await;
    assert!(
        brief.contains(REASON),
        "the writer is told why the workflow could not be started, in its own \
         words, which are the answer to what was asked: {brief}"
    );
}

/// And a turn that never mentions it does not hear about it: a workflow blocked
/// for good would otherwise be a fact in every turn, and the writer says it.
#[tokio::test]
async fn a_turn_about_something_else_is_not_told_what_is_blocked() {
    let brief = narration_brief(SET_NAME, set_subject()).await;
    assert!(
        !brief.contains(REASON),
        "a workflow the turn never named is not this turn's business: {brief}"
    );
}
