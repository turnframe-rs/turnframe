//! An act the turn's keep-unchanged constraint protects is held: the respects task judges
//! it changes what the words keep, and it runs nothing; an act it leaves alone runs.
mod support;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, NotUnderstoodReason};

#[tokio::test]
async fn an_act_changing_what_a_constraint_keeps_is_held() {
    // [1]name [2]Lisbon, [3]date [4]tomorrow, [5]but [6]leave [7]the [8]date
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two sets and a constraint.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 3, "to": 4}, "workflow": "trip"},
                {"kind": "constraint", "words": {"from": 5, "to": 8}, "constraint": "keep_unchanged"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(SET_DATE))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer(
            "u2/extract",
            json!({"arguments": {"date": {"kind": "date", "message": "current", "from": 4, "to": 4,
                "date": {"kind": "relative", "unit": "day", "amount": 1}}}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u2/verify", confirmed(json!({"date": "stated"})))
        .answer(
            "u1/respects",
            json!({"reason": "Names the trip.", "changes": false}),
        )
        .answer(
            "u2/respects",
            json!({"reason": "Changes the date.", "changes": true}),
        );
    let run = understand(
        script,
        &turn("name Lisbon, date tomorrow, but leave the date"),
    )
    .await;

    assert_eq!(
        run.understanding.acts.len(),
        1,
        "{:?} {:?}",
        run.understanding,
        run.called()
    );
    assert_eq!(
        run.understanding.acts[0].status,
        ActStatus::Ready,
        "the act the constraint leaves alone runs"
    );
    assert!(
        run.understanding
            .not_understood
            .iter()
            .any(|item| matches!(item.reason, NotUnderstoodReason::KeptUnchanged { .. })),
        "{:?}",
        run.understanding.not_understood
    );
}
