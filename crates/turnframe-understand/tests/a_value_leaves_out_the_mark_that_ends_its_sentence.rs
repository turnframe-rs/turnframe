//! The full stop that ends the sentence is not the value's, unless the copy keeps it: a name
//! that ends in one of its own keeps it, and so does one whose mark a comma follows.
mod support;

use serde_json::{Value, json};
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand};
use turnframe_core::understanding::ArgumentValue;

async fn subject(message: &str, from: usize, to: usize, copied: &str) -> ArgumentValue {
    let script = script()
        .answer("turn/segment", one_request(1, to))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {
                "kind": "words", "message": "current", "from": from, "to": to, "text": copied
            }}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn(message)).await;
    run.understanding.acts[0].arguments["value"].value.clone()
}

#[tokio::test]
async fn a_value_leaves_out_the_mark_that_ends_its_sentence() {
    // [1]the [2]name [3]is [4]AZ1234567.
    assert_eq!(
        subject("the name is AZ1234567.", 4, 4, "AZ1234567").await,
        ArgumentValue::Json(Value::from("AZ1234567"))
    );
}

#[tokio::test]
async fn a_mark_the_copy_keeps_is_the_values() {
    // [1]the [2]name [3]is [4]Bianchi [5]S.r.l.
    assert_eq!(
        subject("the name is Aurora S.r.l.", 4, 5, "Aurora S.r.l.").await,
        ArgumentValue::Json(Value::from("Aurora S.r.l."))
    );
}

#[tokio::test]
async fn a_mark_before_the_comma_joining_the_next_words_is_the_values() {
    // [1]the [2]name [3]is [4]Bianchi [5]S.r.l., [6]thanks
    assert_eq!(
        subject("the name is Aurora S.r.l., thanks", 4, 5, "Aurora S.r.l").await,
        ArgumentValue::Json(Value::from("Aurora S.r.l."))
    );
}
