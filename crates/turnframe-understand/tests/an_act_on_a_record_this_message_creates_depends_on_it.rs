//! «create an trip and set its name to Lisbon»: the second act targets the first's record.
mod support;

use serde_json::json;
use support::{OPEN, SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActId, ActStatus, ActTarget, UnitId};

#[tokio::test]
async fn an_act_on_a_record_this_message_creates_depends_on_it() {
    // [1]create [2]an [3]trip [4]and [5]set [6]its [7]name [8]to [9]Lisbon
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Create an trip, then set its name.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 5, "to": 9}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(OPEN))
        .answer("u1/verify", confirmed(json!({})))
        .answer("u2/route", routed(SET_NAME))
        .answer("u2/locate", json!({"record": "s1", "named": null}))
        .answer("u2/extract", json!({"arguments": {"value": words(9, 9)}}))
        .answer("u2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("create an trip and set its name to Lisbon")).await;

    let acts = &run.understanding.acts;
    assert_eq!(acts.len(), 2, "{acts:?}");
    let create = ActId::new(UnitId(1), 1);
    assert_eq!(
        acts[0].target,
        ActTarget::New {
            workflow: "trip".into()
        }
    );
    assert_eq!(acts[1].target, ActTarget::SameTurn { act: create });
    assert_eq!(acts[1].depends_on, vec![create]);
    assert!(acts.iter().all(|act| act.status == ActStatus::Ready));
}
