//! Coverage adds a request the segmentation missed, and it runs like any other.
mod support;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, one_request, routed, turn, understand, words};
use turnframe_core::understanding::{FoundBy, UnitId};
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn a_request_the_segmentation_missed_is_added_by_coverage() {
    // [1]name [2]Lisbon [3]and [4]fly [5]tomorrow
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", one_request(1, 2))
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "request", "words": {"from": 4, "to": 5}, "workflow": "trip"}]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u2/route", routed(SET_DATE))
        .answer(
            "u2/extract",
            json!({"arguments": {"date": {
                "kind": "date", "message": "current", "from": 5, "to": 5,
                "date": {"kind": "relative", "unit": "day", "amount": 1}
            }}}),
        )
        .answer("u2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn("name Lisbon and fly tomorrow")).await;

    let understanding = &run.understanding;
    assert_eq!(understanding.acts.len(), 2, "{understanding:?}");
    let added = understanding
        .units
        .iter()
        .find(|u| u.id == UnitId(2))
        .unwrap();
    assert_eq!(added.found_by, FoundBy::Coverage);
    assert!(
        run.provider.unanswered().is_empty(),
        "{:?}",
        run.provider.unanswered()
    );
}
