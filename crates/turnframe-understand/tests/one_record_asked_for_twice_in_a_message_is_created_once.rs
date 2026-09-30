//! Two parts of one message read as creating the same record, with the same values, are one
//! request read twice: «register Nadia Rinaldi, then open a new trip for her» opens one trip,
//! even when the first part is routed to opening it too.
#![allow(clippy::panic)]

mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, script, today, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
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

#[tokio::test]
async fn one_record_asked_for_twice_in_a_message_is_created_once() {
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
    let message = "register Nadia Rinaldi, then open a new trip for her";
    let input = UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips)
        .with_workflow(travelers);
    // [1]register [2]Nadia [3]Rinaldi, [4]then [5]open [6]a [7]new [8]trip [9]for [10]her
    let traveler = json!({"traveler": {"kind": "record", "record": "s1", "name": "",
                          "message": "current", "from": 10, "to": 10}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Registers, then opens a trip.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "traveler"},
                {"kind": "request", "words": {"from": 4, "to": 10}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER, OPEN_TRIP]}))
        .answer("u2/route", json!({"operations": [OPEN_TRIP]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": words(2, 3)}}),
        )
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"traveler": {"kind": "record", "record": "by_name",
                   "name": "Nadia Rinaldi", "message": "current", "from": 2, "to": 3}}}),
        )
        .answer("u2/extract", json!({"arguments": traveler}))
        .answer("u1/verify", confirmed(json!({"full_name": "stated"})))
        .answer("u1.a2/verify", confirmed(json!({"traveler": "stated"})))
        .answer("u2/verify", confirmed(json!({"traveler": "stated"})));
    let run = understand(script, &input).await;

    let acts = &run.understanding.acts;
    let opening = acts
        .iter()
        .filter(|act| act.operation().is_some_and(|op| op.as_str() == OPEN_TRIP))
        .count();
    assert_eq!(opening, 1, "{acts:?}");
}
