//! The full stop that ends the sentence is not the value's: an initialism keeps its own, and so
//! does a word whose mark a comma follows. A full stop after any other word or an address ends
//! the sentence, whatever the copy keeps.
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

#[tokio::test]
async fn a_lone_full_stop_after_a_word_ends_the_sentence() {
    // [1]call [2]it [3]Lisbon [4]offsite.
    assert_eq!(
        subject("call it Lisbon offsite.", 3, 4, "Lisbon offsite.").await,
        ArgumentValue::Json(Value::from("Lisbon offsite"))
    );
}

#[tokio::test]
async fn so_does_the_one_after_an_address() {
    // [1]call [2]it [3]nadia@rinaldi.example.
    assert_eq!(
        subject(
            "call it nadia@rinaldi.example.",
            3,
            3,
            "nadia@rinaldi.example."
        )
        .await,
        ArgumentValue::Json(Value::from("nadia@rinaldi.example"))
    );
}

#[tokio::test]
async fn so_does_the_one_after_a_short_word() {
    // [1]call [2]it [3]checked [4]bag.
    assert_eq!(
        subject("call it checked bag.", 3, 4, "checked bag.").await,
        ArgumentValue::Json(Value::from("checked bag"))
    );
}

#[tokio::test]
async fn so_does_the_one_after_a_short_capitalised_word() {
    // [1]call [2]it [3]Trip [4]to [5]Rio.
    assert_eq!(
        subject("call it Trip to Rio.", 3, 5, "Trip to Rio.").await,
        ArgumentValue::Json(Value::from("Trip to Rio"))
    );
}

#[tokio::test]
async fn so_does_the_one_after_a_dotted_address() {
    // [1]call [2]it [3]desk.example.com.
    assert_eq!(
        subject("call it desk.example.com.", 3, 3, "desk.example.com.").await,
        ArgumentValue::Json(Value::from("desk.example.com"))
    );
}

#[tokio::test]
async fn an_abbreviation_whose_stop_a_comma_follows_keeps_it() {
    // [1]the [2]name [3]is [4]Aurora [5]Inc., [6]thanks
    assert_eq!(
        subject("the name is Aurora Inc., thanks", 4, 5, "Aurora Inc.").await,
        ArgumentValue::Json(Value::from("Aurora Inc."))
    );
}
