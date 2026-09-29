//! A small trip domain, a scripted provider and a runner for the pipeline's tests.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use turnframe_core::flow::StateField;
use turnframe_core::locale::Locale;
use turnframe_core::operation::{DateDirection, Money, OperationSpec};
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::Understanding;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_tasks::{Budget, TaskEngine, TaskProfiles, TaskScope};
use turnframe_understand::{
    ActChecker, NoChecks, RecordBrief, RecordedSteps, Understander, UnderstandingInput,
    WorkflowBrief,
};

pub const TODAY: (i32, u32, u32) = (2026, 9, 26);

pub const SET_NAME: &str = "trip.set_name";
pub const SET_DATE: &str = "trip.set_travel_date";
pub const ADD_EXTRA: &str = "trip.add_extra";
pub const OPEN: &str = "trip.open";
pub const REBOOK: &str = "trip.request_rebooking";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetName {
    /// What the traveler calls the trip.
    pub value: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetDate {
    /// The day the traveler would rather fly.
    pub date: NaiveDate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddExtra {
    /// What the extra is.
    pub description: String,
    /// Its amount.
    pub amount: Money,
}

pub fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(TODAY.0, TODAY.1, TODAY.2).unwrap()
}

fn operations() -> Vec<OperationSpec> {
    vec![
        OperationSpec::new(SET_NAME)
            .summary("Name the trip.")
            .target(TargetPolicy::RequiresExistingCase)
            .mutating()
            .arguments::<SetName>()
            .argument("value", |a| {
                a.label("trip name")
                    .label_in("it-IT", "nome del viaggio")
                    .required()
            })
            .example(
                "the trip is for the Lisbon offsite",
                json!({"value": "Lisbon offsite"}),
            )
            .example_not_given("the trip name needs changing", ["value"]),
        OperationSpec::new(SET_DATE)
            .summary("Set the day the traveler would rather fly.")
            .target(TargetPolicy::RequiresExistingCase)
            .mutating()
            .arguments::<SetDate>()
            .argument("date", |a| {
                a.label("travel date").date_direction(DateDirection::Future)
            }),
        OperationSpec::new(ADD_EXTRA)
            .summary("Add an extra to the trip.")
            .target(TargetPolicy::RequiresExistingCase)
            .mutating()
            .arguments::<AddExtra>()
            .argument("amount", |a| a.money()),
        OperationSpec::new(OPEN)
            .summary("Open a disruption case.")
            .target(TargetPolicy::NewCaseOnly)
            .mutating(),
        OperationSpec::new(REBOOK)
            .summary("Rebook the quoted flight.")
            .target(TargetPolicy::RequiresCatalogedCase)
            .mutating(),
    ]
}

/// The trip workflow with `records` in view, each offering every operation but opening.
pub fn trips(records: Vec<RecordBrief>) -> WorkflowBrief {
    let mut workflow = WorkflowBrief::new("trip")
        .summary("Open, fill in and rebook disruption cases.")
        .on_new_case(OPEN);
    for spec in operations() {
        spec.validate().unwrap();
        workflow = workflow.operation(spec);
    }
    for record in records {
        workflow = workflow.record(record.offering([SET_NAME, SET_DATE, ADD_EXTRA, REBOOK]));
    }
    workflow
}

pub fn trip(number: u32, traveler: &str) -> RecordBrief {
    RecordBrief::new(
        format!("tok-trip-{number}"),
        format!("Trip {number}"),
        "collecting",
    )
    .field(StateField::new("traveler", json!(traveler)).identifying())
    .field(StateField::new("name", Value::Null))
}

/// A turn saying `message` today, with one trip in view.
pub fn turn(message: &str) -> UnderstandingInput {
    UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi")]))
}

/// A script that already answers coverage with nothing missed.
pub fn script() -> ScriptedTasks {
    ScriptedTasks::new("scripted", "small").answer("turn/coverage", json!({"missed": []}))
}

pub struct Run {
    pub understanding: Understanding,
    pub provider: Arc<ScriptedTasks>,
    pub steps: RecordedSteps,
    pub scope: TaskScope,
}

impl Run {
    pub fn called(&self) -> Vec<String> {
        self.provider.called()
    }

    pub fn was_called(&self, task: &str) -> bool {
        self.called().iter().any(|called| called == task)
    }
}

pub async fn understand(script: ScriptedTasks, input: &UnderstandingInput) -> Run {
    understand_checked(script, input, &NoChecks).await
}

pub async fn understand_checked(
    script: ScriptedTasks,
    input: &UnderstandingInput,
    checker: &dyn ActChecker,
) -> Run {
    run_in(script, input, checker, Budget::understanding()).await
}

/// Understands under `budget`, for what a turn does when its calls run out.
pub async fn understand_in(
    script: ScriptedTasks,
    input: &UnderstandingInput,
    budget: Budget,
) -> Run {
    run_in(script, input, &NoChecks, budget).await
}

/// Understands with the engine's task profiles changed, for votes and repairs.
pub async fn understand_profiled(
    script: ScriptedTasks,
    input: &UnderstandingInput,
    profiles: TaskProfiles,
) -> Run {
    run_profiled(script, input, &NoChecks, Budget::understanding(), profiles).await
}

async fn run_in(
    script: ScriptedTasks,
    input: &UnderstandingInput,
    checker: &dyn ActChecker,
    budget: Budget,
) -> Run {
    run_profiled(script, input, checker, budget, TaskProfiles::new()).await
}

async fn run_profiled(
    script: ScriptedTasks,
    input: &UnderstandingInput,
    checker: &dyn ActChecker,
    budget: Budget,
    profiles: TaskProfiles,
) -> Run {
    let provider = Arc::new(script);
    let engine = TaskEngine::builder(provider.router())
        .profiles(profiles)
        .build();
    let understander = Understander::new(engine);
    let scope = TaskScope::new(budget, Locale::from("en-GB"));
    let steps = RecordedSteps::new();
    let understanding = understander
        .understand_checked(&scope, input, &steps, checker)
        .await;
    Run {
        understanding,
        provider,
        steps,
        scope,
    }
}

/// A segmentation of one request over words `from` to `to`.
pub fn one_request(from: usize, to: usize) -> Value {
    json!({
        "analysis": "One request.",
        "units": [{"kind": "request", "words": {"from": from, "to": to}, "workflow": "trip"}]
    })
}

pub fn routed(operation: &str) -> Value {
    json!({ "operations": [operation] })
}

pub fn confirmed(arguments: Value) -> Value {
    json!({ "reason": "The user said so.", "arguments": arguments, "overall": "confirmed" })
}

pub fn words(from: usize, to: usize) -> Value {
    json!({ "kind": "words", "message": "current", "from": from, "to": to, "text": "" })
}
