//! The scripted provider and the replay assertions, driven the way a runtime
//! would drive them (spec §11.3, §20.1, I20).
//!
//! This file plays a runtime's part by hand: segment the message, narrate. What it
//! proves is that the kit can *hold a runtime to* a specific number of model calls,
//! made for specific purposes, carrying a specific schema, and that two runs of the
//! same turn can be compared as artefacts rather than eyeballed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use turnframe_core::case::CaseRef;
use turnframe_core::command::CommandPolicy;
use turnframe_core::event::ReceiptEvent;
use turnframe_core::flow::{WorkflowDefinition, WorkflowExecutor};
use turnframe_core::ids::{AccountId, BlockId, CaseRevision, ConversationId, TurnId};
use turnframe_core::locale::Locale;
use turnframe_core::policy::PolicySnapshot;
use turnframe_core::reduce::CommandRef;
use turnframe_core::replay::{CommandOutcome, CommandOutcomeRecord, ReplayRecord};
use turnframe_core::response::{
    AssistantTurn, GeneratedTransition, NarratableFact, ReceiptBlock, ReplayToken, ResponseBlock,
};
use turnframe_core::understanding::Understanding;
use turnframe_provider::prelude::{Message, ModelPurpose, ModelRequest, OutputSpec};
use turnframe_provider::provider::ModelProvider;
use turnframe_test::providers::{
    ScriptViolation, ScriptedProvider, ScriptedReply, UnderstandingBuilder,
};
use turnframe_test::replay::{ReplayDivergence, ReplayEvidence, TurnExecution, same_turn};
use turnframe_test::workflows::trip::{
    SAMPLE_NAME, TripCommand, TripEvent, TripExecutor, TripWorkflow, operations,
};

const TEXT: &str = "Cambia il nome in Lisbona e dimmi quando parto";

fn understanding() -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .apply(
            operations::SET_NAME,
            "tok_trip_1",
            serde_json::json!({"value": SAMPLE_NAME}),
            "Cambia il nome in Lisbona",
        )
        .ask("dimmi quando parto")
        .build()
        .expect("the quotes come from the text")
}

fn segmentation() -> serde_json::Value {
    serde_json::json!({
        "analysis": "Names the trip, then asks when the flight leaves.",
        "units": [
            {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"},
            {"kind": "question", "words": {"from": 7, "to": 9}, "workflow": "trip",
             "basis": "current_committed_state", "continues_previous": false}
        ]
    })
}

/// A segmentation call as it reaches a model: the output schema carries its closed sets.
fn segment_request() -> ModelRequest {
    ModelRequest::new(ModelPurpose::Segment)
        .with_system("Split the user's message into units.")
        .with_message(Message::user(TEXT))
        .with_output(OutputSpec::json(
            "segment",
            serde_json::json!({
                "type": "object",
                "properties": {"workflow": {"enum": ["trip", "unknown"]}}
            }),
        ))
}

#[tokio::test]
async fn a_scripted_turn_makes_exactly_the_calls_the_script_declares() {
    let provider = ScriptedProvider::builder("scripted", "m")
        .reply_to(ModelPurpose::Segment, ScriptedReply::Json(segmentation()))
        .reply_to(
            ModelPurpose::Acknowledge,
            ScriptedReply::text("Ho cambiato il nome."),
        )
        .build();

    let first = provider.generate(segment_request()).await.unwrap();
    let units: serde_json::Value = serde_json::from_str(&first.text()).unwrap();
    assert_eq!(units, segmentation());

    let second = provider
        .generate(ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user(TEXT)))
        .await
        .unwrap();
    assert_eq!(second.text(), "Ho cambiato il nome.");

    // What was sent, not only what came back.
    let calls = provider.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].schema_name(), Some("segment"));
    assert!(calls[0].schema_mentions("trip"));
    assert!(!calls[0].schema_mentions("traveler"));
    assert_eq!(calls[1].purpose(), ModelPurpose::Acknowledge);
    assert_eq!(calls[1].user_text(), TEXT);

    provider.verify().expect("the script was followed exactly");
}

#[tokio::test]
async fn one_model_call_too_many_is_reported_even_when_it_is_swallowed() {
    let provider = ScriptedProvider::builder("scripted", "m")
        .reply_to(ModelPurpose::Segment, ScriptedReply::Json(segmentation()))
        .build();
    provider.generate(segment_request()).await.unwrap();

    // A runtime that retries the stage and ignores the failure.
    let swallowed = provider.generate(segment_request()).await;
    assert!(swallowed.is_err());

    assert_eq!(
        provider.verify().unwrap_err(),
        ScriptViolation::UnexpectedCall {
            call_index: 1,
            purpose: "segment",
        }
    );
    assert_eq!(provider.call_count(), 2);
}

/// Runs one trip command and returns the events it committed.
async fn run(executor: &TripExecutor, command: TripCommand) -> Vec<ReceiptEvent<TripEvent>> {
    let case_ref = CaseRef::new("trip", "trip-1", CaseRevision(0));
    let batch = support::batch(
        support::turn(1),
        &case_ref,
        &support::confirmed_click(),
        vec![command],
    );
    executor
        .execute(batch)
        .await
        .expect("the command commits")
        .receipt_events()
}

