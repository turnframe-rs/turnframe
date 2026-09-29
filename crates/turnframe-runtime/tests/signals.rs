//! Every declared signal, driven through the path that emits it (spec §26.2, §28).
//!
//! Each test drives the real pipeline into the situation a signal describes and asserts
//! it was observed exactly once, with exactly the labels `turnframe-telemetry` documents
//! for it and nothing of higher cardinality. Nothing here calls an emitting function
//! directly, so a signal that stops being wired fails a test instead of emptying a panel.
//!
//! The task signals come from the real understanding pipeline, its model calls answered
//! per task id by [`ScriptedTasks`].
//!
//! The last test is the guard: it unions what every driver observed and holds it against
//! [`Signal::ALL`]. A new signal needs a driver here, or an entry in [`undriven`] saying
//! why it cannot have one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use turnframe_core::effort::Effort;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use support::{
    FixedKnowledge, Harness, Observed, RecordingObserver, assert_labels, narrating, silent,
    token_for, transient_failure,
};
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::command::IdempotencyKey;
use turnframe_core::error::{DomainRejection, ExecutionError, RevisionConflict, StoreError};
use turnframe_core::event::{OutboxEntry, OutboxStatus};
use turnframe_core::ids::{CaseRevision, CommandId, ConversationId, OutboxId, TurnId, WorkflowKey};
use turnframe_core::interaction::InteractionKind;
use turnframe_core::observe::{Signal, SignalLabels};
use turnframe_core::plan::AnswerBasis;
use turnframe_core::turn::{ActorContext, TurnInput};
use turnframe_core::understanding::{ActTarget, Understanding};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_runtime::config::{OrchestratorConfig, UnderstandingConfig};
use turnframe_runtime::dispatch::{
    DispatchConfig, Dispatched, OutboxDispatcher, OutboxReconciler, OutboxSender, Reconciled,
};
use turnframe_runtime::orchestrator::{CaseCandidate, CaseDirectory};
use turnframe_runtime::policy::CONFIRM_OPTION_ID;
use turnframe_store::memory::MemoryStores;
use turnframe_store::outbox::OutboxWriter;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_tasks::{Budget, TaskKind, TaskProfiles};
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{complete_case, incomplete_case, operations};

// ---------------------------------------------------------------------------
// Shared fixtures.
// ---------------------------------------------------------------------------

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// The provider key and model key every scripted narrator in this file uses,
/// which is what the provider signals must carry.
const PROVIDER: &str = "scripted";
const MODEL: &str = "model-1";

/// The keys of the provider that answers understanding tasks.
const TASKS_PROVIDER: &str = "tasks";
const TASKS_MODEL: &str = "small";

/// The labels a provider signal carries for `purpose`: the configured keys and
/// the normalized purpose, and never a request id.
fn provider_labels(purpose: ModelPurpose) -> SignalLabels {
    SignalLabels::none()
        .with_provider(PROVIDER)
        .with_model(MODEL)
        .with_purpose(purpose.as_str())
}

/// The labels every task signal carries: the task's kind as its purpose.
fn task_labels(kind: TaskKind) -> SignalLabels {
    SignalLabels::none()
        .with_effort(Effort::Medium)
        .with_purpose(kind.as_str())
}

/// The one occurrence of `signal` for tasks of `kind`, failing when there is not
/// exactly one: a task signal fires per task, so the count is per purpose.
fn once_for(seen: &RecordingObserver, signal: Signal, kind: TaskKind) -> Observed {
    let found: Vec<Observed> = seen
        .occurrences(signal)
        .into_iter()
        .filter(|observed| observed.labels.purpose.as_deref() == Some(kind.as_str()))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{} fired {} times for {}, not once; the turn saw {:?}",
        signal.name(),
        found.len(),
        kind.as_str(),
        seen.distinct()
    );
    found.into_iter().next().expect("exactly one")
}

fn trip_workflow() -> WorkflowKey {
    WorkflowKey::from("trip")
}

/// An understanding that sets the name of `inv-1`.
fn set_subject(turn_id: TurnId, text: &str) -> Understanding {
    UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .build()
        .unwrap()
}

/// An understanding that cancels `inv-1`, which the conservative policy holds
/// behind a confirmation card.
fn cancel(turn_id: TurnId, text: &str) -> Understanding {
    UnderstandingBuilder::of(text)
        .apply(
            operations::WITHDRAW,
            token_for(turn_id, "trip", "trip-1"),
            json!(null),
            "Cancel that trip",
        )
        .build()
        .unwrap()
}

