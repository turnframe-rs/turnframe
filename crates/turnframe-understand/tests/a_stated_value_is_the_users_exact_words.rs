//! A text value is the user's words, sliced by code from a pointer, with their offsets.
mod support;

use serde_json::{Value, json};
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue, MessageRef};

#[tokio::test]
async fn a_stated_value_is_the_users_exact_words() {
    let message = "set the name to Porto for March";
    let script = script()
        .answer("turn/segment", one_request(1, 7))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 7)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn(message)).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready);
    let value = &act.arguments["value"];
    assert_eq!(
        value.value,
        ArgumentValue::Json(Value::from("Porto for March"))
    );
    let excerpt = value.excerpt.unwrap();
    assert_eq!(excerpt.message, MessageRef::Current);
    assert_eq!(
        &message[excerpt.words.start..excerpt.words.end],
        "Porto for March"
    );
}