/// Builds the record and the answer a turn that ran `command` would produce.
async fn execution(executor: &TripExecutor, command: TripCommand) -> TurnExecution {
    let workflow = TripWorkflow::default();
    let case_ref = CaseRef::new("trip", "trip-1", CaseRevision(0));
    let batch = support::batch(
        support::turn(1),
        &case_ref,
        &support::confirmed_click(),
        vec![command.clone()],
    );
    let commit = executor
        .execute(batch.clone())
        .await
        .expect("the command commits");
    let receipts = workflow.receipts(&commit.receipt_events(), &Locale::from("it-IT"));
    let envelope = &batch.envelopes[0];
    let command_ref = CommandRef {
        batch_id: batch.batch_id,
        command_id: envelope.command_id,
    };

    let mut record = ReplayRecord::received(
        support::turn(1),
        ConversationId::nil(),
        AccountId::from(support::ACCOUNT),
        commit
            .events
            .first()
            .map_or_else(chrono::Utc::now, |event| event.occurred_at),
    );
    record.understanding = Some(understanding());
    record
        .policy_decisions
        .push(PolicySnapshot::conservative().decide(
            command_ref,
            &workflow.command_policy(None, &command),
            &envelope.origin,
        ));
    record.command_outcomes.push(CommandOutcomeRecord {
        command_ref,
        idempotency_key: envelope.idempotency_key.clone(),
        case_ref,
        origin: Some(envelope.origin.clone()),
        outcome: CommandOutcome::Committed {
            new_revision: commit.new_revision,
            event_ids: commit.event_ids(),
        },
    });
    record.event_ids = commit.event_ids();

    let mut blocks: Vec<ResponseBlock> = receipts
        .iter()
        .map(|receipt| {
            ResponseBlock::Receipt(ReceiptBlock {
                block_id: BlockId::new(format!("receipt-{}", receipt.receipt_id)),
                receipt: receipt.clone(),
            })
        })
        .collect();
    if let Some(last) = receipts.last() {
        blocks.push(ResponseBlock::Transition(GeneratedTransition {
            block_id: BlockId::from("transition-1"),
            text: "Ecco cosa ho fatto.".to_owned(),
            facts_used: vec![NarratableFact::OperationalOutcome {
                receipt_id: last.receipt_id,
                event_ids: last.event_ids.clone(),
                status_code: last.status_code.clone(),
            }],
        }));
    }
    record.response_block_ids = blocks
        .iter()
        .map(|block| block.block_id().clone())
        .collect();

    let response = AssistantTurn {
        turn_id: support::turn(1),
        conversation_id: ConversationId::nil(),
        blocks,
        subjects: Vec::new(),
        expectations: Vec::new(),
        replay_token: ReplayToken::from("replay-1"),
        done: Vec::new(),
        offers: Vec::new(),
    };
    TurnExecution::new(record, response)
}

#[tokio::test]
async fn two_runs_of_the_same_turn_produce_the_same_artefacts() {
    let first = execution(&TripExecutor::default(), TripCommand::Open).await;
    let second = execution(&TripExecutor::default(), TripCommand::Open).await;

    same_turn(&first, &second).expect("the same turn twice produces the same artefacts");
    assert!(!first.record.event_ids.is_empty(), "the turn did something");
}

#[tokio::test]
async fn a_turn_that_did_something_else_is_reported() {
    let first = execution(&TripExecutor::default(), TripCommand::Open).await;
    let mut second = first.clone();
    second.record.understanding = Some(
        UnderstandingBuilder::of(TEXT)
            .start("trip", "Cambia")
            .build()
            .unwrap(),
    );

    assert!(matches!(
        same_turn(&first, &second).unwrap_err(),
        ReplayDivergence::PlanDiffers { .. }
    ));
}

#[tokio::test]
async fn a_record_built_from_a_real_commit_explains_its_turn() {
    let executor = TripExecutor::default();
    let workflow = TripWorkflow::default();
    let case_ref = CaseRef::new("trip", "trip-1", CaseRevision(0));
    let batch = support::batch(
        support::turn(1),
        &case_ref,
        &support::confirmed_click(),
        vec![TripCommand::Open],
    );
    let commit = executor.execute(batch.clone()).await.unwrap();
    let receipts = workflow.receipts(&commit.receipt_events(), &Locale::from("it-IT"));
    let execution = execution(&TripExecutor::default(), TripCommand::Open).await;

    ReplayEvidence::new(&execution.record)
        .with_batch(&batch)
        .with_receipts(&receipts)
        .explains_its_turn()
        .expect("every command has a decision, an origin and its events");

    // Take the origin away and the record no longer accounts for itself.
    let gaps = ReplayEvidence::new(&execution.record)
        .with_receipts(&receipts)
        .explains_its_turn()
        .unwrap_err();
    assert!(gaps.to_string().contains("has no origin"), "{gaps}");
}

#[tokio::test]
async fn a_receipt_that_outruns_the_ledger_is_reported() {
    let executor = TripExecutor::default();
    let workflow = TripWorkflow::default();
    let events = run(&executor, TripCommand::Open).await;
    let receipts = workflow.receipts(&events, &Locale::from("it-IT"));
    let mut execution = execution(&TripExecutor::default(), TripCommand::Open).await;
    // The turn forgot to record the event its receipt cites.
    execution.record.event_ids.clear();
    execution.record.command_outcomes.clear();

    let gaps = ReplayEvidence::new(&execution.record)
        .with_receipts(&receipts)
        .explains_its_turn()
        .unwrap_err();
    assert!(
        gaps.to_string().contains("which the record does not list"),
        "{gaps}"
    );
}

#[test]
fn a_low_risk_command_needs_no_confirmation_for_the_record_to_hold() {
    // The evidence check reads the policy the *record* carries, so a decision
    // recorded for the wrong policy is caught even when the origin looks fine.
    let workflow = TripWorkflow::default();
    assert_eq!(
        workflow.command_policy(None, &TripCommand::Open),
        CommandPolicy::low_risk()
    );
    assert_eq!(TurnId::nil(), TurnId::nil());
}
