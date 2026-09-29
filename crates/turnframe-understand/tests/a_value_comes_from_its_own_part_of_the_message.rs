//! An act takes its values from its own part of the message, or from an earlier message:
//! a value in another part belongs to that part's act, and an act left waiting for what
//! another act of the turn already does is dropped, not asked about.
mod support;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;

#[tokio::test]
async fn a_value_comes_from_its_own_part_of_the_message() {
    // [1]fly [2]tomorrow, [3]name [4]Lisbon
    let tomorrow = json!({"kind": "date", "message": "current", "from": 2, "to": 2,
                          "date": {"kind": "relative", "unit": "day", "amount": 1}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two things.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 3, "to": 4}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_DATE, SET_NAME]}))
        .answer("u2/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"date": tomorrow}}))
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"value": words(4, 4)}}),
        )
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"value": {"kind": "not_given"}}}),
        )
        .answer("u2/extract", json!({"arguments": {"value": words(4, 4)}}))
        .answer("u1/verify", confirmed(json!({"date": "stated"})))
        .answer("u2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("fly tomorrow, name Lisbon")).await;

    let acts = &run.understanding.acts;
    assert_eq!(acts.len(), 2, "{acts:?}");
    assert!(
        acts.iter().all(|act| act.status == ActStatus::Ready),
        "{acts:?}"
    );
    assert!(
        !run.called().iter().any(|call| call.contains("repair")),
        "a value in another part is simply not this act's: {:?}",
        run.called()
    );
}
