//! A part asking for two things, routed in the other order, still runs them in the order the
//! message says them: each act stands where its values stand. Acts one waits on keep the
//! order routing gave, and so does a part with a value pointed at all of its words, which
//! says nothing of where that value stands.
mod support;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, one_request, script, turn, understand, words};
use turnframe_core::understanding::ActAction;

#[tokio::test]
async fn acts_asked_in_one_part_keep_the_order_the_message_says_them() {
    let script = script()
        .answer("turn/segment", one_request(1, 9))
        .answer("u1/route", json!({"operations": [SET_NAME, SET_DATE]}))
        .answer("u1/extract", json!({"arguments": {"value": words(8, 9)}}))
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"date": {
                "kind": "date", "message": "current", "from": 3, "to": 4,
                "date": {"kind": "absolute", "day": 12, "month": 10, "year": null}
            }}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u1.a2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(
        script,
        &turn("fly on 12 October and call it Lisbon offsite"),
    )
    .await;

    let operations: Vec<String> = run
        .understanding
        .acts
        .iter()
        .filter_map(|act| match &act.action {
            ActAction::Apply { operation } => Some(operation.to_string()),
            ActAction::Start { .. } => None,
        })
        .collect();
    assert_eq!(operations, [SET_DATE, SET_NAME], "{:?}", run.understanding);
}

#[tokio::test]
async fn a_value_pointed_at_the_whole_part_leaves_the_order_routing_gave() {
    // [1]name: [2]offsite [3]Lisbon, [4]travel [5]date [6]30/11/2026
    let script = script()
        .answer("turn/segment", one_request(1, 6))
        .answer("u1/route", json!({"operations": [SET_NAME, SET_DATE]}))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 3)}}))
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"date": {
                "kind": "date", "message": "current", "from": 1, "to": 6,
                "date": {"kind": "absolute", "day": 30, "month": 11, "year": 2026}
            }}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u1.a2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(
        script,
        &turn("name: offsite Lisbon, travel date 30/11/2026"),
    )
    .await;

    let operations: Vec<String> = run
        .understanding
        .acts
        .iter()
        .filter_map(|act| match &act.action {
            ActAction::Apply { operation } => Some(operation.to_string()),
            ActAction::Start { .. } => None,
        })
        .collect();
    assert_eq!(operations, [SET_NAME, SET_DATE], "{:?}", run.understanding);
}
