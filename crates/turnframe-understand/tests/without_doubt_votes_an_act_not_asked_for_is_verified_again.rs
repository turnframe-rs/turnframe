//! With no doubt votes a single verdict found the act was not asked for: repaired to the same
//! values, it is verified once more, so one misjudged vote does not drop it.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;

#[tokio::test]
async fn without_doubt_votes_an_act_not_asked_for_is_verified_again() {
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 4)}}))
        .answer(
            "u1/verify",
            json!({"reason": "Not asked.", "arguments": {"value": "stated"}, "overall": "not_requested"}),
        )
        .answer("u1/extract.after_verify", json!({"arguments": {"value": words(3, 4)}}))
        .answer("u1/verify.after_repair", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("name it Lisbon offsite")).await;

    let [act] = run.understanding.acts.as_slice() else {
        panic!("{:?} {:?}", run.understanding, run.called());
    };
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
}
