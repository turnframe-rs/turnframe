//! What a configured prompt source changes about a turn, and what it leaves alone.
//!
//! The claim under test is the one an audit rests on: **the reference reaches the
//! replay record**, on the turn and on the task call that used the prompt, and the
//! text the model actually saw is the text the reference names.
//!
//! Understanding and narration each run in a turn of their own: with the task
//! provider in the pool, the harness routes narration to it first.
//!
//! The source here is a few lines of in-test code rather than anything from
//! `turnframe-prompt`: the runtime is written against the trait in `turnframe-core`
//! and depends on no implementation of it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use support::{Harness, narrating, narration, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::prompt::{LoadedPrompt, PromptError, PromptName, PromptSelector, PromptSource};
use turnframe_core::replay::ReplayRecord;
use turnframe_core::response::AssistantTurn;
use turnframe_tasks::TASK_LABEL;
use turnframe_tasks::instructions::BUILT_IN_VERSION;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

/// The user's words, which the scripted tasks point into.
const TURN_TEXT: &str = "change the name to Lisbon";

const SEGMENT_TEXT: &str = "Split the message the way this repository says to.";
const NARRATE_TEXT: &str = "Acknowledge in one line. Claim nothing.";

/// A source that answers from a small table, counts what was asked of it, and
/// can be told to fail.
#[derive(Debug)]
struct TableSource {
    fail: bool,
    asked: AtomicUsize,
}

impl TableSource {
    fn answering() -> Arc<Self> {
        Arc::new(Self {
            fail: false,
            asked: AtomicUsize::new(0),
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            fail: true,
            asked: AtomicUsize::new(0),
        })
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl PromptSource for TableSource {
    async fn load(
        &self,
        name: &PromptName,
        _selector: &PromptSelector,
    ) -> Result<LoadedPrompt, PromptError> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err(PromptError::Transport { code: "connect" });
        }
        let text = match name.as_str() {
            "understand.segment" => SEGMENT_TEXT,
            "narrate.acknowledge" => NARRATE_TEXT,
            _ => {
                return Err(PromptError::NotFound { name: name.clone() });
            }
        };
        Ok(LoadedPrompt::new(name.clone(), "r7", text))
    }

    fn describe(&self) -> &'static str {
        "table"
    }
}

fn turn_id() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(41))
}

