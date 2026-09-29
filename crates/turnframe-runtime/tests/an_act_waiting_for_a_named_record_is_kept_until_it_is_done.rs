//! An act left waiting for a record the user named that did not exist is carried from
//! reply to reply while the conversation moves on, so the record registered later can
//! complete it, and is let go once the act is done, or once a record of that name exists
//! and the turn that brought it left the act undone.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use support::{Harness, token_for};
use turnframe_core::case::CaseKey;
use turnframe_core::ids::ConversationId;
use turnframe_core::ids::TurnId;
use turnframe_core::response::{AssistantTurn, Expectation};
use turnframe_core::turn::ActorContext;
use turnframe_core::understanding::{ActTarget, ArgumentValue, RecordValue};
use turnframe_runtime::orchestrator::{CaseCandidate, CaseDirectory};
use turnframe_store::error::StoreError;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::traveler::active_traveler;
use turnframe_test::workflows::trip::{TripState, operations};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

fn set_traveler(
    turn: TurnId,
    text: &str,
    traveler: RecordValue,
) -> turnframe_core::understanding::Understanding {
    UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_TRAVELER,
            ActTarget::Record {
                token: token_for(turn, "trip", "trip-1"),
            },
            serde_json::json!({}),
            text,
        )
        .with_record("traveler", traveler)
        .build()
        .unwrap()
}

fn still_waiting(answered: &AssistantTurn) -> Vec<String> {
    answered
        .expectations
        .iter()
        .filter_map(|expectation| match expectation {
            Expectation::StillWaiting { act, .. } => match &act.arguments["traveler"].value {
                ArgumentValue::Record(RecordValue::Named { named, .. }) => Some(named.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_act_waiting_for_a_named_record_is_kept_until_it_is_done() {
    let named = RecordValue::Named {
        workflow: "traveler".into(),
        named: "Omar Haddad".to_owned(),
    };
    let meanwhile = "set the name to Lisbon";
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, TripState::default())
        .traveler("trav-1", "Ferri", 1, active_traveler())
        .understands(set_traveler(turn(1), "the trip is for Omar Haddad", named))
        .understands(
            UnderstandingBuilder::of(meanwhile)
                .apply(
                    operations::SET_NAME,
                    token_for(turn(2), "trip", "trip-1"),
                    serde_json::json!({"value": "Lisbon"}),
                    meanwhile,
                )
                .build()
                .unwrap(),
        )
        .understands(set_traveler(
            turn(3),
            "make it Ferri",
            RecordValue::Record {
                token: token_for(turn(3), "traveler", "trav-1"),
            },
        ))
        .without_narration()
        .build()
        .await;

    harness
        .handle(harness.turn(turn(1), "the trip is for Omar Haddad"))
        .await
        .unwrap();
    let moved_on = harness
        .handle(harness.turn(turn(2), meanwhile))
        .await
        .unwrap();
    assert_eq!(
        still_waiting(&moved_on),
        vec!["Omar Haddad".to_owned()],
        "{moved_on:#?}"
    );

    let done = harness
        .handle(harness.turn(turn(3), "make it Ferri"))
        .await
        .unwrap();
    assert!(still_waiting(&done).is_empty(), "{done:#?}");
}

/// Lists the trip, and the traveler named «Omar Haddad» once `registered` is set.
struct Registering {
    registered: AtomicBool,
}

#[async_trait]
impl CaseDirectory for Registering {
    async fn candidates(
        &self,
        _actor: &ActorContext,
        _conversation: &ConversationId,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        let mut listed = vec![CaseCandidate::new(CaseKey::new("trip", "trip-1"), "Trip 1")];
        if self.registered.load(Ordering::SeqCst) {
            listed.push(CaseCandidate::new(
                CaseKey::new("traveler", "trav-2"),
                "Omar Haddad",
            ));
        }
        Ok(listed)
    }
}

#[tokio::test]
async fn an_act_waiting_for_a_named_record_is_let_go_once_that_record_exists() {
    let named = RecordValue::Named {
        workflow: "traveler".into(),
        named: "Omar Haddad".to_owned(),
    };
    let meanwhile = "set the name to Lisbon";
    let directory = Arc::new(Registering {
        registered: AtomicBool::new(false),
    });
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, TripState::default())
        .traveler("trav-2", "Omar Haddad", 1, active_traveler())
        .case_directory(Arc::clone(&directory) as Arc<dyn CaseDirectory>)
        .understands(set_traveler(turn(1), "the trip is for Omar Haddad", named))
        .understands(
            UnderstandingBuilder::of(meanwhile)
                .apply(
                    operations::SET_NAME,
                    token_for(turn(2), "trip", "trip-1"),
                    serde_json::json!({"value": "Lisbon"}),
                    meanwhile,
                )
                .build()
                .unwrap(),
        )
        .without_narration()
        .build()
        .await;

    harness
        .handle(harness.turn(turn(1), "the trip is for Omar Haddad"))
        .await
        .unwrap();

    // Registered elsewhere, by an act that did not complete the waiting one.
    directory.registered.store(true, Ordering::SeqCst);
    let moved_on = harness
        .handle(harness.turn(turn(2), meanwhile))
        .await
        .unwrap();
    assert!(still_waiting(&moved_on).is_empty(), "{moved_on:#?}");
}
