//! An act whose every value points into another part's words, even after it is read again,
//! is that part's act read twice: it runs nothing and reports nothing, though what it lacks
//! is optional.
#![allow(clippy::unwrap_used)]
mod support;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use support::{SET_NAME, confirmed, routed, script, today, trip, trips, understand, words};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_understand::UnderstandingInput;

const NOTE: &str = "trip.note";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct Note {
    text: Option<String>,
}

async fn read_with_repair(
    repair: serde_json::Value,
) -> turnframe_core::understanding::Understanding {
    let note = OperationSpec::new(NOTE)
        .summary("Add a note.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating()
        .arguments::<Note>();
    let workflow = trips(vec![])
        .operation(note)
        .record(trip(1, "Bianchi").offering([SET_NAME, NOTE]));
    let input =
        UnderstandingInput::new("name Lisbon, and so on", "en-GB", today()).with_workflow(workflow);
    // [1]name [2]Lisbon, [3]and [4]so [5]on
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two things.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 3, "to": 5}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(NOTE))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u2/extract", json!({"arguments": {"text": words(2, 2)}}))
        .answer("u2/extract.after_elsewhere", repair)
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input).await;
    assert!(!run.was_called("u2/verify"), "{:?}", run.called());
    run.understanding
}

#[tokio::test]
async fn an_act_whose_every_value_is_another_parts_asks_nothing() {
    let understanding = read_with_repair(json!({"arguments": {"text": words(2, 2)}})).await;
    assert_eq!(understanding.acts.len(), 1, "{understanding:?}");
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
}

#[tokio::test]
async fn one_read_again_to_nothing_asks_nothing_either() {
    let understanding =
        read_with_repair(json!({"arguments": {"text": {"kind": "not_given"}}})).await;
    assert_eq!(understanding.acts.len(), 1, "{understanding:?}");
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
}