/// Every task call understanding makes of [`TURN_TEXT`], answered.
fn understanding_tasks() -> Arc<ScriptedTasks> {
    let pointer = |from: usize, to: usize| json!({"from": from, "to": to});
    let unit = json!({"kind": "request", "words": pointer(1, 5), "workflow": "trip"});
    let value =
        json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    Arc::new(
        ScriptedTasks::new("tasks", "small")
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

/// A turn understood by the real task pipeline, which commits and does not narrate.
async fn understood(source: Option<Arc<TableSource>>, tasks: Arc<ScriptedTasks>) -> ReplayRecord {
    let mut builder = Harness::builder()
        .trip("trip-1", "Trip 2026-1", 3, incomplete_case())
        .understanding_tasks(tasks)
        .without_narration();
    if let Some(source) = source {
        builder = builder.prompts(source as Arc<dyn PromptSource>);
    }
    let harness = builder.build().await;
    harness
        .handle(harness.turn(turn_id(), TURN_TEXT))
        .await
        .expect("a prompt source is a convenience, not a dependency");
    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set"],
        "the turn was understood and committed"
    );
    harness.replay(turn_id()).await
}

/// The same turn, understood as scripted, committed and narrated.
async fn narrated(source: Option<Arc<TableSource>>) -> (ReplayRecord, AssistantTurn) {
    let understanding = UnderstandingBuilder::of(TURN_TEXT)
        .apply(
            operations::SET_NAME,
            token_for(turn_id(), "trip", "trip-1"),
            json!({"value": "Lisbon"}),
            TURN_TEXT,
        )
        .build()
        .unwrap();
    let mut builder = Harness::builder()
        .trip("trip-1", "Trip 2026-1", 3, incomplete_case())
        .understands(understanding)
        .provider(narrating().build_shared());
    if let Some(source) = source {
        builder = builder.prompts(source as Arc<dyn PromptSource>);
    }
    let harness = builder.build().await;
    let turn = harness
        .handle(harness.turn(turn_id(), TURN_TEXT))
        .await
        .expect("a prompt source is a convenience, not a dependency");
    (harness.replay(turn_id()).await, turn)
}

/// Whether every task on the record says it ran on the text compiled into the library.
fn every_task_ran_built_in(record: &ReplayRecord) -> bool {
    !record.tasks.is_empty()
        && record.tasks.iter().all(|task| {
            task.prompt_ref
                .as_ref()
                .is_some_and(|reference| reference.version.as_str() == BUILT_IN_VERSION)
        })
}

#[tokio::test]
async fn the_prompt_reference_reaches_the_replay_record_and_the_task_that_used_it() {
    let source = TableSource::answering();
    let (record, turn) = narrated(Some(Arc::clone(&source))).await;
    assert!(!narration(&turn).is_empty(), "the narrator ran");

    // The turn names every prompt it ran on, by the digest of the text the source
    // returned, so the record can be checked rather than believed.
    let names: Vec<&str> = record
        .prompt_refs
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert_eq!(names, vec!["narrate.acknowledge"]);
    assert_eq!(record.prompt_refs[0].version.as_str(), "r7");
    assert!(
        record.prompt_refs[0].matches(NARRATE_TEXT),
        "{} does not name its text",
        record.prompt_refs[0]
    );

    // The task that wrote the acknowledgement says which prompt produced it.
    let acknowledged = record
        .tasks
        .iter()
        .find(|task| task.task_id == "reply/acknowledge")
        .expect("the acknowledgement is on the record");
    let reference = acknowledged
        .prompt_ref
        .as_ref()
        .expect("a task cites its prompt");
    assert_eq!(reference.name.as_str(), "narrate.acknowledge");
    assert_eq!(reference.version.as_str(), "r7");
    assert!(source.asked() >= 1, "narration asked the source");
}

#[tokio::test]
async fn the_prompt_reference_reaches_the_task_that_used_it_and_the_turn() {
    let source = TableSource::answering();
    let tasks = understanding_tasks();
    let record = understood(Some(Arc::clone(&source)), Arc::clone(&tasks)).await;

    // Each task call says which prompt it ran under, sourced or built in.
    let segment = record
        .tasks
        .iter()
        .find(|task| task.task_id == "turn/segment")
        .expect("the segmentation is on the record");
    let reference = segment
        .prompt_ref
        .as_ref()
        .expect("a task cites its prompt");
    assert_eq!(reference.name.as_str(), "understand.segment");
    assert_eq!(reference.version.as_str(), "r7");
    assert!(
        reference.matches(SEGMENT_TEXT),
        "{reference} does not name its text"
    );
    for task in record
        .tasks
        .iter()
        .filter(|task| task.task_id != "turn/segment")
    {
        let reference = task.prompt_ref.as_ref().expect("a task cites its prompt");
        assert_eq!(
            reference.version.as_str(),
            BUILT_IN_VERSION,
            "{} ran on the built-in text the source did not replace",
            task.task_id
        );
    }
    let sent = tasks
        .calls()
        .into_iter()
        .find(|call| call.metadata.get(TASK_LABEL) == Some("turn/segment"))
        .expect("the segmentation was sent");
    assert_eq!(
        sent.system.as_deref(),
        Some(SEGMENT_TEXT),
        "the model saw the text the reference names"
    );
    assert!(source.asked() >= 1, "understanding asked the source");

    // The turn names every prompt the source supplied, understanding's included.
    let names: Vec<&str> = record
        .prompt_refs
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert_eq!(names, vec!["understand.segment"]);
}

#[tokio::test]
async fn with_no_source_configured_the_record_carries_no_prompt_reference() {
    let (narrated, _) = narrated(None).await;
    assert!(
        narrated.prompt_refs.is_empty(),
        "the default configuration reaches no prompt source"
    );
    assert!(
        every_task_ran_built_in(&narrated),
        "every narration task cites the built-in text it ran on"
    );

    let understood = understood(None, understanding_tasks()).await;
    assert!(understood.prompt_refs.is_empty());
    assert!(
        every_task_ran_built_in(&understood),
        "every task cites the built-in text it ran on"
    );
}

#[tokio::test]
async fn a_source_that_cannot_answer_leaves_the_turn_standing_and_the_record_honest() {
    let source = TableSource::failing();
    let (narrated, turn) = narrated(Some(Arc::clone(&source))).await;
    assert!(!turn.blocks.is_empty());
    assert!(source.asked() >= 1, "the source was tried");
    assert!(
        narrated.prompt_refs.is_empty(),
        "no reference is the audit signal that the built-in text was used"
    );
    assert!(every_task_ran_built_in(&narrated));

    let understood = understood(Some(source), understanding_tasks()).await;
    assert!(understood.prompt_refs.is_empty());
    assert!(
        every_task_ran_built_in(&understood),
        "every task says it ran on the built-in text"
    );
}
