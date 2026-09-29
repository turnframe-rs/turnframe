//! «set the name to Lisbon, actually leave it»: no act; the request is superseded.
mod support;

use serde_json::json;
use support::{SET_NAME, routed, script, turn, understand};
use turnframe_core::understanding::{ActAction, ActId, Superseded, UnitId};

#[tokio::test]
async fn a_cancel_withdraws_the_act_it_names() {
    // [1]set [2]the [3]name [4]to [5]Lisbon, [6]actually [7]leave [8]it
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A request, then its withdrawal.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"},
                {"kind": "cancel", "words": {"from": 6, "to": 8}, "workflow": "trip", "cancels": 1}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME));
    let run = understand(script, &turn("set the name to Lisbon, actually leave it")).await;

    let understanding = &run.understanding;
    assert!(
        understanding.acts.is_empty(),
        "nothing is left to run: {understanding:?}"
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
        !run.was_called("u1/extract"),
        "a withdrawn act is not filled in"
    );
}