const SUBJECT_TEXT: &str = "Set the name to Lisbon";
const CANCEL_TEXT: &str = "Cancel that trip";

/// Four words that ask for nothing, which the real pipeline reads in two calls.
const THANKS_TEXT: &str = "Thanks, that is all";

/// The rejection the armed executor answers with.
fn locked() -> ExecutionError {
    ExecutionError::Rejected(DomainRejection::new("trip.locked", "trip.error.locked"))
}

/// A segmentation of [`THANKS_TEXT`] as chitchat over words `from` to `to`, counted
/// from 1 as a model counts them.
fn chitchat(from: usize, to: usize) -> Value {
    json!({
        "analysis": "Thanks; nothing is asked.",
        "units": [{"kind": "chitchat", "words": {"from": from, "to": to}}]
    })
}

/// A segmentation the task's own check refuses: every message has a unit.
fn no_units() -> Value {
    json!({"analysis": "Nothing.", "units": []})
}

fn nothing_missed() -> Value {
    json!({"missed": []})
}

/// The conservative configuration with understanding changed by `change`.
fn understanding(
    change: impl FnOnce(UnderstandingConfig) -> UnderstandingConfig,
) -> OrchestratorConfig {
    let config = OrchestratorConfig::conservative();
    let understanding = change(config.understanding.clone());
    config.with_understanding(understanding)
}

// ---------------------------------------------------------------------------
// Drivers. Each one runs the real pipeline and hands back what was observed.
// ---------------------------------------------------------------------------

/// One ordinary turn: one case, one command, one narration call.
async fn drive_plain_turn() -> Arc<RecordingObserver> {
    let id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(set_subject(id, SUBJECT_TEXT))
        .provider(narrating().build_shared())
        .observing()
        .build()
        .await;
    harness
        .handle(harness.turn(id, SUBJECT_TEXT))
        .await
        .unwrap();
    harness.observed()
}

/// The same turn with narration switched off, so the turn makes no provider call.
async fn drive_silent_turn() -> Arc<RecordingObserver> {
    let id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(set_subject(id, SUBJECT_TEXT))
        .without_narration()
        .observing()
        .build()
        .await;
    harness
        .handle(harness.turn(id, SUBJECT_TEXT))
        .await
        .unwrap();
    harness.observed()
}

/// A card written in one workspace, answered from another one in the same
/// account and the same conversation (spec §25.4).
async fn drive_scope_change() -> Arc<RecordingObserver> {
    let first = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(cancel(first, CANCEL_TEXT))
        .case_directory(Arc::new(WorkspaceDirectory::listing("north", "trip-1")))
        .without_narration()
        .observing()
        .build()
        .await;

    let asked = harness
        .handle(in_workspace(harness.turn(first, CANCEL_TEXT), "north"))
        .await
        .unwrap();
    let card = asked.interactions().next().expect("a confirmation").id;
    let revision = harness.trip_revision("trip-1").value();

    // The actor has moved. The directory no longer lists the case, and its
    // default answer to "may this card still name it?" is no.
    let refused = harness
        .handle(in_workspace(
            harness.click(turn(2), card, CONFIRM_OPTION_ID, revision),
            "south",
        ))
        .await;
    assert!(refused.is_err(), "the click is refused");
    harness.observed()
}

/// A destructive command that stops at the card its policy names.
async fn drive_confirmation() -> Arc<RecordingObserver> {
    let (harness, _card, _revision) = confirmation_pending(None).await;
    harness.observed()
}

/// The same, then the click that authorizes it, which commits.
async fn drive_confirmed_click() -> Arc<RecordingObserver> {
    let (harness, card, revision) = confirmation_pending(None).await;
    harness
        .handle(harness.click(turn(2), card, CONFIRM_OPTION_ID, revision))
        .await
        .unwrap();
    harness.observed()
}

/// The same click, with the domain refusing the command it authorized, so the
/// card is left failed.
async fn drive_refused_click() -> Arc<RecordingObserver> {
    let (harness, card, revision) = confirmation_pending(Some(locked())).await;
    harness.arm_trip_failure();
    harness
        .handle(harness.click(turn(2), card, CONFIRM_OPTION_ID, revision))
        .await
        .unwrap();
    harness.observed()
}

