//! «the trip is for Omar Haddad», read with no traveler: a record argument the reading gives
//! no value is read once more, told so, and words that name the record give it. Read again
//! with none, it stays unset and the act asks for it.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{confirmed, script, today, trip, trips, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ActStatus, ArgumentValue, RecordValue};
use turnframe_understand::{UnderstandingInput, WorkflowBrief};

const SET_TRAVELER: &str = "trip.set_traveler";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code, reason = "only its schema is read")]
struct SetTraveler {
    traveler: serde_json::Value,
}

fn input() -> UnderstandingInput {
    let set_traveler = OperationSpec::new(SET_TRAVELER)
        .summary("Choose the traveler the trip is for.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<SetTraveler>()
        .argument("traveler", |a| {
            a.label("traveler").required().record("traveler")
        });
    UnderstandingInput::new("the trip is for Omar Haddad", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "none")]).operation(set_traveler))
        .with_workflow(WorkflowBrief::new("traveler"))
}

/// Reads the message with a first reading giving no traveler and `again` as the second.
async fn read(
    again: serde_json::Value,
) -> (
    Vec<turnframe_core::understanding::UnderstoodAct>,
    Vec<String>,
) {
    // [1]the [2]trip [3]is [4]for [5]Omar [6]Haddad
    let unset = json!({"arguments": {"traveler": {"kind": "not_given"}}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "The traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 6}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_TRAVELER]}))
        .answer("u1/extract", unset)
        .answer(
            "u1/extract.after_not_given",
            json!({"arguments": {"traveler": again}}),
        )
        .answer("u1/verify", confirmed(json!({"traveler": "stated"})));
    let run = understand(script, &input()).await;
    (run.understanding.acts.clone(), run.called())
}

#[tokio::test]
async fn a_record_the_reading_left_unset_is_read_once_more() {
    let (acts, called) = read(json!({"kind": "record", "name": "Omar Haddad",
        "message": "current", "from": 5, "to": 6, "record": "by_name"}))
    .await;
    let traveler = acts
        .first()
        .and_then(|act| act.arguments.get("traveler"))
        .map(|given| given.value.clone());
    assert_eq!(
        traveler,
        Some(ArgumentValue::Record(RecordValue::Named {
            workflow: "traveler".into(),
            named: "Omar Haddad".to_owned(),
        })),
        "{acts:?} {called:?}"
    );
}

#[tokio::test]
async fn read_again_with_no_record_it_stays_unset() {
    let (acts, called) = read(json!({"kind": "not_given"})).await;
    assert!(
        matches!(acts.as_slice(), [act] if matches!(act.status, ActStatus::NeedsValue { .. })),
        "{acts:?}"
    );
    let rereads = called
        .iter()
        .filter(|task| task.starts_with("u1/extract.after_not_given"))
        .count();
    assert_eq!(rereads, 1, "{called:?}");
}
