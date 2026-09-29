//! Words the segmentation read as small talk and coverage as an act go back once; a second
//! reading that makes a condition of them agrees with neither, and a condition holds every
//! act of the turn. They stay small talk, and the act beside them runs.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, routed, turn, understand, words};
use turnframe_core::understanding::UnitKind;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::Settings;

#[tokio::test]
async fn small_talk_read_again_does_not_become_a_condition() {
    // [1]so [2]she [3]confirmed [4]everything, [5]name [6]it [7]Porto
    let script = ScriptedTasks::new("scripted", "small")
        .answer(
            "turn/segment",
            json!({"analysis": "A remark, then a name.", "units": [
                {"kind": "chitchat", "words": {"from": 1, "to": 4}},
                {"kind": "request", "words": {"from": 5, "to": 7}, "workflow": "trip"}
            ]}),
        )
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"}]}),
        )
        .answer("u2/route", routed(SET_NAME))
        .answer(
            "turn/segment.after_coverage",
            json!({"analysis": "A condition, then a name.", "units": [
                {"kind": "constraint", "words": {"from": 1, "to": 4}, "constraint": "apply_only_if"},
                {"kind": "request", "words": {"from": 5, "to": 7}, "workflow": "trip"}
            ]}),
        )
        .answer("turn/coverage.after_segment", json!({"missed": []}))
        .answer("u2/route", routed(SET_NAME))
        .answer("u2/extract", json!({"arguments": {"value": words(7, 7)}}))
        .answer("u2/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "u2/respects",
            json!({"reason": "Names the trip.", "changes": false}),
        );
    let input = turn("so she confirmed everything, name it Porto")
        .with_settings(Settings::conservative().with_reread_small_talk(true));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.constraints.is_empty(), "{understanding:?}");
    assert_eq!(
        understanding.units[0].kind,
        UnitKind::Chitchat,
        "{understanding:?}"
    );
    assert_eq!(understanding.acts.len(), 1, "{understanding:?}");
}
