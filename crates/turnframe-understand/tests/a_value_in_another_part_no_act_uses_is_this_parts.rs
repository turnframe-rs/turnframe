//! A value pointed at in another part's words is that part's only when an act of it uses
//! them: «add a checked bag, it costs 40 euros, and call the trip Porto» adds the bag at
//! 40 euros, though segmentation put the price in the part that names the trip.
mod support;

use serde_json::json;
use support::{ADD_EXTRA, SET_NAME, confirmed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;

#[tokio::test]
async fn a_value_in_another_part_no_act_uses_is_this_parts() {
    // [1]add [2]a [3]checked [4]bag, [5]it [6]costs [7]40 [8]euros, [9]and [10]call [11]the
    // [12]trip [13]Porto
    let priced = json!({"arguments": {"description": words(3, 4),
        "amount": {"kind": "money", "message": "current", "from": 7, "to": 8,
                   "amount": "40.00", "currency": "EUR"}}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two requests.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 5, "to": 13}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [ADD_EXTRA]}))
        .answer("u2/route", json!({"operations": [SET_NAME]}))
        .answer("u1/extract", priced.clone())
        .answer("u1/extract.after_elsewhere", priced)
        .answer("u2/extract", json!({"arguments": {"value": words(13, 13)}}))
        .answer("u1/verify", confirmed(json!({"description": "stated"})))
        .answer("u2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(
        script,
        &turn("add a checked bag, it costs 40 euros, and call the trip Porto"),
    )
    .await;

    let acts = &run.understanding.acts;
    let adding = acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == ADD_EXTRA))
        .expect("the bag");
    assert_eq!(adding.status, ActStatus::Ready, "{acts:?}");
    assert!(adding.arguments.contains_key("amount"), "{acts:?}");
}

#[tokio::test]
async fn so_it_is_when_a_second_reading_gives_it_up() {
    // [1]add [2]a [3]checked [4]bag, [5]it [6]costs [7]40 [8]euros, [9]and [10]call [11]the
    // [12]trip [13]Porto
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two requests.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 5, "to": 13}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [ADD_EXTRA]}))
        .answer("u2/route", json!({"operations": [SET_NAME]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"description": words(3, 4),
                "amount": {"kind": "money", "message": "current", "from": 7, "to": 8,
                           "amount": "40.00", "currency": "EUR"}}}),
        )
        .answer(
            "u1/extract.after_elsewhere",
            json!({"arguments": {"description": words(3, 4), "amount": {"kind": "not_given"}}}),
        )
        .answer("u2/extract", json!({"arguments": {"value": words(13, 13)}}))
        .answer("u1/verify", confirmed(json!({"description": "stated"})))
        .answer("u2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(
        script,
        &turn("add a checked bag, it costs 40 euros, and call the trip Porto"),
    )
    .await;

    let adding = run
        .understanding
        .acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == ADD_EXTRA))
        .expect("the bag");
    assert_eq!(
        adding.status,
        ActStatus::Ready,
        "{:?}",
        run.understanding.acts
    );
}
