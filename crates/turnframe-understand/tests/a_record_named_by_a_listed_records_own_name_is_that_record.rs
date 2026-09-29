//! A record named by the whole name of one record in view is that record, even when the
//! reading called it a record to look up: two acts reading it apart are then one act.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, one_request, script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ArgumentValue, RecordValue};
use turnframe_understand::{RecordBrief, UnderstandingInput, WorkflowBrief};

const SET_TRAVELER: &str = "trip.set_traveler";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SetTraveler {
    traveler: serde_json::Value,
}

#[tokio::test]
async fn a_record_named_by_a_listed_records_own_name_is_that_record() {
    let set_traveler = OperationSpec::new(SET_TRAVELER)
        .summary("Choose the traveler the trip is for.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<SetTraveler>()
        .argument("traveler", |a| {
            a.label("traveler").required().record("traveler")
        });
    let travelers = WorkflowBrief::new("traveler").record(RecordBrief::new(
        "tok-trav-1",
        "Luca Ferri",
        "active",
    ));
    // [1]the [2]traveler [3]is [4]Luca [5]Ferri
    let input = UnderstandingInput::new("the traveler is Luca Ferri", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
        .with_workflow(travelers);
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", json!({"operations": [SET_TRAVELER]}))
        .answer("u1/locate", json!({"record": "r1", "named": null}))
        .answer(
            "u1/extract",
            json!({"arguments": {"traveler": {"kind": "record", "name": "luca ferri",
                "message": "current", "from": 4, "to": 5, "record": "by_name"}}}),
        )
        .answer("u1/verify", confirmed(json!({"traveler": "stated"})));
    let run = understand(script, &input).await;

    let [act] = run.understanding.acts.as_slice() else {
        panic!("{:?} {:?}", run.understanding, run.called());
    };
    assert_eq!(
        act.arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::Record {
            token: "tok-trav-1".into()
        }),
        "{act:?}"
    );
}
