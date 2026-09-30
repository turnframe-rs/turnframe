//! Words the segmentation took for an answer, read as another operation than the one the
//! last reply asked for, ask for that operation: they are checked as a request, not as an
//! answer to a question they do not answer.
mod support;

use std::collections::BTreeMap;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, script, turn, understand};
use turnframe_understand::{Expectation, PendingAct};

#[tokio::test]
async fn a_value_read_as_another_operation_than_the_one_asked_is_a_request() {
    let pending = PendingAct {
        operation: SET_NAME.into(),
        record: Some("tok-trip-1".into()),
        given: BTreeMap::new(),
        missing: vec!["value".to_owned()],
    };
    // [1]on [2]12 [3]May
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Gives a date.", "units": [
                {"kind": "provides_value", "words": {"from": 1, "to": 3}}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_DATE]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"date": {"kind": "date", "message": "current", "from": 2,
                "to": 3, "date": {"kind": "absolute", "year": null, "month": 5, "day": 12}}}}),
        )
        .answer("u1/verify", confirmed(json!({"date": "stated"})));
    let input = turn("on 12 May").with_expectation(Expectation::Values(pending));
    let run = understand(script, &input).await;

    let verify = run
        .provider
        .calls()
        .into_iter()
        .map(|call| format!("{:?}", call.messages))
        .find(|text| text.contains("The part of the message this act reads"))
        .unwrap_or_default();
    assert!(
        verify.contains("this act reads, a request"),
        "{verify}\n{:?}",
        run.understanding
    );
}
