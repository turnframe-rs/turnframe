//! «tomorrow» comes back as an expression; the date is computed by code from today.
mod support;

use serde_json::{Value, json};
use support::{SET_DATE, confirmed, one_request, routed, script, turn, understand};
use turnframe_core::understanding::ArgumentValue;

#[tokio::test]
async fn relative_dates_are_computed_by_code() {
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(SET_DATE))
        .answer(
            "u1/extract",
            json!({"arguments": {"date": {
                "kind": "date", "message": "current", "from": 4, "to": 4,
                "date": {"kind": "relative", "unit": "day", "amount": 1}
            }}}),
        )
        .answer("u1/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn("make it fly tomorrow")).await;

    let date = &run.understanding.acts[0].arguments["date"];
    assert_eq!(date.value, ArgumentValue::Json(Value::from("2026-09-27")));
}
