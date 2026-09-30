//! A record that could not be created because the record it names did not exist yet is
//! created with it once the user registers that record, and the registered record takes
//! the name the user gave it before, even when the whole-turn check reads it again.
mod support;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{
    ActId, ActStatus, ActTarget, ArgumentValue, RecordValue, UnderstoodArgument, UnitId,
};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::{Expectation, PendingAct, Settings, UnderstandingInput, WorkflowBrief};

const OPEN_TRIP: &str = "trip.open";
const REGISTER: &str = "traveler.create_draft";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct OpenTrip {
    traveler: Option<serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Register {
    full_name: Option<String>,
}

fn input() -> UnderstandingInput {
    saying("register the new traveler")
}

fn saying(message: &str) -> UnderstandingInput {
    let trips = WorkflowBrief::new("trip").on_new_case(OPEN_TRIP).operation(
        OperationSpec::new(OPEN_TRIP)
            .summary("Start a new trip, for its traveler when named.")
            .target(TargetPolicy::NewCaseOnly)
            .mutating()
            .arguments::<OpenTrip>()
            .argument("traveler", |a| a.label("traveler").record("traveler")),
    );
    let travelers = WorkflowBrief::new("traveler")
        .on_new_case(REGISTER)
        .operation(
            OperationSpec::new(REGISTER)
                .summary("Register a new traveler.")
                .target(TargetPolicy::NewCaseOnly)
                .mutating()
                .arguments::<Register>()
                .argument("full_name", |a| a.label("name").names_the_record()),
        );
    let named = UnderstoodArgument {
        value: ArgumentValue::Record(RecordValue::Named {
            workflow: "traveler".into(),
            named: "Ferri".to_owned(),
        }),
        excerpt: None,
    };
    UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips)
        .with_workflow(travelers)
        .with_expectation(Expectation::Values(PendingAct {
            operation: OPEN_TRIP.into(),
            record: None,
            given: BTreeMap::from([("traveler".to_owned(), named)]),
            missing: vec!["traveler".to_owned()],
        }))
}

fn registers() -> ScriptedTasks {
    // [1]register [2]the [3]new [4]traveler
    script()
        .answer(
            "turn/segment",
            json!({"analysis": "Registers a traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Asked for.", "arguments": {}, "overall": "confirmed"}),
        )
}

#[tokio::test]
async fn a_record_registered_for_a_record_still_to_create_completes_it() {
    let run = understand(registers(), &input()).await;

    let acts = &run.understanding.acts;
    let created = ActId::new(UnitId(1), 1);
    let register = acts.iter().find(|act| act.id == created).unwrap();
    assert_eq!(
        register.arguments["full_name"].value,
        ArgumentValue::Json("Ferri".into()),
        "{acts:?}"
    );
    let trip = acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == OPEN_TRIP))
        .unwrap_or_else(|| panic!("the trip is created with the traveler: {acts:?}"));
    assert_eq!(trip.status, ActStatus::Ready);
    assert_eq!(
        trip.target,
        ActTarget::New {
            workflow: "trip".into()
        }
    );
    assert_eq!(
        trip.arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::SameTurn { act: created })
    );
    assert_eq!(trip.depends_on, vec![created]);
}

#[tokio::test]
async fn the_name_stays_when_the_whole_turn_check_reads_the_record_again() {
    let script = registers()
        .answer(
            "turn/cross_check",
            json!({"findings": [{"kind": "wrong_value", "act": "u1.a1", "argument": "full_name",
                                 "words": {"from": 1, "to": 4}}]}),
        )
        .answer(
            "u1/extract.after_cross_check",
            json!({"arguments": {"full_name": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify.after_cross_check",
            json!({"reason": "Asked for.", "arguments": {}, "overall": "confirmed"}),
        )
        .answer("turn/cross_check.round2", json!({"findings": []}));
    let input = input().with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let acts = &run.understanding.acts;
    let register = acts
        .iter()
        .find(|act| act.id == ActId::new(UnitId(1), 1))
        .unwrap();
    assert_eq!(
        register.arguments.get("full_name").map(|a| &a.value),
        Some(&ArgumentValue::Json("Ferri".into())),
        "{acts:?}"
    );
}

#[tokio::test]
async fn an_answer_routed_to_the_record_still_to_create_creates_it() {
    // [1]Nadia [2]Rinaldi
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "The asked value.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 2}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER, OPEN_TRIP]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": {"kind": "words", "text": "Nadia Rinaldi",
                   "message": "current", "from": 1, "to": 2}}}),
        )
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"traveler": {"kind": "record", "record": "s1", "name": "",
                   "message": "current", "from": 1, "to": 2}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Given.", "arguments": {"full_name": "stated"}, "overall": "confirmed"}),
        )
        .answer(
            "u1.a2/verify",
            json!({"reason": "Given.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
        );
    let run = understand(script, &saying("Nadia Rinaldi")).await;

    let acts = &run.understanding.acts;
    let trip = acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == OPEN_TRIP))
        .unwrap_or_else(|| panic!("the trip is created: {acts:?}"));
    assert_eq!(
        trip.target,
        ActTarget::New {
            workflow: "trip".into()
        },
        "{acts:?}"
    );
}
