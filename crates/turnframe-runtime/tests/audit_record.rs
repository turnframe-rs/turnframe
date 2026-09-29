//! A stored audit record answers, on its own, every question the audit trail
//! is required to answer.
//!
//! The interesting property is not that the fields exist — a struct definition
//! proves that — but that a record *read back from storage*, after a turn nobody
//! instrumented for the occasion, is sufficient to answer them. An auditor arrives
//! after the fact with a turn identifier and nothing else. If answering needs the
//! batches the runtime held in memory, or the reduction it discarded, the record is
//! a summary rather than an audit trail.
//!
//! So each test below runs a real turn through the orchestrator, its understanding
//! made by the real task pipeline, throws away everything it returned, reads the
//! record out of the store by its identifier, and answers from that alone.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::replay::{CommandOutcome, ReplayRecord, TaskVerdict, TurnPhase};
use turnframe_core::target::TargetResolution;
use turnframe_core::understanding::{ArgumentValue, MessageRef};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

const TEXT: &str = "Set the name to Lisbon";

/// The provider that answers every task call of the turn.
const TASKS_PROVIDER: &str = "tasks";

/// Every task call understanding makes of [`TEXT`], answered.
fn understanding_tasks() -> Arc<ScriptedTasks> {
    let unit = json!({"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"});
    let value =
        json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    Arc::new(
        ScriptedTasks::new(TASKS_PROVIDER, "small")
            .answer(
                "turn/segment",
                json!({"analysis": "One request.", "units": [unit]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
            .answer("u1/extract", json!({"arguments": {"value": value}}))
            .answer(
                "u1/verify",
                json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}),
            ),
    )
}

/// Runs one ordinary turn and returns the record as an auditor would find it:
/// read out of the store by turn identifier, with nothing carried over from the
/// run that produced it.
async fn audited_turn() -> (Harness, TurnId, ReplayRecord) {
    let turn_id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(understanding_tasks())
        .without_narration()
        .build()
        .await;

    let returned = harness.handle(harness.turn(turn_id, TEXT)).await;
    assert!(returned.is_ok(), "the turn under audit must have succeeded");
    drop(returned);

    let record = harness
        .stores
        .replay_record(&harness.account(), &turn_id)
        .await
        .expect("an auditor finds the record by turn identifier alone");
    (harness, turn_id, record)
}

#[tokio::test]
async fn the_record_answers_what_the_user_sent() {
    let (harness, turn_id, record) = audited_turn().await;

    assert_eq!(record.turn_id, turn_id);
    assert_eq!(record.account_id, harness.account());

    // The record cites the turn; the turn itself is the conversation store's, so
    // one copy of the user's words is kept rather than two that can disagree.
    let stored = harness
        .stores
        .turn(&record.account_id, &record.turn_id)
        .await
        .expect("the turn the record names was persisted");
    assert_eq!(
        stored.user.input.text.as_deref(),
        Some(TEXT),
        "what the user sent is recoverable from the record's own identifiers"
    );
}

#[tokio::test]
async fn the_record_answers_which_state_and_revision_were_loaded() {
    let (_harness, _turn_id, record) = audited_turn().await;

    let loaded = record
        .loaded_cases
        .iter()
        .find(|case| case.case_id.as_str() == "trip-1")
        .expect("the case the turn addressed is named");
    assert_eq!(
        loaded.expected_revision.0, 3,
        "the revision the turn read, not the one it left behind"
    );

    let version = record
        .workflow_versions
        .iter()
        .find(|entry| entry.key.as_str() == "trip")
        .expect("the projector version is named");
    assert_eq!(
        version.version.as_str(),
        "1",
        "which projector produced the view the model was shown"
    );
}

#[tokio::test]
async fn the_record_answers_what_the_model_understood_and_what_grounded_it() {
    let (_harness, _turn_id, record) = audited_turn().await;

    let understanding = record
        .understanding
        .as_ref()
        .expect("the understanding is kept, not only its hash");
    assert_eq!(
        record.plan_hash,
        understanding.hash().ok(),
        "and the hash that identifies it"
    );

    let act = understanding
        .acts
        .first()
        .expect("the understanding carries the act");
    assert_eq!(
        act.operation().map(|operation| operation.as_str()),
        Some(operations::SET_NAME)
    );

    // Grounding is what makes a reading auditable rather than merely recorded:
    // the act and its value point at the user's own words.
    assert_eq!(
        &TEXT[act.words.start..act.words.end],
        TEXT,
        "the act is a span of what the user actually sent"
    );
    let value = &act.arguments["value"];
    assert_eq!(value.value, ArgumentValue::Json(json!("Lisbon")));
    let excerpt = value.excerpt.expect("the value cites its words");
    assert_eq!(excerpt.message, MessageRef::Current);
    assert_eq!(&TEXT[excerpt.words.start..excerpt.words.end], "Lisbon");
}

#[tokio::test]
async fn the_record_answers_which_model_calls_produced_the_understanding() {
    let (_harness, _turn_id, record) = audited_turn().await;

    let called: BTreeSet<&str> = record
        .tasks
        .iter()
        .map(|task| task.task_id.as_str())
        .collect();
    assert_eq!(
        called,
        BTreeSet::from([
            "turn/segment",
            "turn/coverage",
            "u1/route",
            "u1/extract",
            "u1/verify"
        ]),
        "every call the understanding rests on is named"
    );
    for task in &record.tasks {
        assert!(
            task.prompt_ref.is_some(),
            "{} names the instructions it ran under",
            task.task_id
        );
        assert!(
            task.input_digest.is_some(),
            "{} identifies what it was shown, schema included",
            task.task_id
        );
        assert_eq!(
            task.provider_key.as_ref().map(AsRef::as_ref),
            Some(TASKS_PROVIDER),
            "{} names who answered it",
            task.task_id
        );
        assert!(
            matches!(task.verdict, TaskVerdict::Accepted),
            "{} says what was made of the answer: {:?}",
            task.task_id,
            task.verdict
        );
    }

    let budget = record
        .budget
        .as_ref()
        .expect("the record says what was spent");
    assert_eq!(
        usize::try_from(budget.model_calls).unwrap(),
        record.tasks.len(),
        "and what it spent is the calls it names"
    );
    assert_eq!(budget.exhausted, None, "with no bound reached");
}

#[tokio::test]
async fn the_record_answers_how_the_target_was_resolved() {
    let (_harness, _turn_id, record) = audited_turn().await;

    let resolution = record
        .target_resolutions
        .first()
        .expect("the turn resolved a target and said so");
    match &resolution.resolution {
        TargetResolution::Exact { case_ref } => {
            assert_eq!(
                case_ref.case_id.as_str(),
                "trip-1",
                "which record the act was bound to, not merely that it was bound"
            );
        }
        other => panic!("expected an exact resolution, got {other:?}"),
    }
}

#[tokio::test]
async fn the_record_answers_which_policy_applied_and_what_authorized_the_command() {
    let (_harness, _turn_id, record) = audited_turn().await;

    let outcome = record
        .command_outcomes
        .first()
        .expect("the turn ran a command");
    let decision = record
        .policy_decisions
        .iter()
        .find(|decision| decision.command_ref == outcome.command_ref)
        .expect("every command the record lists carries the decision that let it run");
    assert!(decision.allowed, "and the decision says it was allowed");

    let origin = outcome
        .origin
        .as_ref()
        .expect("the record names what authorized the command, without reloading the batch");
    assert!(
        turnframe_core::command::origin_satisfies(origin, &decision.policy),
        "and that origin actually satisfies the policy recorded beside it"
    );
}

#[tokio::test]
async fn the_record_answers_what_committed_and_which_event_backs_each_receipt() {
    let (harness, _turn_id, record) = audited_turn().await;

    let outcome = record
        .command_outcomes
        .first()
        .expect("the turn ran a command");
    let CommandOutcome::Committed {
        new_revision,
        event_ids,
    } = &outcome.outcome
    else {
        panic!("expected a committed outcome, got {:?}", outcome.outcome);
    };
    assert_eq!(new_revision.0, 4, "the revision the commit reached");
    assert!(!event_ids.is_empty(), "and the events it produced");

    for id in event_ids {
        assert!(
            record.event_ids.contains(id),
            "an event a command produced is listed by the record itself"
        );
    }

    // The events are in the ledger, not merely named by the record, which is
    // what makes a receipt's citation checkable rather than self-asserted.
    let stored = harness
        .stores
        .events_by_ids(&record.account_id, &record.event_ids)
        .await
        .expect("the ledger answers");
    assert_eq!(
        stored.len(),
        record.event_ids.len(),
        "every event the record cites is in the ledger it points at"
    );
}

#[tokio::test]
async fn the_record_answers_what_was_returned_to_the_user() {
    let (harness, _turn_id, record) = audited_turn().await;

    assert!(
        !record.response_block_ids.is_empty(),
        "the record names the blocks the answer was made of"
    );

    let returned = harness
        .stores
        .turn(&record.account_id, &record.turn_id)
        .await
        .expect("the turn was persisted")
        .assistant
        .expect("a delivered turn has an answer");
    let ids: Vec<_> = returned.blocks.iter().map(block_id).collect();
    assert_eq!(
        ids, record.response_block_ids,
        "and they are the blocks that were actually returned, in the order returned"
    );
}

#[tokio::test]
async fn the_record_says_how_far_the_turn_got() {
    let (_harness, _turn_id, record) = audited_turn().await;

    assert_eq!(
        record.phase,
        TurnPhase::Delivered,
        "a turn that answered says so, which is what tells recovery to leave it alone"
    );
}

/// The whole list at once, so a field that stops being populated fails here
/// rather than in whichever individual question happened to cover it.
#[tokio::test]
async fn one_stored_record_answers_the_whole_audit_list() {
    let (harness, _turn_id, record) = audited_turn().await;
    let mut unanswered: Vec<&str> = Vec::new();

    if harness
        .stores
        .turn(&record.account_id, &record.turn_id)
        .await
        .ok()
        .and_then(|stored| stored.user.input.text.clone())
        .is_none()
    {
        unanswered.push("what did the user send");
    }
    if record.loaded_cases.is_empty() {
        unanswered.push("which state and revision were loaded");
    }
    if record.workflow_versions.is_empty() {
        unanswered.push("which projector produced the view");
    }
    if record.understanding.is_none() {
        unanswered.push("what did the model understand");
    }
    if record.understanding.as_ref().is_none_or(|understanding| {
        understanding.acts.iter().any(|act| {
            TEXT.get(act.words.start..act.words.end)
                .is_none_or(str::is_empty)
        })
    }) {
        unanswered.push("what words grounded each act");
    }
    if record.tasks.is_empty() {
        unanswered.push("which model calls produced the understanding");
    }
    if record.budget.is_none() {
        unanswered.push("what those calls spent");
    }
    if record.target_resolutions.is_empty() {
        unanswered.push("how was the target resolved");
    }
    if record.policy_decisions.is_empty() {
        unanswered.push("which policy applied");
    }
    if record
        .command_outcomes
        .iter()
        .any(|outcome| outcome.origin.is_none())
    {
        unanswered.push("which interaction or act authorized the command");
    }
    if record.command_outcomes.is_empty() {
        unanswered.push("what committed");
    }
    if record.event_ids.is_empty() {
        unanswered.push("which event authorized each receipt");
    }
    if record.response_block_ids.is_empty() {
        unanswered.push("what was returned to the user");
    }

    assert!(
        unanswered.is_empty(),
        "a stored record left these unanswerable: {unanswered:?}"
    );
}

/// The identifier of a response block, whatever kind it is.
fn block_id(block: &turnframe_core::response::ResponseBlock) -> turnframe_core::ids::BlockId {
    use turnframe_core::response::ResponseBlock;
    match block {
        ResponseBlock::Answer(answer) => answer.block_id.clone(),
        ResponseBlock::Transition(transition) => transition.block_id.clone(),
        ResponseBlock::Receipt(receipt) => receipt.block_id.clone(),
        ResponseBlock::Notice(notice) => notice.block_id.clone(),
        ResponseBlock::Interaction(interaction) => interaction.block_id.clone(),
        ResponseBlock::Artifact(artifact) => artifact.block_id.clone(),
        _ => turnframe_core::ids::BlockId::from("unknown"),
    }
}
