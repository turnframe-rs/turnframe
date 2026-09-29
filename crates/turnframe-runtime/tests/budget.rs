//! The resource budget of the sandboxed autonomous mode is enforced (§11.1).
//!
//! It bounds the turn's model calls, prompt tokens and wall clock, understanding's
//! calls included. A bound spent before the commit fails the turn with nothing run. One that runs out after it stops the model calls and leaves the effects, and
//! the receipts that describe them, exactly as they are (§23.1).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use support::{Harness, SteppingClock, notice_codes, receipt_codes, silent, token_for};
use turnframe_core::error::{OrchestratorError, PolicyError};
use turnframe_core::ids::TurnId;
use turnframe_core::replay::TurnPhase;
use turnframe_core::response::ResponseBlock;
use turnframe_core::understanding::Understanding;
use turnframe_runtime::config::{
    OrchestrationMode, OrchestratorConfig, ResourceBudget, SandboxAcknowledgement,
};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "Set the name to Lisbon";

fn turn_id() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn understanding() -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .apply(
            operations::SET_NAME,
            token_for(turn_id(), "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            TEXT,
        )
        .build()
        .unwrap()
}

/// The sandboxed mode, with `budget` and nothing else changed.
fn sandboxed(budget: ResourceBudget) -> OrchestratorConfig {
    OrchestratorConfig::conservative().with_mode(OrchestrationMode::sandboxed_autonomous(
        budget,
        SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes(),
    ))
}

/// The name of the bound that stopped a turn.
fn stopped_by(error: &OrchestratorError) -> &str {
    match error {
        OrchestratorError::Policy(PolicyError::BudgetExhausted { limit }) => limit,
        other => panic!("the turn stopped for another reason: {other}"),
    }
}

/// Every call understanding makes of [`TEXT`], answered: five model calls.
fn understanding_tasks() -> Arc<ScriptedTasks> {
    let unit =
        serde_json::json!({"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"});
    let value = serde_json::json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    Arc::new(
        ScriptedTasks::new("tasks", "small")
            .answer("turn/segment", serde_json::json!({"analysis": "One request.", "units": [unit]}))
            .answer("turn/coverage", serde_json::json!({"missed": []}))
            .answer("u1/route", serde_json::json!({"operations": [operations::SET_NAME]}))
            .answer("u1/extract", serde_json::json!({"arguments": {"value": value}}))
            .answer(
                "u1/verify",
                serde_json::json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}),
            ),
    )
}

/// A turn understood by the real task pipeline, under `budget`.
async fn understood_under(budget: ResourceBudget) -> Result<(), OrchestratorError> {
    let harness = Harness::builder()
        .config(sandboxed(budget))
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(understanding_tasks())
        .without_narration()
        .build()
        .await;
    let outcome = harness
        .handle(harness.turn(turn_id(), TEXT))
        .await
        .map(|_| ());
    if outcome.is_err() {
        assert!(
            harness.events("trip", "trip-1").await.is_empty(),
            "nothing ran"
        );
    }
    outcome
}

#[tokio::test]
async fn a_turn_that_spends_its_model_calls_stops_before_anything_runs() {
    let error = understood_under(ResourceBudget::conservative().with_max_model_calls(2))
        .await
        .expect_err("understanding alone made five calls");
    assert_eq!(stopped_by(&error), "model_calls");
}

#[tokio::test]
async fn a_turn_that_spends_its_prompt_tokens_stops_before_anything_runs() {
    let error = understood_under(ResourceBudget::conservative().with_max_prompt_tokens(10))
        .await
        .expect_err("understanding's prompts are longer than ten tokens");
    assert_eq!(stopped_by(&error), "prompt_tokens");
}

#[tokio::test]
async fn a_budget_that_covers_understanding_lets_the_turn_run() {
    understood_under(ResourceBudget::conservative())
        .await
        .expect("the conservative budget covers one understood request");
}

#[tokio::test]
async fn a_turn_that_runs_out_of_wall_clock_stops_before_anything_runs() {
    let harness = Harness::builder()
        .config(sandboxed(
            ResourceBudget::conservative().with_max_wall_clock(Duration::from_secs(1)),
        ))
        .clock(Arc::new(SteppingClock::every(10)))
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding())
        .provider(silent().build_shared())
        .build()
        .await;

    let error = harness
        .handle(harness.turn(turn_id(), TEXT))
        .await
        .expect_err("the budget stops the turn");

    assert_eq!(stopped_by(&error), "wall_clock");
    assert!(harness.events("trip", "trip-1").await.is_empty());
}

#[tokio::test]
async fn a_budget_that_runs_out_after_the_commit_keeps_the_effects_and_stops_writing() {
    // The clock is read at the start of the turn and at each checkpoint, so
    // twenty-five seconds is past both pre-commit checks and short of the one
    // that guards narration.
    let harness = Harness::builder()
        .config(sandboxed(
            ResourceBudget::conservative().with_max_wall_clock(Duration::from_secs(25)),
        ))
        .clock(Arc::new(SteppingClock::every(10)))
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding())
        .provider(silent().build_shared())
        .build()
        .await;

    let answer = harness.handle(harness.turn(turn_id(), TEXT)).await.unwrap();

    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set"],
        "the commit stands: it happened before the budget ran out"
    );
    assert_eq!(
        receipt_codes(&answer),
        vec!["trip.name_set"],
        "and the receipt still describes it"
    );
    assert!(
        !answer
            .blocks
            .iter()
            .any(|block| matches!(block, ResponseBlock::Transition(_))),
        "but no model was asked to write about it"
    );
    assert_eq!(harness.providers[0].call_count(), 0, "not even once");
    assert!(
        notice_codes(&answer).contains(&"turnframe.notice.budget_exhausted".to_owned()),
        "and the turn says why: {:?}",
        notice_codes(&answer)
    );
    assert_eq!(harness.phase(turn_id()).await, TurnPhase::Delivered);
}
