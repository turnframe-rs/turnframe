//! Quotes mark a value off; they are not part of it: «name "vendita libri"» sets the
//! name to vendita libri.
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
