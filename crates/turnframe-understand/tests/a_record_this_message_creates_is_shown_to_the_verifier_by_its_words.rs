//! An act that names a record another act of the message creates is checked against the words
//! that create it: the verifier reads which record, not an act's number.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, trip, trips, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
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
async fn a_record_this_message_creates_is_shown_to_the_verifier_by_its_words() {
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
                .target(TargetPolicy::AllowsNewCase)
                .mutating()
                .arguments::<Register>()
                .argument("full_name", |a| a.label("name").names_the_record()),
        );
    let input = UnderstandingInput::new("Ferri, a new traveler", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
        .with_workflow(travelers);
    // [1]Ferri, [2]a [3]new [4]traveler
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Registers the traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [REGISTER, SET_TRAVELER]}))
        .answer("u1/extract", json!({"arguments": {"full_name": words(1, 1)}}))
        .answer(
            "u1/verify",
            json!({"reason": "Asked for.", "arguments": {"full_name": "stated"}, "overall": "confirmed"}),
        )
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"traveler": {
                "kind": "record", "name": "Ferri", "message": "current",
                "from": 1, "to": 1, "record": "s1"
            }}}),
        )
        .answer(
            "u1.a2/verify",
            json!({"reason": "Named.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
        );
    let run = understand(script, &input).await;

    let shown = run
        .provider
        .calls()
        .iter()
        .find(|call| format!("{:?}", call.metadata).contains("\"u1.a2/verify\""))
        .map(|call| format!("{:?}", call.messages))
        .unwrap_or_default();
    assert!(
        shown.contains("the traveler record this message creates, «Ferri, a new traveler»"),
        "{shown}"
    );
}
