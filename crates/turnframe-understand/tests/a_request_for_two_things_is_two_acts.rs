//! One unit asking for two things, as segmentation left it, routes to two operations
//! and becomes two acts, each filled and checked on its own.
mod support;

use serde_json::{Value, json};
use support::{SET_DATE, SET_NAME, confirmed, one_request, script, turn, understand};
use turnframe_core::understanding::{ActAction, ActId, ActStatus, ArgumentValue, UnitId};

// [1]set [2]the [3]name [4]to [5]Lisbon [6]offsite [7]and [8]the [9]travel [10]date [11]to
// [12]30 [13]November [14]2026
const MESSAGE: &str = "set the name to Lisbon offsite and the travel date to 30 November 2026";

#[tokio::test]
async fn a_request_for_two_things_is_two_acts() {
    let date = json!({"kind": "absolute", "year": 2026, "month": 11, "day": 30});
    let script = script()
        .answer("turn/segment", one_request(1, 14))
        .answer("u1/route", json!({"operations": [SET_NAME, SET_DATE]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {
                "kind": "words", "text": "Lisbon offsite", "message": "current", "from": 1, "to": 14
            }}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "u1.a2/extract",
            json!({"arguments": {"date": {
                "kind": "date", "message": "current", "from": 12, "to": 14, "date": date
            }}}),
        )
        .answer("u1.a2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn(MESSAGE)).await;

    let acts = &run.understanding.acts;
    let ids: Vec<ActId> = acts.iter().map(|act| act.id).collect();
    assert_eq!(
        ids,
        vec![ActId::new(UnitId(1), 1), ActId::new(UnitId(1), 2)]
    );
    assert!(acts.iter().all(|act| act.status == ActStatus::Ready));
    assert_eq!(
        acts[0].action,
        ActAction::Apply {
            operation: SET_NAME.into()
        }
    );
    assert_eq!(
        acts[0].arguments["value"].value,
        ArgumentValue::Json(Value::from("Lisbon offsite"))
    );
    assert_eq!(
        acts[1].arguments["date"].value,
        ArgumentValue::Json(Value::from("2026-11-30"))
    );
}

#[tokio::test]
async fn none_stands_alone() {
    let script = script()
        .answer("turn/segment", one_request(1, 14))
        .answer("u1/route", json!({"operations": [SET_NAME, "none"]}))
        .answer("u1/route", json!({"operations": ["none"]}));
    let run = understand(script, &turn(MESSAGE)).await;

    assert!(run.understanding.acts.is_empty());
    assert_eq!(run.understanding.not_understood.len(), 1);
}
