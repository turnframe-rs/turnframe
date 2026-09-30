//! A value the message itself holds is taken from the message, even when the reading points
//! at an earlier message holding the same words: what the user says now is the value, and
//! the earlier words may differ in what matching looks past, such as a stray mark.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand};
use turnframe_core::understanding::{ArgumentValue, MessageRef};
use turnframe_understand::Speaker;

#[tokio::test]
async fn a_value_the_message_repeats_is_taken_from_the_message() {
    // [1]I [2]made [3]a [4]mistake, [5]the [6]name [7]is [8]Lisbon [9]offsite
    let text = "I made a mistake, the name is Lisbon offsite";
    let input = turn(text).with_earlier(Speaker::User, "Lisbon offsite\\");
    let script = script()
        .answer("turn/segment", one_request(1, 9))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {"kind": "words", "text": "Lisbon offsite",
                                            "message": "m1", "from": 1, "to": 2}}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    let value = &act.arguments["value"];
    assert_eq!(
        value.value,
        ArgumentValue::Json(json!("Lisbon offsite")),
        "{act:?}"
    );
    assert_eq!(
        value.excerpt.map(|excerpt| excerpt.message),
        Some(MessageRef::Current)
    );
}
