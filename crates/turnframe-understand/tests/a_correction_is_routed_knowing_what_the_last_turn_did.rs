//! «No sorry, it was reduced rate» corrects something the last turn did: its routing is
//! shown what that was, so it can choose the operation that changes it.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_understand::PreviousReceipt;

#[tokio::test]
async fn a_correction_is_routed_knowing_what_the_last_turn_did() {
    // [1]no, [2]it [3]was [4]Lisbon
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A correction.", "units": [
                {"kind": "correction", "words": {"from": 1, "to": 4}, "workflow": "trip",
                 "corrects": null}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(4, 4)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let input = turn("no, it was Lisbon").with_receipt(PreviousReceipt::new(
        "r1",
        "Name set: The name is now Porto.",
    ));
    let run = understand(script, &input).await;

    let shown = run
        .provider
        .calls()
        .iter()
        .find(|call| format!("{:?}", call.metadata).contains("\"u1/route\""))
        .map(|call| format!("{:?}", call.messages))
        .unwrap_or_default();
    assert!(
        shown.contains("Name set: The name is now Porto."),
        "{shown}"
    );
}
