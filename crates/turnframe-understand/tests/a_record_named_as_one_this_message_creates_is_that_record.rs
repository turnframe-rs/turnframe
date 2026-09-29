//! A record named by the name a record this message creates is given is that record, wherever
//! the name was said: names are looked up, never copied, so the words may be another part's.
#![allow(clippy::panic)]
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, trip, trips, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ActId, ArgumentValue, RecordValue, UnitId};
use turnframe_understand::{UnderstandingInput, WorkflowBrief};

const SET_TRAVELER: &str = "trip.set_traveler";
const REGISTER: &str = "traveler.create_draft";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SetTraveler {
    traveler: serde_json::Value,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Register {
    full_name: Option<String>,
}

#[tokio::test]
async fn a_record_named_as_one_this_message_creates_is_that_record() {
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
    let input =
        UnderstandingInput::new("register Ferri and put them on this trip", "en-GB", today())
            .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
            .with_workflow(travelers);
    // [1]register [2]Ferri [3]and [4]put [5]them [6]on [7]this [8]trip
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Registers a traveler and puts it on the trip.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "traveler"},
                {"kind": "request", "words": {"from": 4, "to": 8}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER]}))
        .answer("u2/route", json!({"operations": [SET_TRAVELER]}))
        .answer("u1/extract", json!({"arguments": {"full_name": words(2, 2)}}))
        .answer(
            "u2/extract",
            json!({"arguments": {"traveler": {"kind": "record", "name": "Ferri",
                "message": "current", "from": 2, "to": 2, "record": "by_name"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Asked for.", "arguments": {"full_name": "stated"}, "overall": "confirmed"}),
        )
        .answer(
            "u2/verify",
            json!({"reason": "Named.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
        );
    let run = understand(script, &input).await;

    let acts = &run.understanding.acts;
    let registered = ActId::new(UnitId(1), 1);
    let set = acts
        .iter()
        .find(|act| {
            act.operation()
                .is_some_and(|op| op.as_str() == SET_TRAVELER)
        })
        .unwrap_or_else(|| panic!("{acts:?}"));
    assert_eq!(
        set.arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::SameTurn { act: registered }),
        "{acts:?}"
    );
    assert_eq!(set.depends_on, vec![registered]);
}
