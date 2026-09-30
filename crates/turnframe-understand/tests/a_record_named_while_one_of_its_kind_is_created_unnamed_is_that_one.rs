//! A record named in a message that also creates one record of its kind, left unnamed, is
//! that record, and gives it the name: «open a trip for Nadia Rinaldi, she is not registered
//! yet» registers one traveler, called so, and opens the trip for her.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, script, today, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ArgumentValue, RecordValue};
use turnframe_understand::{UnderstandingInput, WorkflowBrief};

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

/// The acts of «open a trip for Nadia Rinaldi, she is not registered yet».
async fn opened_for_the_one_created() -> Vec<turnframe_core::understanding::UnderstoodAct> {
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
    let message = "open a trip for Nadia Rinaldi, she is not registered yet";
    let input = UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips)
        .with_workflow(travelers);
    // [1]open [2]a [3]trip [4]for [5]Nadia [6]Rinaldi, [7]she [8]is [9]not [10]registered [11]yet
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Opens a trip; registers its traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 6}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 7, "to": 11}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [OPEN_TRIP]}))
        .answer("u2/route", json!({"operations": [REGISTER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"traveler": {"kind": "record", "name": "Nadia Rinaldi",
                "message": "current", "from": 5, "to": 6, "record": "by_name"}}}),
        )
        .answer(
            "u2/extract",
            json!({"arguments": {"full_name": {"kind": "not_given"}}}),
        )
        .answer("u1/verify", confirmed(json!({"traveler": "stated"})))
        .answer(
            "u2/verify",
            json!({"reason": "Asked for.", "arguments": {}, "overall": "confirmed"}),
        );
    understand(script, &input).await.understanding.acts
}

#[tokio::test]
async fn a_record_named_while_one_of_its_kind_is_created_unnamed_is_that_one() {
    let acts = &opened_for_the_one_created().await;
    let registering = acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == REGISTER))
        .unwrap_or_else(|| panic!("{acts:?}"));
    let opening = acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == OPEN_TRIP))
        .unwrap_or_else(|| panic!("{acts:?}"));
    assert_eq!(
        opening.arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::SameTurn {
            act: registering.id
        }),
        "{acts:?}"
    );
    assert_eq!(
        registering.arguments.get("full_name").map(|a| &a.value),
        Some(&ArgumentValue::Json("Nadia Rinaldi".into())),
        "{acts:?}"
    );
}

#[tokio::test]
async fn the_record_is_created_before_the_act_that_waits_on_it() {
    let acts = opened_for_the_one_created().await;
    let operations: Vec<&str> = acts
        .iter()
        .filter_map(|act| act.operation().map(|op| op.as_str()))
        .collect();
    assert_eq!(operations, [REGISTER, OPEN_TRIP], "{acts:?}");
}
