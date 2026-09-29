//! A pointer past the message's last word is a structural error, quoted back in the repair.
mod support;

use serde_json::{Value, json};
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ArgumentValue;

#[tokio::test]
async fn a_pointer_outside_the_message_is_repaired_with_the_exact_error() {
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 10)}}))
        .answer(
            "u1/extract#repair1",
            json!({"arguments": {"value": words(5, 5)}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("set the name to Lisbon")).await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(Value::from("Lisbon"))
    );
    let repair = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("u1/extract#repair1"))
        .unwrap();
    let said = format!("{:?}", repair.messages.last().unwrap());
    assert!(
        said.contains("words 5 to 10 are not in the message, whose words are numbered 1 to 5"),
        "{said}"
    );
}
