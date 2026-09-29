//! An act whose keep-unchanged check could not be had does not run: nothing shows it keeps
//! to what the user asked.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, script, turn, understand, words};
use turnframe_core::understanding::NotUnderstoodReason;
use turnframe_provider::error::ProviderError;

#[tokio::test]
async fn an_act_the_check_could_not_judge_is_held() {
    // [1]name [2]Lisbon, [3]but [4]leave [5]the [6]date
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A set and a constraint.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "constraint", "words": {"from": 3, "to": 6}, "constraint": "keep_unchanged"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [SET_NAME]}))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .failing("u1/respects", ProviderError::other("down"));
    let run = understand(script, &turn("name Lisbon, but leave the date")).await;

    assert!(run.understanding.acts.is_empty(), "{:?}", run.understanding);
    assert!(
        run.understanding
            .not_understood
            .iter()
            .any(|item| matches!(item.reason, NotUnderstoodReason::TaskFailed { .. })),
        "{:?} {:?}",
        run.understanding.not_understood,
        run.called()
    );
}
