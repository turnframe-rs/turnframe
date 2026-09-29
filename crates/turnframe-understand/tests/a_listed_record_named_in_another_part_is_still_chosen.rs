//! A record chosen from those listed is that record, whatever part's words name it: an act
//! choosing it from another part's words keeps it, and that part, read as creating the
//! same record, creates none.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, script, today, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ArgumentValue, RecordValue};
use turnframe_understand::{RecordBrief, UnderstandingInput, WorkflowBrief};

const OPEN: &str = "trip.open";
const REGISTER: &str = "traveler.create_draft";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Open {
    traveler: Option<serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Register {
    full_name: Option<String>,
}

#[tokio::test]
async fn a_listed_record_named_in_another_part_is_still_chosen() {
    let trips = WorkflowBrief::new("trip").on_new_case(OPEN).operation(
        OperationSpec::new(OPEN)
            .summary("Open a disruption case, for its traveler when named.")
            .target(TargetPolicy::NewCaseOnly)
            .mutating()
            .arguments::<Open>()
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
        )
        .record(RecordBrief::new("tok-traveler-1", "Luca Ferri", "active"));
    let message = "open a new trip then, the traveler is Luca Ferri";
    let input = UnderstandingInput::new(message, "en-GB", today())
        .with_workflow(trips)
        .with_workflow(travelers);
    // [1]open [2]a [3]new [4]trip [5]then, [6]the [7]traveler [8]is [9]Luca [10]Ferri
    let listed = json!({"arguments": {"traveler": {"kind": "record", "name": "Luca Ferri",
        "message": "current", "from": 9, "to": 10, "record": "r1"}}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Opens a trip, then names a traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 6, "to": 10}, "workflow": "traveler"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [OPEN]}))
        .answer("u2/route", json!({"operations": [REGISTER]}))
        .answer("u1/extract", listed.clone())
        .answer("u1/extract.after_elsewhere", listed)
        .answer("u1/verify", confirmed(json!({"traveler": "stated"})))
        .answer(
            "u2/extract",
            json!({"arguments": {"full_name": words(9, 10)}}),
        )
        .answer("u2/verify", confirmed(json!({"full_name": "stated"})));
    let run = understand(script, &input).await;

    let operations: Vec<String> = run
        .understanding
        .acts
        .iter()
        .filter_map(|act| act.operation().map(ToString::to_string))
        .collect();
    assert_eq!(
        operations,
        [OPEN],
        "{:?} {:?}",
        run.understanding,
        run.called()
    );
    assert_eq!(
        run.understanding.acts[0].arguments["traveler"].value,
        ArgumentValue::Record(RecordValue::Record {
            token: "tok-traveler-1".into()
        })
    );
}
