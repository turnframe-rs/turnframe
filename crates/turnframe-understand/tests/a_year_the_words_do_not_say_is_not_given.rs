//! A date's year is the user's only when the words it points at say it: «21 October», read
//! with a year taken from today's date, is a date with no year, placed as its argument's
//! direction places one. «21 October 2027» keeps its year.
#![allow(clippy::panic)]

mod support;

use serde_json::{Value, json};
use support::{SET_DATE, confirmed, routed, script, turn, understand};
use turnframe_core::understanding::ArgumentValue;

async fn dated(message: &str, to: usize, year: i32) -> Value {
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A date.", "units": [
                {"kind": "request", "words": {"from": 1, "to": to}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_DATE))
        .answer(
            "u1/extract",
            json!({"arguments": {"date": {"kind": "date", "message": "current", "from": 3, "to": to,
                   "date": {"kind": "absolute", "year": year, "month": 10, "day": 21}}}}),
        )
        .answer("u1/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn(message)).await;
    match &run.understanding.acts[0].arguments["date"].value {
        ArgumentValue::Json(value) => value.clone(),
        other => panic!("a date: {other:?}"),
    }
}

#[tokio::test]
async fn a_year_the_words_do_not_say_is_not_given() {
    // [1]fly [2]on [3]21 [4]October
    assert_eq!(
        dated("fly on 21 October", 4, 2023).await,
        json!("2026-10-21")
    );
}

#[tokio::test]
async fn a_year_the_words_say_is_kept() {
    // [1]fly [2]on [3]21 [4]October [5]2027
    assert_eq!(
        dated("fly on 21 October 2027", 5, 2027).await,
        json!("2027-10-21")
    );
}
