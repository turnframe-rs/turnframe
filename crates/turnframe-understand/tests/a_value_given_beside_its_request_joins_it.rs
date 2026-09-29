//! A request and a value given beside it for the same act, read as two units, are one
//! act: «start a new A, B is X» creates one record with X, never two records. A value the
//! request read from the value's own words is the value's reading of them.
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{script, today, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::{ArgumentValue, RecordValue, Understanding, UnderstoodAct};
use turnframe_understand::{RecordBrief, Speaker, UnderstandingInput, WorkflowBrief};

const OPEN: &str = "trip.open";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Create {
    traveler: Option<serde_json::Value>,
}

fn input(travelers: Vec<RecordBrief>) -> UnderstandingInput {
    let trips = WorkflowBrief::new("trip").on_new_case(OPEN).operation(
        OperationSpec::new(OPEN)
            .summary("Start a new trip, for its traveler when named.")
            .target(TargetPolicy::NewCaseOnly)
            .mutating()
            .arguments::<Create>()
            .argument("traveler", |a| a.label("traveler").record("traveler")),
    );
    let mut travelers_brief = WorkflowBrief::new("traveler")
        .on_new_case("traveler.create_draft")
        .operation(
            OperationSpec::new("traveler.create_draft")
                .summary("Register a new traveler.")
                .target(TargetPolicy::NewCaseOnly)
                .mutating(),
        );
    for traveler in travelers {
        travelers_brief = travelers_brief.record(traveler);
    }
    UnderstandingInput::new("start a new trip then, traveler is Ferri", "en-GB", today())
        .with_earlier(Speaker::Assistant, "What would you like to do?")
        .with_workflow(trips)
        .with_workflow(travelers_brief)
}

fn creates(understanding: &Understanding) -> Vec<&UnderstoodAct> {
    understanding
        .acts
        .iter()
        .filter(|act| act.operation().is_some_and(|op| op.as_str() == OPEN))
        .collect()
}

#[tokio::test]
async fn a_value_given_beside_its_request_joins_it() {
    // [1]start [2]a [3]new [4]trip [5]then, [6]traveler [7]is [8]Ferri
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Starts an trip and names its traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"},
                {"kind": "provides_value", "words": {"from": 6, "to": 8}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [OPEN]}))
        .answer("u2/route", json!({"operations": [OPEN]}))
        .answer("u1/extract", json!({"arguments": {"traveler": {"kind": "not_given"}}}))
        .answer(
            "u2/extract",
            json!({"arguments": {"traveler": {"kind": "record", "name": "Ferri",
                "message": "current", "from": 8, "to": 8, "record": "by_name"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Asked for.", "arguments": {}, "overall": "confirmed"}),
        )
        .answer(
            "u2/verify",
            json!({"reason": "Named.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
        );
    let run = understand(script, &input(vec![])).await;

    let creates = creates(&run.understanding);
    assert_eq!(creates.len(), 1, "{creates:?}");
    assert!(
        matches!(
            &creates[0].arguments["traveler"].value,
            ArgumentValue::Record(RecordValue::Named { named, .. }) if named == "Ferri"
        ),
        "{creates:?}"
    );
}

#[tokio::test]
async fn a_value_the_request_read_from_the_values_words_is_the_values() {
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Starts an trip and names its traveler.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"},
                {"kind": "provides_value", "words": {"from": 6, "to": 8}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [OPEN]}))
        .answer("u2/route", json!({"operations": [OPEN]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"traveler": {"kind": "record", "name": "Ferri",
                "message": "current", "from": 8, "to": 8, "record": "by_name"}}}),
        )
        .answer(
            "u2/extract",
            json!({"arguments": {"traveler": {"kind": "record", "name": "Ferri",
                "message": "current", "from": 6, "to": 8, "record": "r1"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Named.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
        )
        .answer(
            "u2/verify",
            json!({"reason": "Named.", "arguments": {"traveler": "stated"}, "overall": "confirmed"}),
        );
    let run = understand(
        script,
        &input(vec![RecordBrief::new("tok-traveler-1", "Ferri", "active")]),
    )
    .await;

    let creates = creates(&run.understanding);
    assert_eq!(creates.len(), 1, "{creates:?}");
    assert!(
        matches!(
            &creates[0].arguments["traveler"].value,
            ArgumentValue::Record(RecordValue::Record { token }) if token.as_str() == "tok-traveler-1"
        ),
        "{creates:?}"
    );
}
