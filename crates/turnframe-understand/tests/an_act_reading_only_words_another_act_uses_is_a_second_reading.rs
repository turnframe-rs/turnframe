//! Words two acts on the same record both read, one complete once it has them and one with
//! nothing else of its own, are the first act's: «add a checked bag, it costs 40 euros», with
//! the price's part misread as another request, adds the bag at 40 euros and nothing else.
#![allow(clippy::panic)]

mod support;

use serde_json::json;
use support::{ADD_EXTRA, SET_NAME, confirmed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;

#[tokio::test]
async fn an_act_reading_only_words_another_act_uses_is_a_second_reading() {
    // [1]add [2]a [3]checked [4]bag, [5]it [6]costs [7]40 [8]euros
    let priced = json!({"arguments": {"description": words(3, 4),
        "amount": {"kind": "money", "message": "current", "from": 7, "to": 8,
                   "amount": "40.00", "currency": "EUR"}}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two requests.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 5, "to": 8}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [ADD_EXTRA]}))
        .answer("u2/route", json!({"operations": [SET_NAME]}))
        .answer("u1/extract", priced.clone())
        .answer("u1/extract.after_elsewhere", priced)
        .answer("u2/extract", json!({"arguments": {"value": words(7, 8)}}))
        .answer("u1/verify", confirmed(json!({"description": "stated"})))
        .answer("u2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("add a checked bag, it costs 40 euros")).await;

    let acts = &run.understanding.acts;
    let [adding] = acts.as_slice() else {
        panic!("the bag alone: {acts:?}");
    };
    assert_eq!(adding.status, ActStatus::Ready, "{acts:?}");
    assert!(adding.arguments.contains_key("amount"), "{acts:?}");
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct Extra {
    description: String,
    payer: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct Payer {
    payer: String,
}

#[tokio::test]
async fn so_are_words_for_a_value_the_first_act_may_go_without() {
    use turnframe_core::operation::OperationSpec;
    use turnframe_core::plan::TargetPolicy;
    let spec = |key: &str| {
        OperationSpec::new(key)
            .summary("A change to the trip.")
            .target(TargetPolicy::RequiresExistingCase)
            .mutating()
    };
    let workflow = turnframe_understand::WorkflowBrief::new("trip")
        .operation(spec("trip.add_extra").arguments::<Extra>())
        .operation(spec("trip.assign_payer").arguments::<Payer>())
        .record(support::trip(1, "Bianchi").offering(["trip.add_extra", "trip.assign_payer"]));
    let input = turnframe_understand::UnderstandingInput::new(
        "add a checked bag, the airline pays",
        "en-GB",
        support::today(),
    )
    .with_workflow(workflow);
    // [1]add [2]a [3]checked [4]bag, [5]the [6]airline [7]pays
    let extra = json!({"arguments": {"description": words(3, 4),
        "payer": {"kind": "words", "text": "airline", "message": "current", "from": 6, "to": 6}}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two requests.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 5, "to": 7}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": ["trip.add_extra"]}))
        .answer("u2/route", json!({"operations": ["trip.assign_payer"]}))
        .answer("u1/extract", extra.clone())
        .answer("u1/extract.after_elsewhere", extra)
        .answer(
            "u2/extract",
            json!({"arguments": {"payer": {"kind": "words", "text": "airline",
                   "message": "current", "from": 6, "to": 6}}}),
        )
        .answer("u1/verify", confirmed(json!({"description": "stated"})))
        .answer("u2/verify", confirmed(json!({"payer": "stated"})));
    let run = understand(script, &input).await;

    let acts = &run.understanding.acts;
    let [adding] = acts.as_slice() else {
        panic!("the extra alone: {acts:?}");
    };
    assert!(adding.arguments.contains_key("payer"), "{acts:?}");
}
