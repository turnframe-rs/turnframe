//! A fact that becomes true of one case and settles a question standing on
//! another.
//!
//! Everything that can add a command to a turn sees one case: `compile_act` is
//! handed its own state, an executor's preparers one batch. A write that implies
//! another on a different case — a traveler created, and the trip waiting
//! for that traveler — is knowable only to the application, which declares it
//! as `TurnConsequences`; the runtime executes what it returns in the same turn.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use support::{Harness, narrating, token_for};
use turnframe_core::command::IdempotencyKey;
use turnframe_core::command::{AtomicityScope, CommandBatch, CommandEnvelope};
use turnframe_core::error::StoreError;
use turnframe_core::ids::{BatchId, CommandId, TurnId};
use turnframe_core::turn::ActorContext;
use turnframe_core::understanding::Understanding;
use turnframe_runtime::orchestrator::TurnConsequences;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

const TEXT: &str = "call the name Lisbon";

fn set_name() -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .apply(
            operations::SET_NAME,
            token_for(turn_one(), "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            TEXT,
        )
        .build()
        .unwrap()
}

/// «A name written on one trip is written on the other», which is not a
/// rule any real domain has — but it is one only an application could state,
/// which is the property under test.
struct NameTravels;

#[async_trait]
impl TurnConsequences for NameTravels {
    async fn following(
        &self,
        actor: &ActorContext,
        batches: &[CommandBatch<serde_json::Value>],
    ) -> Result<Vec<CommandBatch<serde_json::Value>>, StoreError> {
        let Some(source) = batches
            .iter()
            .flat_map(|batch| batch.envelopes.iter())
            .find(|envelope| envelope.command.get("set_name").is_some())
        else {
            return Ok(Vec::new());
        };
        let case_ref = turnframe_core::case::CaseRef::new(
            source.case_ref.workflow.clone(),
            turnframe_core::ids::CaseId::from("trip-2"),
            source.case_ref.expected_revision,
        );
        Ok(vec![CommandBatch {
            batch_id: BatchId::new(),
            scope: AtomicityScope::PerCase,
            envelopes: vec![CommandEnvelope {
                command_id: CommandId::new(),
                turn_id: source.turn_id,
                actor: actor.clone(),
                case_ref,
                idempotency_key: IdempotencyKey::new("consequence:name"),
                origin: source.origin.clone(),
                command: source.command.clone(),
            }],
        }])
    }
}

/// The consequence executes in the same turn, on a case the turn never named.
#[tokio::test]
async fn a_turn_writes_what_its_writes_imply_elsewhere() {
    let turn = turn_one();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip("trip-2", "Trip 2", 3, incomplete_case())
        .understands(set_name())
        .provider(narrating().build_shared())
        .consequences(Arc::new(NameTravels))
        .build()
        .await;

    harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    assert_eq!(
        harness.trip_name("trip-1").as_deref(),
        Some("Lisbon"),
        "the case the turn named is written, as always"
    );
    assert_eq!(
        harness.trip_name("trip-2").as_deref(),
        Some("Lisbon"),
        "and so is the one only the application knew about"
    );
}

/// A deployment that declares none behaves exactly as before.
#[tokio::test]
async fn a_deployment_that_declares_none_writes_only_what_it_planned() {
    let turn = turn_one();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip("trip-2", "Trip 2", 3, incomplete_case())
        .understands(set_name())
        .provider(narrating().build_shared())
        .build()
        .await;

    harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    assert_eq!(harness.trip_name("trip-1").as_deref(), Some("Lisbon"));
    assert_eq!(
        harness.trip_name("trip-2"),
        None,
        "nothing reaches a case the turn did not name"
    );
}
