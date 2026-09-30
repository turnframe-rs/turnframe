//! Asked what to call a record, the user may answer with words that are also a record's label:
//! «Trip 1.» names the trip Trip 1. A reading that gives the asked value no value reads the
//! answer once more, told the user's own words are the value.
#![allow(clippy::panic)]

mod support;

use std::collections::BTreeMap;

use serde_json::json;
use support::{SET_NAME, confirmed, script, turn, understand};
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::{Expectation, PendingAct};

#[tokio::test]
async fn an_asked_value_that_is_also_a_record_label_is_still_the_value() {
    // [1]Trip [2]1.
    let input = turn("Trip 1.").with_expectation(Expectation::Values(PendingAct {
        operation: SET_NAME.into(),
        record: Some("tok-trip-1".into()),
        given: BTreeMap::new(),
        missing: vec!["value".to_owned()],
    }));
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "The asked name.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 2}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_NAME]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/extract.after_asked",
            json!({"arguments": {"value": {"kind": "words", "text": "Trip 1", "message": "current", "from": 1, "to": 2}}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input).await;

    let [act] = run.understanding.acts.as_slice() else {
        panic!("one act: {:?}", run.understanding);
    };
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json("Trip 1".into())
    );
}

#[tokio::test]
async fn so_it_is_when_the_record_was_asked_for_what_it_still_needs() {
    // [1]Trip [2]1.
    let input = turn("Trip 1.").with_expectation(Expectation::Obligation {
        record: "tok-trip-1".into(),
        sentence: "What should I call this trip?".to_owned(),
    });
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "The asked name.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 2}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_NAME]}))
        .answer("u1/locate", json!({"record": "r1", "named": null}))
        .answer("u1/extract", json!({"arguments": {"value": {"kind": "not_given"}}}))
        .answer(
            "u1/extract.after_asked",
            json!({"arguments": {"value": {"kind": "words", "text": "Trip 1", "message": "current", "from": 1, "to": 2}}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &input).await;

    let [act] = run.understanding.acts.as_slice() else {
        panic!("one act: {:?}", run.understanding);
    };
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json("Trip 1".into())
    );
}
