//! When one unit about a record fails, every act on that record waits: nothing half-done.
mod support;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, NotUnderstoodReason, UnitId};

#[tokio::test]
async fn a_unit_not_understood_holds_the_acts_on_its_record() {
    // [1]name [2]Lisbon [3]and [4]fly [5]whenever
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two requests.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 4, "to": 5}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u2/route", routed(SET_DATE));
    let run = understand(script, &turn("name Lisbon and fly whenever")).await;

    let understanding = &run.understanding;
    let [act] = understanding.acts.as_slice() else {
        panic!("one act expected: {understanding:?}");
    };
    assert_eq!(act.status, ActStatus::Held { because: UnitId(2) });
    let [failed] = understanding.not_understood.as_slice() else {
        panic!("one unit not understood expected: {understanding:?}");
    };
    assert_eq!(failed.unit, UnitId(2));
    assert!(
        matches!(&failed.reason, NotUnderstoodReason::TaskFailed { task, .. } if task == "extract")
    );
}
