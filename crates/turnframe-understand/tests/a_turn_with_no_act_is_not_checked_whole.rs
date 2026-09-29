//! A turn that understood no act, only a question, is not checked whole: there is no
//! reading of acts to check.
mod support;

use serde_json::json;
use support::{script, turn, understand};
use turnframe_understand::Settings;

#[tokio::test]
async fn a_turn_with_no_act_is_not_checked_whole() {
    // [1]what [2]is [3]the [4]name?
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A question.", "units": [
                {"kind": "question", "words": {"from": 1, "to": 4}, "workflow": "trip",
                 "basis": "current_committed_state", "continues_previous": false}
            ]}),
        )
        .answer(
            "u1/frame",
            json!({"topic": "record_state", "record": "r1", "subjects": []}),
        );
    let input = turn("what is the name?")
        .with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    assert_eq!(run.understanding.questions.len(), 1);
    assert!(!run.was_called("turn/cross_check"));
}
