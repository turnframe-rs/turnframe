//! With `reread_small_talk`, words segmentation read as small talk and coverage as an act
//! go back to segmentation once, told what coverage saw. A dispute the second reading
//! leaves is reported unclear, never acted on.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use serde_json::json;
use support::{turn, understand};
use turnframe_core::understanding::{NotUnderstoodReason, UnitKind};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::Settings;

#[tokio::test]
async fn small_talk_read_as_an_act_is_segmented_again_when_asked() {
    // [1]I [2]give [3]up
    let small_talk = json!({"analysis": "A remark.", "units": [
        {"kind": "chitchat", "words": {"from": 1, "to": 3}}
    ]});
    let cancel = json!({"missed": [
        {"kind": "cancel", "words": {"from": 1, "to": 3}, "workflow": "unknown"}
    ]});
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", small_talk.clone())
        .answer("turn/coverage", cancel.clone())
        .answer("turn/segment.after_coverage", small_talk)
        .answer("turn/coverage.after_segment", cancel);
    let input =
        turn("I give up").with_settings(Settings::conservative().with_reread_small_talk(true));
    let run = understand(script, &input).await;

    assert!(run.was_called("turn/segment.after_coverage"));
    let told = format!(
        "{:?}",
        run.provider
            .calls()
            .iter()
            .find(|call| format!("{:?}", call.metadata).contains("after_coverage"))
            .unwrap()
            .messages
    );
    assert!(told.contains("read them as a cancel"), "{told}");
    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert_eq!(understanding.units[0].kind, UnitKind::Chitchat);
    assert_eq!(
        understanding.not_understood[0].reason,
        NotUnderstoodReason::Unclear
    );
}
