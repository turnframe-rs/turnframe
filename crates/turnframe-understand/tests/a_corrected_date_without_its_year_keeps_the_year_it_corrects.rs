//! A correction keeps what it does not restate: a date corrected without its year takes the
//! year of the date it corrects, stated earlier in the message or done last turn, and is not
//! read against today.
#![allow(clippy::panic)]

mod support;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use support::{SET_DATE, confirmed, routed, script, turn, understand};
use turnframe_core::understanding::{ArgumentValue, UnderstoodArgument};
use turnframe_understand::PendingAct;

fn date(from: usize, to: usize, year: Option<i32>, month: u32, day: u32) -> Value {
    json!({"kind": "date", "message": "current", "from": from, "to": to,
           "date": {"kind": "absolute", "year": year, "month": month, "day": day}})
}

fn only_date(run: &support::Run) -> Value {
    let [act] = run.understanding.acts.as_slice() else {
        panic!("one act expected: {:?}", run.understanding);
    };
    match &act.arguments["date"].value {
        ArgumentValue::Json(value) => value.clone(),
        other => panic!("a date expected: {other:?}"),
    }
}

#[tokio::test]
async fn a_date_corrected_in_the_same_message_keeps_its_year() {
    // [1]10 [2]December [3]2027, [4]no [5]wait, [6]12 [7]December
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A request, then a correction of it.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "trip"},
                {"kind": "correction", "words": {"from": 4, "to": 7}, "workflow": "trip", "corrects": 1}
            ]}),
        )
        .answer("u1/route", routed(SET_DATE))
        .answer(
            "u2/extract",
            json!({"arguments": {"date": date(6, 7, None, 12, 12)}}),
        )
        .answer(
            "u2/extract.corrected",
            json!({"arguments": {"date": date(1, 3, Some(2027), 12, 10)}}),
        )
        .answer("u2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn("10 December 2027, no wait, 12 December")).await;
    assert_eq!(only_date(&run), json!("2027-12-12"));
}

#[tokio::test]
async fn a_date_corrected_after_the_last_turn_keeps_its_year() {
    // [1]no, [2]12 [3]December
    let done = PendingAct {
        operation: SET_DATE.into(),
        record: Some("tok-trip-1".into()),
        given: BTreeMap::from([(
            "date".to_owned(),
            UnderstoodArgument {
                value: ArgumentValue::Json(json!("2027-12-10")),
                excerpt: None,
            },
        )]),
        missing: Vec::new(),
    };
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A correction.", "units": [{"kind": "correction",
                "words": {"from": 1, "to": 3}, "workflow": "trip", "corrects": null}]}),
        )
        .answer("u1/route", routed(SET_DATE))
        .answer(
            "u1/extract",
            json!({"arguments": {"date": date(2, 3, None, 12, 12)}}),
        )
        .answer("u1/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn("no, 12 December").with_done(done)).await;
    assert_eq!(only_date(&run), json!("2027-12-12"));
    assert!(
        !run.was_called("u1/extract.corrected"),
        "the date done last turn is at hand"
    );
}

#[tokio::test]
async fn a_year_the_correction_does_not_say_is_the_corrected_dates() {
    // [1]10 [2]December [3]2027, [4]no [5]wait, [6]12 [7]December
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A request, then a correction of it.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "trip"},
                {"kind": "correction", "words": {"from": 4, "to": 7}, "workflow": "trip", "corrects": 1}
            ]}),
        )
        .answer("u1/route", routed(SET_DATE))
        .answer(
            "u2/extract",
            json!({"arguments": {"date": date(6, 7, Some(2026), 12, 12)}}),
        )
        .answer(
            "u2/extract.corrected",
            json!({"arguments": {"date": date(1, 3, Some(2027), 12, 10)}}),
        )
        .answer("u2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn("10 December 2027, no wait, 12 December")).await;
    assert_eq!(only_date(&run), json!("2027-12-12"));
}
