//! Words every reading took as small talk and one check read as an act go back once; when
//! the second reading makes parts of them that no operation on offer does, neither reading
//! found anything to do. The small talk stands: nothing is reported as not understood.
mod support;

use serde_json::json;
use support::{turn, understand};
use turnframe_core::understanding::UnitKind;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::Settings;

#[tokio::test]
async fn small_talk_read_again_stands_when_nothing_on_offer_does_it() {
    // [1]I [2]do [3]not [4]see [5]it [6]here
    let none = json!({"operations": ["none"]});
    let script = ScriptedTasks::new("scripted", "small")
        .answer(
            "turn/segment",
            json!({"analysis": "A remark.", "units": [
                {"kind": "chitchat", "words": {"from": 1, "to": 6}}
            ]}),
        )
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "correction", "words": {"from": 2, "to": 6}, "workflow": "unknown"}]}),
        )
        .answer(
            "turn/segment.after_coverage",
            json!({"analysis": "A correction.", "units": [
                {"kind": "correction", "words": {"from": 2, "to": 6}, "workflow": "unknown",
                 "corrects": null}
            ]}),
        )
        .answer(
            "turn/coverage.after_segment",
            json!({"missed": [{"kind": "correction", "words": {"from": 1, "to": 1}, "workflow": "unknown"}]}),
        )
        .answer("u1/route", none.clone())
        .answer("u2/route", none);
    let input = turn("I do not see it here")
        .with_settings(Settings::conservative().with_reread_small_talk(true));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert!(
        understanding
            .units
            .iter()
            .all(|unit| unit.kind == UnitKind::Chitchat),
        "{understanding:?}"
    );
}
