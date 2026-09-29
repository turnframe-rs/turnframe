//! A text value's copied words narrow a pointer that took the field's name with it,
//! and words the pointer does not hold are sent back for a repair.
mod support;

use serde_json::{Value, json};
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand};
use turnframe_core::understanding::{ActStatus, ArgumentValue};

const MESSAGE: &str = "oggetto: sviluppo sito";

fn copied(text: &str) -> Value {
    json!({"kind": "words", "text": text, "message": "current", "from": 1, "to": 3})
}

#[tokio::test]
async fn a_copied_value_narrows_its_pointer_to_the_value() {
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": copied("sviluppo sito")}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn(MESSAGE)).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready);
    let value = &act.arguments["value"];
    assert_eq!(
        value.value,
        ArgumentValue::Json(Value::from("sviluppo sito"))
    );
    let excerpt = value.excerpt.unwrap();
    assert_eq!(
        &MESSAGE[excerpt.words.start..excerpt.words.end],
        "sviluppo sito"
    );
}

#[tokio::test]
async fn words_the_pointer_does_not_hold_are_repaired() {
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": copied("sviluppo web")}}),
        )
        .answer(
            "u1/extract",
            json!({"arguments": {"value": copied("sviluppo sito")}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn(MESSAGE)).await;

    let value = &run.understanding.acts[0].arguments["value"];
    assert_eq!(
        value.value,
        ArgumentValue::Json(Value::from("sviluppo sito"))
    );
}
