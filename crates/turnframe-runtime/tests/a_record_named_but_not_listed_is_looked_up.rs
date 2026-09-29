//! A record the user names that is not in view is looked up in the case directory:
//! one found is the act's target, several are offered on a selection card.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use support::Harness;
use turnframe_core::case::CaseKey;
use turnframe_core::error::StoreError;
use turnframe_core::ids::{ConversationId, TurnId, WorkflowKey};
use turnframe_core::interaction::InteractionKind;
use turnframe_core::turn::ActorContext;
use turnframe_core::understanding::Understanding;
use turnframe_runtime::orchestrator::{CaseCandidate, CaseDirectory};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "set the name of the Ferri trip to Rent";

/// Lists nothing, and finds the trips it was given for any words.
struct Finding(Vec<&'static str>);

#[async_trait]
impl CaseDirectory for Finding {
    async fn candidates(
        &self,
        _actor: &ActorContext,
        _conversation: &ConversationId,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        Ok(Vec::new())
    }

    async fn find(
        &self,
        _actor: &ActorContext,
        _conversation: &ConversationId,
        workflow: &WorkflowKey,
        named: Option<&str>,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        assert_eq!(
            named,
            Some("the Ferri trip"),
            "the directory gets the words"
        );
        Ok(self
            .0
            .iter()
            .map(|id| {
                CaseCandidate::new(CaseKey::new(workflow.clone(), *id), format!("Ferri {id}"))
            })
            .collect())
    }
}

fn turn() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn named_not_listed() -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .apply_to_unlisted(
            operations::SET_NAME,
            "trip",
            "the Ferri trip",
            serde_json::json!({"value": "Rent"}),
            TEXT,
        )
        .build()
        .unwrap()
}

async fn harness(found: Vec<&'static str>) -> Harness {
    Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip("trip-2", "Trip 2", 3, incomplete_case())
        .case_directory(Arc::new(Finding(found)))
        .understands(named_not_listed())
        .without_narration()
        .build()
        .await
}

#[tokio::test]
async fn the_one_record_found_is_written() {
    let harness = harness(vec!["trip-2"]).await;
    harness.handle(harness.turn(turn(), TEXT)).await.unwrap();
    assert_eq!(harness.trip_name("trip-2").as_deref(), Some("Rent"));
    assert_eq!(harness.trip_name("trip-1"), None);
}

#[tokio::test]
async fn several_records_found_are_offered_as_a_choice() {
    let harness = harness(vec!["trip-1", "trip-2"]).await;
    let answered = harness.handle(harness.turn(turn(), TEXT)).await.unwrap();
    let card = answered.interactions().next().expect("a selection card");
    assert_eq!(card.kind, InteractionKind::SelectTarget);
    assert!(harness.events("trip", "trip-1").await.is_empty());
    assert!(harness.events("trip", "trip-2").await.is_empty());
}

#[tokio::test]
async fn planning_finds_it_too() {
    let harness = harness(vec!["trip-2"]).await;
    let planned = harness
        .orchestrator
        .plan_turn(harness.turn(turn(), TEXT))
        .await
        .unwrap();
    assert_eq!(planned.would_execute.len(), 1);
    assert!(
        harness.events("trip", "trip-2").await.is_empty(),
        "planning writes nothing"
    );
}
