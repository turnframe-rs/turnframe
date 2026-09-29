//! A question a check finds inside words read as a dispute takes only its own words: the
//! rest are still the dispute, so an act the check reads there sends the reading back.
mod support;

use serde_json::json;
use support::{turn, understand};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::{PreviousReceipt, Speaker};

#[tokio::test]
async fn a_question_inside_a_dispute_leaves_the_rest_a_dispute() {
    // [1]is [2]that [3]a [4]name? [5]that [6]is [7]wrong
    let dispute = json!({"analysis": "Disputes the reported name.", "units": [
        {"kind": "dispute", "words": {"from": 1, "to": 7}, "receipt": "r1"}
    ]});
    let checked = json!({"missed": [
        {"kind": "question", "words": {"from": 1, "to": 4}, "workflow": "unknown"},
        {"kind": "correction", "words": {"from": 5, "to": 7}, "workflow": "trip"}
    ]});
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", dispute.clone())
        .answer("turn/coverage", checked.clone())
        .answer("turn/segment.after_coverage", dispute)
        .answer("turn/coverage.after_segment", checked);
    let input = turn("is that a name? that is wrong")
        .with_earlier(
            Speaker::Assistant,
            "Done. The trip is called «needs changing».",
        )
        .with_receipt(PreviousReceipt::new("r1", "Trip named: needs changing."));
    let run = understand(script, &input).await;

    assert!(
        run.was_called("turn/segment.after_coverage"),
        "{:?}",
        run.called()
    );
    assert!(run.understanding.acts.is_empty(), "{:?}", run.understanding);
}
