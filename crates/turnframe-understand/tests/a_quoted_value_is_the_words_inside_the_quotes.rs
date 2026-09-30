//! Quotes mark a value off; they are not part of it: «name "vendita libri"» sets the
//! name to vendita libri. Nor is the sentence's comma or full stop written inside them.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ArgumentValue;

#[tokio::test]
async fn a_quoted_value_is_the_words_inside_the_quotes() {
    // [1]name [2]"vendita [3]libri"
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 3)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("name \"vendita libri\"")).await;

    assert_eq!(
        run.understanding.acts[0].arguments["value"].value,
        ArgumentValue::Json("vendita libri".into())
    );
}

#[tokio::test]
async fn the_mark_ending_the_sentence_after_the_quotes_is_not_the_value() {
    // [1]call [2]it [3]«Offsite [4]Lisbona».
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 4)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("call it «Offsite Lisbona».")).await;

    assert_eq!(
        run.understanding.acts[0].arguments["value"].value,
        ArgumentValue::Json("Offsite Lisbona".into())
    );
}

async fn quoted(message: &str, from: usize, to: usize) -> ArgumentValue {
    let script = script()
        .answer("turn/segment", one_request(1, to))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": words(from, to)}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn(message)).await;
    run.understanding.acts[0].arguments["value"].value.clone()
}

#[tokio::test]
async fn the_sentences_comma_inside_the_closing_quote_is_not_the_value() {
    // [1]call [2]it [3]“Lisbon [4]offsite,” [5]please
    assert_eq!(
        quoted("call it “Lisbon offsite,” please", 3, 4).await,
        ArgumentValue::Json("Lisbon offsite".into())
    );
}

#[tokio::test]
async fn nor_is_its_full_stop_while_an_initialism_keeps_its_own() {
    // [1]call [2]it [3]"Lisbon [4]offsite."
    assert_eq!(
        quoted("call it \"Lisbon offsite.\"", 3, 4).await,
        ArgumentValue::Json("Lisbon offsite".into())
    );
    // [1]call [2]it [3]"Aurora [4]S.r.l."
    assert_eq!(
        quoted("call it \"Aurora S.r.l.\"", 3, 4).await,
        ArgumentValue::Json("Aurora S.r.l.".into())
    );
}
