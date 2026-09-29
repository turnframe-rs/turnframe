//! A value copied exactly from a message but pointed a word off is found where its words
//! stand, when they stand in that message once: the copy says what the value is, and only
//! where it is was wrong.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand};
use turnframe_core::understanding::{ActStatus, ArgumentValue, MessageRef};
use turnframe_understand::Speaker;

#[tokio::test]
async fn a_copy_pointed_beside_its_words_is_found_where_it_stands() {
    // m1: [1]the [2]trip [3]is [4]for [5]the [6]Lisbon [7]offsite
    // [1]call [2]it [3]what [4]I [5]told [6]you [7]before
    let script = script()
        .answer("turn/segment", one_request(1, 7))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {"kind": "words", "text": "the Lisbon offsite",
                "message": "m1", "from": 4, "to": 6}}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let input = turn("call it what I told you before")
        .with_earlier(Speaker::User, "the trip is for the Lisbon offsite")
        .with_earlier(Speaker::Assistant, "What should I add to it?");
    let run = understand(script, &input).await;

    let [act] = run.understanding.acts.as_slice() else {
        panic!("{:?} {:?}", run.understanding, run.called());
    };
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    let value = &act.arguments["value"];
    assert_eq!(
        value.value,
        ArgumentValue::Json(json!("the Lisbon offsite"))
    );
    let excerpt = value.excerpt.expect("pointed at");
    assert_eq!(excerpt.message, MessageRef::Earlier { index: 0 });
    assert_eq!((excerpt.words.first, excerpt.words.last), (4, 6));
    assert!(!run.was_called("u1/extract#repair1"), "{:?}", run.called());
}
