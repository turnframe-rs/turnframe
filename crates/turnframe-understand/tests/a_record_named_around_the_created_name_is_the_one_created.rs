//! A record named by words that hold the name a record this message creates was given is
//! that record, even when the words run past the name: the act waits for the creation.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, script, today, trip, trips, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ArgumentValue, RecordValue};
use turnframe_understand::{UnderstandingInput, WorkflowBrief};

const REGISTER: &str = "traveler.create_draft";
const SET_TRAVELER: &str = "trip.set_traveler";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Register {
    full_name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SetTraveler {
    traveler: serde_json::Value,
}

#[tokio::test]
async fn a_record_named_around_the_created_name_is_the_one_created() {
    let set_traveler = OperationSpec::new(SET_TRAVELER)
        .summary("Choose the traveler the trip is for.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<SetTraveler>()
        .argument("traveler", |a| {
            a.label("traveler").required().record("traveler")
        });
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
    let message = "register Nadia Rinaldi, email nadia@rinaldi.example, and put her on this trip";
    let input = UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
        .with_workflow(travelers);
    // [1]register [2]Nadia [3]Rinaldi, [4]email [5]nadia@rinaldi.example, [6]and [7]put
    // [8]her [9]on [10]this [11]trip
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Registers a traveler and puts her on the trip.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 11}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER, SET_TRAVELER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": words(2, 3)}}),
        )
        .answer("u1.a2/locate", json!({"record": "r1", "named": null}))
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"traveler": {"kind": "record",
                "name": "Nadia Rinaldi, email nadia@rinaldi.example", "message": "current",
                "from": 2, "to": 5, "record": "by_name"}}}),
        )
        .answer("u1/verify", confirmed(json!({"full_name": "stated"})))
        .answer("u1.a2/verify", confirmed(json!({"traveler": "stated"})));
    let run = understand(script, &input).await;

    let placing = run
        .understanding
        .acts
        .iter()
        .find(|act| {
            act.operation()
                .is_some_and(|op| op.as_str() == SET_TRAVELER)
        })
        .unwrap_or_else(|| panic!("{:?} {:?}", run.understanding, run.called()));
    let registering = run
        .understanding
        .acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == REGISTER))
        .unwrap_or_else(|| panic!("{:?}", run.understanding));
    assert_eq!(
        placing.arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::SameTurn {
            act: registering.id
        }),
        "{:?}",
        run.understanding
    );
}