/// The same click, with a revision the traveler made up, which is stale.
async fn drive_stale_click() -> Arc<RecordingObserver> {
    let (harness, card, revision) = confirmation_pending(None).await;
    let refused = harness
        .handle(harness.click(turn(2), card, CONFIRM_OPTION_ID, revision + 1))
        .await;
    assert!(
        refused.is_err(),
        "a revision nobody rendered is not an answer"
    );
    harness.observed()
}

/// Runs the turn that opens a confirmation card and returns the harness, the
/// card and the revision it was bound to.
async fn confirmation_pending(
    failure: Option<ExecutionError>,
) -> (Harness, turnframe_core::ids::InteractionId, u64) {
    let first = turn(1);
    let mut builder = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(cancel(first, CANCEL_TEXT))
        .without_narration()
        .observing();
    if let Some(failure) = failure {
        builder = builder.trip_fails_once_armed(failure);
    }
    let harness = builder.build().await;
    let asked = harness
        .handle(harness.turn(first, CANCEL_TEXT))
        .await
        .unwrap();
    let card = asked.interactions().next().expect("a confirmation").id;
    let revision = harness.trip_revision("trip-1").value();
    (harness, card, revision)
}

/// A narrator that states an outcome nothing committed, so the block is
/// withheld (spec §17.4, I16).
async fn drive_withheld_claim() -> Arc<RecordingObserver> {
    let id = turn(1);
    let provider = ScriptedProvider::builder(PROVIDER, MODEL)
        .acknowledging("The trip was sent.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip_fails(locked())
        .understands(set_subject(id, SUBJECT_TEXT))
        .provider(provider)
        .observing()
        .build()
        .await;
    harness
        .handle(harness.turn(id, SUBJECT_TEXT))
        .await
        .unwrap();
    harness.observed()
}

/// One narrator that fails every attempt and one that answers.
async fn drive_provider_fallback() -> Arc<RecordingObserver> {
    let id = turn(1);
    let failing = ScriptedProvider::builder("first", "model-a")
        .failing(transient_failure())
        .failing(transient_failure())
        .failing(transient_failure())
        .failing(transient_failure())
        .build_shared();
    let healthy = ScriptedProvider::builder("second", "model-b")
        .acknowledging("Right, here is where that leaves things.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(set_subject(id, SUBJECT_TEXT))
        .provider(failing)
        .provider(healthy)
        .observing()
        .build()
        .await;
    harness
        .handle(harness.turn(id, SUBJECT_TEXT))
        .await
        .unwrap();
    harness.observed()
}

/// A turn whose only narrator returns no structured output: routing refuses it for
/// the acknowledgement, whose answer is a schema.
async fn drive_capability_mismatch() -> Arc<RecordingObserver> {
    let id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(set_subject(id, SUBJECT_TEXT))
        .provider(
            narrating()
                .structured_output(
                    turnframe_provider::capabilities::StructuredOutputCapability::None,
                )
                .build_shared(),
        )
        .observing()
        .build()
        .await;
    harness
        .handle(harness.turn(id, SUBJECT_TEXT))
        .await
        .unwrap();
    harness.observed()
}

/// Two trips answer to the same name, so the act resolves to neither.
async fn drive_ambiguous_target() -> Arc<RecordingObserver> {
    let id = turn(1);
    let text = "Set the name on the Ferri trip to Lisbon";
    let understanding = UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_NAME,
            ActTarget::Ambiguous {
                candidates: vec![
                    token_for(id, "trip", "trip-1"),
                    token_for(id, "trip", "trip-2"),
                ],
            },
            json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;
    harness.handle(harness.turn(id, text)).await.unwrap();
    harness.observed()
}

/// A record named that is not among those the actor may address.
async fn drive_missing_target() -> Arc<RecordingObserver> {
    let id = turn(1);
    let text = "Set the name on the Bianchi trip to Lisbon";
    let understanding = UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_NAME,
            ActTarget::NotListed {
                workflow: trip_workflow(),
                words: None,
            },
            json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;
    harness.handle(harness.turn(id, text)).await.unwrap();
    harness.observed()
}

/// An act whose target shape the operation does not accept.
///
/// Unlike an ambiguous or missing target, this one produces no resolution at
/// all, so the act leaves no target record, no policy decision and no command.
/// The signal is the only thing that says the turn did less than it said.
async fn drive_unresolved_target() -> Arc<RecordingObserver> {
    let id = turn(1);
    let text = "Cancel that";
    // A cancellation takes only a record in view, so aiming it at a new one is
    // a shape its own rule refuses and the act reaches no case at all.
    let understanding = UnderstandingBuilder::of(text)
        .open(operations::WITHDRAW, "trip", json!(null), text)
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;
    harness.handle(harness.turn(id, text)).await.unwrap();
    harness.observed()
}

/// An act the domain refuses during reduction: it never becomes a command, so
/// `CommandRejected` never fires for it.
async fn drive_refused_act() -> Arc<RecordingObserver> {
    let id = turn(1);
    let text = "Set the name to nothing at all";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(id, "trip", "trip-1"),
            json!({"value": "   "}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;
    harness.handle(harness.turn(id, text)).await.unwrap();
    harness.observed()
}

/// Two acts of one operation about one name: a correction.
///
/// The later value wins and the earlier act is dropped, which is right, and
/// countable, because dropping an act makes a turn do less than it was asked.
async fn drive_superseded_act() -> Arc<RecordingObserver> {
    let id = turn(1);
    let text = "Set the name to Lisbon, no, to Porto";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(id, "trip", "trip-1"),
            json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .superseded_by_next()
        .apply(
            operations::SET_NAME,
            token_for(id, "trip", "trip-1"),
            json!({"value": "Porto"}),
            "no, to Porto",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;
    harness.handle(harness.turn(id, text)).await.unwrap();
    harness.observed()
}

/// A command whose transport timed out after it left: uncertainty, not failure
/// (I15).
async fn drive_unknown_external_outcome() -> Arc<RecordingObserver> {
    let id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip_fails(ExecutionError::Timeout)
        .understands(set_subject(id, SUBJECT_TEXT))
        .without_narration()
        .observing()
        .build()
        .await;
    harness
        .handle(harness.turn(id, SUBJECT_TEXT))
        .await
        .unwrap();
    harness.observed()
}

/// A command planned against a revision the case has left (I13).
async fn drive_revision_conflict() -> Arc<RecordingObserver> {
    let id = turn(1);
    let conflict = ExecutionError::RevisionConflict(RevisionConflict {
        expected: CaseRef::new("trip", "trip-1", CaseRevision(3)),
        current_revision: CaseRevision(4),
    });
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip_fails(conflict)
        .understands(set_subject(id, SUBJECT_TEXT))
        .without_narration()
        .observing()
        .build()
        .await;
    harness
        .handle(harness.turn(id, SUBJECT_TEXT))
        .await
        .unwrap();
    harness.observed()
}

/// A question the turn can answer from committed state.
async fn drive_question_answered() -> Arc<RecordingObserver> {
    let id = turn(1);
    let text = "Tell me when it flies";
    // Answering is part of narration, so narration stays on. The question is about the
    // trip, so the reply also asks what it needs next.
    let provider = ScriptedProvider::builder(PROVIDER, MODEL)
        .answering("It has no travel date yet.")
        .acknowledging("It has no travel date yet. Which day would you rather fly?")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(UnderstandingBuilder::of(text).ask(text).build().unwrap())
        .provider(provider)
        .observing()
        .build()
        .await;
    harness.handle(harness.turn(id, text)).await.unwrap();
    harness.observed()
}

/// A question no approved source can settle.
async fn drive_question_unanswered() -> Arc<RecordingObserver> {
    let id = turn(1);
    let text = "Why does a rebooking need a loyalty number?";
    let understanding = UnderstandingBuilder::of(text)
        .ask_about(AnswerBasis::GeneralDomainKnowledge, None, &[], text)
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .knowledge(Arc::new(FixedKnowledge::unavailable()))
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;
    harness.handle(harness.turn(id, text)).await.unwrap();
    harness.observed()
}

/// A workflow whose map claims a phase is the user's while nothing is waiting
/// on the user (spec §8.4).
async fn drive_broken_projection() -> Arc<RecordingObserver> {
    let id = turn(1);
    let harness = Harness::builder()
        .broken_case("broken-1", "A case nobody can project")
        .provider(silent().build_shared())
        .without_narration()
        .observing()
        .build()
        .await;
    let refused = harness.handle(harness.turn(id, SUBJECT_TEXT)).await;
    assert!(refused.is_err(), "a view that breaks §8.4 stops the turn");
    harness.observed()
}

/// The external half of the saga: one row sent, its outcome unknown, then
/// settled against the remote system (spec §16.4, §16.5).
async fn drive_outbox_reconciliation() -> Arc<RecordingObserver> {
    let observer = Arc::new(RecordingObserver::new());
    let stores = Arc::new(MemoryStores::new());
    let row = outbox_row();
    OutboxWriter::enqueue(stores.as_ref(), row.clone())
        .await
        .expect("the row is new");

    let dispatcher = OutboxDispatcher::new(
        Arc::clone(&stores) as Arc<dyn turnframe_store::outbox::OutboxStore>,
        Arc::new(Silent),
        DispatchConfig::new("worker-1").with_send_timeout(Duration::from_millis(20)),
    )
    .with_observer(Arc::clone(&observer) as Arc<dyn turnframe_core::observe::Observer>);

    let swept = dispatcher.run_once(dispatch_now()).await.expect("a sweep");
    assert_eq!(
        swept.unknown.len(),
        1,
        "a rebooking that never answered is unknown"
    );
    let answer = dispatcher
        .reconcile(&row.outbox_id, &Says(Reconciled::Completed), dispatch_now())
        .await
        .expect("the row is readable");
    assert_eq!(answer, Reconciled::Completed);
    observer
}

/// Understands [`THANKS_TEXT`] with the real task pipeline, its calls answered by
/// `tasks` under `config`, and checks every scripted answer was asked for.
async fn drive_understanding(
    tasks: ScriptedTasks,
    config: OrchestratorConfig,
) -> Arc<RecordingObserver> {
    let tasks = Arc::new(tasks);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(Arc::clone(&tasks))
        .config(config)
        .without_narration()
        .observing()
        .build()
        .await;
    harness
        .handle(harness.turn(turn(1), THANKS_TEXT))
        .await
        .unwrap();
    assert!(
        tasks.unanswered().is_empty(),
        "every scripted task was asked; asked {:?}, left {:?}",
        tasks.called(),
        tasks.unanswered()
    );
    harness.observed()
}

fn tasks() -> ScriptedTasks {
    ScriptedTasks::new(TASKS_PROVIDER, TASKS_MODEL)
}

/// A message read by the pipeline in two accepted calls: segment, then coverage.
async fn drive_understood_turn() -> Arc<RecordingObserver> {
    let script = tasks()
        .answer("turn/segment", chitchat(1, 4))
        .answer("turn/coverage", nothing_missed());
    drive_understanding(script, OrchestratorConfig::conservative()).await
}

/// A segmentation its check refuses, answered correctly in the repair round.
async fn drive_repaired_task() -> Arc<RecordingObserver> {
    let script = tasks()
        .answer("turn/segment", no_units())
        .answer("turn/segment#repair1", chitchat(1, 4))
        .answer("turn/coverage", nothing_missed());
    drive_understanding(script, OrchestratorConfig::conservative()).await
}

/// The same refused segmentation with no repair round, run again on the stronger
/// model the profile names.
async fn drive_escalated_task() -> Arc<RecordingObserver> {
    let script = tasks()
        .tagged("large")
        .answer("turn/segment", no_units())
        .answer("turn/segment#escalation", chitchat(1, 4))
        .answer("turn/coverage", nothing_missed());
    let config = understanding(|understanding| {
        understanding.with_tasks(TaskProfiles::new().adjust(TaskKind::Segment, |profile| {
            profile.with_repairs(0).escalating_to("large")
        }))
    });
    drive_understanding(script, config).await
}

/// Two votes on the segmentation that read the message differently.
async fn drive_vote_disagreement() -> Arc<RecordingObserver> {
    let script = tasks()
        .answer("turn/segment#vote1", chitchat(1, 4))
        .answer("turn/segment#vote2", chitchat(3, 4));
    let config = understanding(|understanding| {
        understanding.with_tasks(
            TaskProfiles::new().adjust(TaskKind::Segment, |profile| profile.with_votes(2)),
        )
    });
    drive_understanding(script, config).await
}

/// A budget of one model call: segmentation spends it and coverage is refused.
async fn drive_exhausted_budget() -> Arc<RecordingObserver> {
    let script = tasks().answer("turn/segment", chitchat(1, 4));
    let mut budget = Budget::understanding();
    budget.max_model_calls = Some(1);
    let config = understanding(|understanding| understanding.with_budget(budget));
    drive_understanding(script, config).await
}

// ---------------------------------------------------------------------------
// One test per signal this change wired.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_case_the_directory_refuses_is_reported() {
    let seen = drive_scope_change().await;
    let observed = seen.once(Signal::CaseNotAuthorized);
    assert_labels(&observed, &SignalLabels::workflow(trip_workflow()));
}

#[tokio::test]
async fn a_card_whose_command_committed_is_reported_as_resolved() {
    let seen = drive_confirmed_click().await;
    let observed = seen.once(Signal::InteractionResolved);
    assert_labels(
        &observed,
        &SignalLabels::workflow(trip_workflow()).with_interaction(InteractionKind::ConfirmCommand),
    );
}

#[tokio::test]
async fn a_card_whose_command_was_refused_is_reported_as_failed() {
    let seen = drive_refused_click().await;
    let observed = seen.once(Signal::InteractionFailed);
    assert_labels(
        &observed,
        &SignalLabels::workflow(trip_workflow())
            .with_interaction(InteractionKind::ConfirmCommand)
            .with_error_code("trip.locked"),
    );
}

#[tokio::test]
async fn a_settled_unknown_outcome_is_reported_as_reconciled() {
    let seen = drive_outbox_reconciliation().await;
    let observed = seen.once(Signal::ExternalReconciled);
    assert_labels(&observed, &SignalLabels::none());
}

#[tokio::test]
async fn moving_on_to_another_candidate_is_reported_as_a_fallback() {
    let seen = drive_provider_fallback().await;
    let observed = seen.once(Signal::ProviderFallback);
    assert_eq!(
        observed.labels.provider.as_ref().map(AsRef::as_ref),
        Some("first"),
        "the fallback is attributed to the profile that was abandoned"
    );
    assert_eq!(
        observed.labels.model.as_ref().map(AsRef::as_ref),
        Some("model-a")
    );
    assert_eq!(
        observed.labels.purpose.as_deref(),
        Some(ModelPurpose::Acknowledge.as_str())
    );
    assert!(
        observed.labels.error_code.is_some(),
        "with the stable code that made it move on"
    );
    assert_eq!(observed.labels.workflow, None);
    assert_eq!(observed.labels.risk, None);
    assert_eq!(observed.labels.interaction, None);
}

#[tokio::test]
async fn a_profile_refused_for_a_missing_capability_is_reported() {
    let seen = drive_capability_mismatch().await;
    let observed = seen.once(Signal::ProviderCapabilityMismatch);
    assert_labels(
        &observed,
        &provider_labels(ModelPurpose::Acknowledge).with_error_code("structured_output"),
    );
}

#[tokio::test]
async fn a_turn_reports_its_own_wall_clock() {
    let seen = drive_plain_turn().await;
    let observed = seen.once(Signal::TurnDuration);
    assert_labels(&observed, &SignalLabels::none().with_effort(Effort::Medium));
    assert!(
        observed.duration.is_some(),
        "a duration signal carries a measured value"
    );
}

#[tokio::test]
async fn every_projected_case_reports_its_own_time() {
    let seen = drive_plain_turn().await;
    let observed = seen.once(Signal::ProjectionDuration);
    assert_labels(&observed, &SignalLabels::workflow(trip_workflow()));
    assert!(observed.duration.is_some());
}

#[tokio::test]
async fn two_cases_are_two_projections() {
    let seen = drive_ambiguous_target().await;
    assert_eq!(
        seen.count(Signal::ProjectionDuration),
        2,
        "projection time is per case, and the turn addressed two"
    );
}

#[tokio::test]
async fn a_turn_reports_its_reduction_time() {
    let seen = drive_plain_turn().await;
    let observed = seen.once(Signal::ReductionDuration);
    assert_labels(&observed, &SignalLabels::none());
    assert!(observed.duration.is_some());
}

#[tokio::test]
async fn a_turn_reports_the_time_of_its_one_write() {
    let seen = drive_plain_turn().await;
    let observed = seen.once(Signal::PersistenceDuration);
    assert_labels(&observed, &SignalLabels::none());
    assert!(observed.duration.is_some());
}

#[tokio::test]
async fn a_provider_call_reports_its_latency() {
    let seen = drive_plain_turn().await;
    let observed = seen.occurrences(Signal::ProviderLatency);
    let purposes: Vec<Option<&str>> = observed
        .iter()
        .map(|call| call.labels.purpose.as_deref())
        .collect();
    assert_eq!(
        purposes,
        [Some("acknowledge"), Some("review")],
        "one latency per call: the acknowledgement, then its review"
    );
    assert_labels(&observed[0], &provider_labels(ModelPurpose::Acknowledge));
    assert!(observed.iter().all(|call| call.duration.is_some()));
}

#[tokio::test]
async fn every_attempt_reports_its_latency_including_the_abandoned_one() {
    let seen = drive_provider_fallback().await;
    assert!(
        seen.count(Signal::ProviderLatency) >= 2,
        "the candidate that failed is a latency too; it is where the time went"
    );
}

#[tokio::test]
async fn an_external_dispatch_reports_its_latency() {
    let seen = drive_outbox_reconciliation().await;
    let observed = seen.once(Signal::ExternalLatency);
    assert_labels(&observed, &SignalLabels::none());
    assert!(observed.duration.is_some());
}

#[tokio::test]
async fn a_narration_call_reports_its_latency() {
    let seen = drive_plain_turn().await;
    let observed = seen.occurrences(Signal::NarrationLatency);
    assert_eq!(
        observed.len(),
        2,
        "the acknowledgement and its review are narration"
    );
    assert_labels(&observed[0], &provider_labels(ModelPurpose::Acknowledge));
    assert!(observed.iter().all(|call| call.duration.is_some()));
}

#[tokio::test]
async fn a_turn_without_narration_reports_no_narration_latency() {
    let seen = drive_silent_turn().await;
    assert_eq!(
        seen.count(Signal::NarrationLatency),
        0,
        "there was no narration call to time"
    );
}

#[tokio::test]
async fn every_finished_task_is_reported_with_its_purpose_and_verdict() {
    let seen = drive_understood_turn().await;
    for kind in [TaskKind::Segment, TaskKind::Coverage] {
        let observed = once_for(&seen, Signal::TaskCompleted, kind);
        assert_labels(&observed, &task_labels(kind).with_error_code("accepted"));
    }
}

#[tokio::test]
async fn a_task_call_reports_its_latency_and_who_answered_it() {
    let seen = drive_understood_turn().await;
    let observed = once_for(&seen, Signal::TaskLatency, TaskKind::Segment);
    assert_labels(
        &observed,
        &task_labels(TaskKind::Segment)
            .with_provider(TASKS_PROVIDER)
            .with_model(TASKS_MODEL),
    );
    assert!(observed.duration.is_some());
}

#[tokio::test]
async fn an_answer_sent_back_with_its_error_is_reported_as_a_repair() {
    let seen = drive_repaired_task().await;
    let observed = seen.once(Signal::TaskRepaired);
    assert_labels(
        &observed,
        &task_labels(TaskKind::Segment).with_error_code("no_units"),
    );
}

#[tokio::test]
async fn a_task_run_again_on_a_stronger_model_is_reported_as_an_escalation() {
    let seen = drive_escalated_task().await;
    let observed = seen.once(Signal::TaskEscalated);
    assert_labels(
        &observed,
        &task_labels(TaskKind::Segment).with_error_code("no_units"),
    );
}

#[tokio::test]
async fn votes_without_a_majority_are_reported_as_a_disagreement() {
    let seen = drive_vote_disagreement().await;
    let observed = seen.once(Signal::TaskVoteDisagreement);
    assert_labels(&observed, &task_labels(TaskKind::Segment));
}

#[tokio::test]
async fn a_call_the_budget_refuses_is_reported_with_the_bound() {
    let seen = drive_exhausted_budget().await;
    let observed = seen.once(Signal::BudgetExhausted);
    assert_labels(
        &observed,
        &SignalLabels::none()
            .with_effort(Effort::Medium)
            .with_error_code("model_calls"),
    );
}

// ---------------------------------------------------------------------------
// The guard: nothing may be declared and never emitted.
// ---------------------------------------------------------------------------

/// Why a declared signal has no driver in this file.
///
/// The match is exhaustive on purpose: a signal added to the vocabulary cannot
/// reach `main` without either a driver above or a line here saying why it
/// cannot have one.
const fn undriven(signal: Signal) -> Option<&'static str> {
    match signal {
        // An admission that finds a settled journal entry replays the original
        // outcome, never `IdempotentReplay`, so the arm emitting this is unreachable.
        Signal::CommandIdempotencyReplay => Some("the executor never produces this outcome"),
        _ => None,
    }
}

#[tokio::test]
async fn every_declared_signal_is_driven_by_this_suite() {
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    let observers = vec![
        drive_plain_turn().await,
        drive_silent_turn().await,
        drive_scope_change().await,
        drive_confirmation().await,
        drive_confirmed_click().await,
        drive_refused_click().await,
        drive_stale_click().await,
        drive_withheld_claim().await,
        drive_provider_fallback().await,
        drive_capability_mismatch().await,
        drive_ambiguous_target().await,
        drive_missing_target().await,
        drive_unresolved_target().await,
        drive_superseded_act().await,
        drive_refused_act().await,
        drive_unknown_external_outcome().await,
        drive_revision_conflict().await,
        drive_question_answered().await,
        drive_question_unanswered().await,
        drive_broken_projection().await,
        drive_outbox_reconciliation().await,
        drive_understood_turn().await,
        drive_repaired_task().await,
        drive_escalated_task().await,
        drive_vote_disagreement().await,
        drive_exhausted_budget().await,
    ];
    for observer in &observers {
        seen.extend(observer.distinct());
    }

    let missing: Vec<&'static str> = Signal::ALL
        .into_iter()
        .filter(|signal| undriven(*signal).is_none())
        .map(|signal| signal.name())
        .filter(|name| !seen.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "declared but never emitted by this crate's tests: {missing:?}\nthe suite saw: {seen:?}"
    );
}

#[test]
fn nothing_is_exempted_without_a_reason() {
    for signal in Signal::ALL {
        if let Some(reason) = undriven(signal) {
            assert!(
                !reason.is_empty(),
                "{} is exempt for nothing",
                signal.name()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Doubles.
// ---------------------------------------------------------------------------

/// A directory whose scope is a workspace inside the account.
///
/// It lists the cases of the workspace the actor is in and answers nothing
/// else, which is the shape spec §25.4 warns about: the account is the boundary
/// the runtime enforces, and a narrower one lives only here. Its
/// [`authorize_case`](CaseDirectory::authorize_case) is the trait default, so
/// what the test drives is the behaviour an adopter gets for free.
#[derive(Debug)]
struct WorkspaceDirectory {
    workspace: String,
    cases: Vec<CaseCandidate>,
}

impl WorkspaceDirectory {
    fn listing(workspace: &str, case_id: &str) -> Self {
        Self {
            workspace: workspace.to_owned(),
            cases: vec![CaseCandidate::new(CaseKey::new("trip", case_id), "Trip 1")],
        }
    }
}

#[async_trait]
impl CaseDirectory for WorkspaceDirectory {
    async fn candidates(
        &self,
        actor: &ActorContext,
        _conversation: &ConversationId,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        let here = actor
            .attributes
            .get("workspace")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Ok(if here == self.workspace {
            self.cases.clone()
        } else {
            Vec::new()
        })
    }
}

/// The same turn, taken by an actor who is in `workspace`.
fn in_workspace(mut input: TurnInput, workspace: &str) -> TurnInput {
    input.actor.attributes.insert(
        "workspace".to_owned(),
        serde_json::Value::String(workspace.to_owned()),
    );
    input
}

/// A sender that never answers, so the rebooking hits its deadline and the outcome
/// is unknown rather than a retry (I15).
struct Silent;

#[async_trait]
impl OutboxSender for Silent {
    async fn send(&self, _entry: &OutboxEntry) -> Dispatched {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Dispatched::completed()
    }
}

/// A reconciler with a fixed answer.
struct Says(Reconciled);

#[async_trait]
impl OutboxReconciler for Says {
    async fn reconcile(&self, _record: &turnframe_store::outbox::OutboxRecord) -> Reconciled {
        self.0.clone()
    }
}

fn dispatch_now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a valid fixed instant")
}

fn outbox_row() -> OutboxEntry {
    OutboxEntry {
        outbox_id: OutboxId::from(uuid::Uuid::from_u128(9)),
        command_id: CommandId::from(uuid::Uuid::from_u128(9)),
        destination: "airline".to_owned(),
        payload: serde_json::json!({"trip": 1}),
        idempotency_key: IdempotencyKey::new("key-9"),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        next_attempt_at: None,
        created_at: dispatch_now(),
        completed_at: None,
    }
}
