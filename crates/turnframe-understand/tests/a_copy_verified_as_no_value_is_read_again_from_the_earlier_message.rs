//! A value copied from this message that the verifier finds gives no value is sent back told
//! that words referring back give the value said before, not that the user gave none: the
//! reading then finds it in the earlier message.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_provider::request::ContentPart;
use turnframe_understand::Speaker;

#[tokio::test]
async fn a_copy_verified_as_no_value_is_read_again_from_the_earlier_message() {
    // m1: [1]call [2]it [3]Porto
    // [1]name [2]it [3]what [4]I [5]said
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 5)}}))
        .answer(
            "u1/verify",
            json!({"reason": "It only points back.", "arguments": {"value": "not_stated"},
                "overall": "confirmed"}),
        )
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": {"kind": "words", "text": "Porto", "message": "m1",
                "from": 3, "to": 3}}}),
        )
        .answer(
            "u1/verify.after_repair",
            confirmed(json!({"value": "stated"})),
        );
    let input = turn("name it what I said")
        .with_earlier(Speaker::User, "call it Porto")
        .with_earlier(Speaker::Assistant, "What else?");
    let run = understand(script, &input).await;

    let told: String = run
        .provider
        .calls()
        .into_iter()
        .filter(|request| request.metadata.get("task") == Some("u1/extract.after_verify"))
        .flat_map(|request| request.messages)
        .flat_map(|message| message.content)
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        told.contains("in the earlier message that says it"),
        "{told}"
    );
    assert!(!told.contains("the user gave no value"), "{told}");
    let [act] = run.understanding.acts.as_slice() else {
        panic!("{:?}", run.understanding);
    };
    assert_eq!(act.status, ActStatus::Ready);
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(json!("Porto"))
    );
}
