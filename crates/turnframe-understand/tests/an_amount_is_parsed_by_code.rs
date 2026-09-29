//! An amount comes back as a decimal and a currency; code turns it into minor units.
mod support;

use serde_json::json;
use support::{ADD_EXTRA, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ArgumentValue;

#[tokio::test]
async fn an_amount_is_parsed_by_code() {
    // [1]add [2]an [3]extra [4]seat [5]upgrade, [6]120.50 [7]euro
    let script = script()
        .answer("turn/segment", one_request(1, 7))
        .answer("u1/route", routed(ADD_EXTRA))
        .answer(
            "u1/extract",
            json!({"arguments": {
                "description": words(4, 5),
                "amount": {"kind": "money", "message": "current", "from": 6, "to": 7,
                           "amount": "120.50", "currency": "EUR"}
            }}),
        )
        .answer(
            "u1/verify",
            confirmed(json!({"description": "stated", "amount": "stated"})),
        );
    let run = understand(script, &turn("add an extra seat upgrade, 120.50 euro")).await;

    let amount = &run.understanding.acts[0].arguments["amount"];
    assert_eq!(
        amount.value,
        ArgumentValue::Json(json!({"minor": 12050, "currency": "EUR"}))
    );
}
