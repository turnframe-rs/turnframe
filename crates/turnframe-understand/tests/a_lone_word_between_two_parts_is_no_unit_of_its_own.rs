//! A lone word coverage finds between two parts, one ending right before it and one
//! beginning right after, is the word joining them: no lost constraint and no request of its
//! own, so the message is not read again for it. A lone word at the edge of the message
//! holds no such place, and a constraint lost there still fails the turn closed.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, routed, turn, understand, words};
use turnframe_core::understanding::Unreadable;
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn a_lone_word_between_two_parts_is_no_unit_of_its_own() {
    // [1]rename [2]it [3]Porto, [4]but [5]keep [6]the [7]date, [8]and [9]confirm [10]nothing
    let units = json!({"analysis": "A rename and two conditions.", "units": [
        {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "trip"},
        {"kind": "constraint", "words": {"from": 5, "to": 7}, "constraint": "keep_unchanged"},
        {"kind": "constraint", "words": {"from": 9, "to": 10}, "constraint": "do_not_submit"}
    ]});
    let joining = json!({"missed": [
        {"kind": "constraint", "words": {"from": 4, "to": 4}, "workflow": "trip"},
        {"kind": "request", "words": {"from": 8, "to": 8}, "workflow": "trip"}
    ]});
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", units)
        .answer("turn/coverage", joining)
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 3)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "u1/respects",
            json!({"reason": "Renames the trip.", "changes": false}),
        );
    let run = understand(
        script,
        &turn("rename it Porto, but keep the date, and confirm nothing"),
    )
    .await;

    let understanding = &run.understanding;
    assert_eq!(understanding.unreadable, None, "{understanding:?}");
    assert_eq!(understanding.units.len(), 3, "{understanding:?}");
    assert_eq!(understanding.constraints.len(), 2, "{understanding:?}");
    assert_eq!(understanding.acts.len(), 1, "{understanding:?}");
    assert!(
        !run.was_called("turn/segment.after_coverage"),
        "{:?}",
        run.called()
    );
}

#[tokio::test]
async fn a_lone_word_at_the_edge_of_the_message_still_fails_it_closed() {
    // [1]rename [2]it [3]Porto, [4]provisionally
    let lost = json!({"missed": [
        {"kind": "constraint", "words": {"from": 4, "to": 4}, "workflow": "trip"}
    ]});
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", support::one_request(1, 3))
        .answer("turn/coverage", lost.clone())
        .answer("u1/route", routed(SET_NAME))
        .answer("turn/segment.after_coverage", support::one_request(1, 3))
        .answer("turn/coverage.after_segment", lost)
        .answer("u1/route", routed(SET_NAME));
    let run = understand(script, &turn("rename it Porto, provisionally")).await;

    assert_eq!(
        run.understanding.unreadable,
        Some(Unreadable::LostConstraint)
    );
}
