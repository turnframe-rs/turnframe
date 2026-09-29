//! «name Lisbon, no, Porto»: one act, from the correction; the first is superseded.
mod support;

use serde_json::{Value, json};
use support::{SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActAction, ActId, ArgumentValue, Superseded, UnitId};

#[tokio::test]
async fn a_correction_replaces_the_act_it_corrects() {
    // [1]set [2]the [3]name [4]to [5]Lisbon, [6]no, [7]Porto
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A request, then a correction of it.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"},
                {"kind": "correction", "words": {"from": 6, "to": 7}, "workflow": "trip", "corrects": 1}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/extract", json!({"arguments": {"value": words(7, 7)}}))
        .answer("u2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("set the name to Lisbon, no, Porto")).await;

    let understanding = &run.understanding;
    let [act] = understanding.acts.as_slice() else {
        panic!("one act expected: {understanding:?}");
    };
    assert_eq!(act.id, ActId::new(UnitId(2), 1));
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(Value::from("Porto"))
    );
    assert_eq!(
        understanding.superseded,
        vec![Superseded {
            act: ActId::new(UnitId(1), 1),
            action: ActAction::Apply {
                operation: SET_NAME.into()
            },
            by: UnitId(2)
        }]
    );
    assert!(
        !run.was_called("u2/route"),
        "a correction takes the route of what it corrects"
    );
    let extract = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("u2/extract"))
        .unwrap();
    assert!(format!("{:?}", extract.messages).contains("It continues: words 1 to 5"));
}
