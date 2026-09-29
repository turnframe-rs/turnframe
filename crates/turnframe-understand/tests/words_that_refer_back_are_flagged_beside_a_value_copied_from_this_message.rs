//! A value copied from this message is verified beside the rule that words only referring
//! back to an earlier value are not that value; one taken from the earlier message is not,
//! since the message referring back to it is what makes it stated.
mod support;

use serde_json::json;
use support::{
    SET_DATE, SET_NAME, confirmed, one_request, routed, script, turn, understand, words,
};
use turnframe_provider::request::ContentPart;
use turnframe_understand::Speaker;

const NOTE: &str = "words that only refer back to a value said before";

async fn verified_with(value: serde_json::Value) -> String {
    verified(SET_NAME, "value", value).await
}

async fn verified(operation: &str, argument: &str, value: serde_json::Value) -> String {
    // m1: [1]call [2]it [3]Porto
    // [1]name [2]it [3]what [4]I [5]said
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(operation))
        .answer("u1/extract", json!({"arguments": {argument: value}}))
        .answer("u1/verify", confirmed(json!({argument: "stated"})));
    let input = turn("name it what I said")
        .with_earlier(Speaker::User, "call it Porto")
        .with_earlier(Speaker::Assistant, "What else?");
    let run = understand(script, &input).await;
    run.provider
        .calls()
        .into_iter()
        .filter(|request| request.metadata.get("task") == Some("u1/verify"))
        .flat_map(|request| request.messages)
        .flat_map(|message| message.content)
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn words_that_refer_back_are_flagged_beside_a_value_copied_from_this_message() {
    let copied = verified_with(words(3, 5)).await;
    assert!(copied.contains(NOTE), "{copied}");

    let earlier = verified_with(json!({"kind": "words", "text": "Porto", "message": "m1",
        "from": 3, "to": 3}))
    .await;
    assert!(earlier.contains("(from earlier message m1"), "{earlier}");
    assert!(!earlier.contains(NOTE), "{earlier}");
}

#[tokio::test]
async fn a_value_computed_from_words_of_this_message_is_no_copy() {
    // [1]name [2]it [3]what [4]I [5]said: a date read from words 4 to 5 is no copy of them
    let date = json!({"kind": "date", "message": "current", "from": 4, "to": 5,
        "date": {"kind": "relative", "unit": "day", "amount": 1}});
    let shown = verified(SET_DATE, "date", date).await;
    assert!(shown.contains("Understood:"), "{shown}");
    assert!(!shown.contains(NOTE), "{shown}");
}
