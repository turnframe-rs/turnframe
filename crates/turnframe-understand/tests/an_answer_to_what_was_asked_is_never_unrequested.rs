//! An answer to the value the assistant asked for is requested by the asking: the verifier
//! judges whether the value is what the user said, and a verdict that nobody asked for the
//! act does not drop it, whatever else the message says.
mod support;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use support::{SET_NAME, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::{Expectation, PendingAct};

#[tokio::test]
async fn an_answer_to_what_was_asked_is_never_unrequested() {
    let pending = PendingAct {
        operation: SET_NAME.into(),
        record: Some("tok-trip-1".into()),
        given: BTreeMap::new(),
        missing: vec!["value".to_owned()],
    };
    // [1]Porto [2]isn't [3]that [4]obvious!?
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Gives the name and complains.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 1}},
                {"kind": "chitchat", "words": {"from": 2, "to": 4}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_NAME]}))
        .answer("u1/extract", json!({"arguments": {"value": words(1, 1)}}))
        .answer(
            "u1/verify",
            json!({"reason": "The user only complained.", "arguments": {"value": "stated"},
                   "overall": "not_requested"}),
        );
    let input = turn("Porto isn't that obvious!?").with_expectation(Expectation::Values(pending));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{:?}", run.understanding);
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(Value::from("Porto"))
    );
}
