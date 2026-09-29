//! Words segmentation reads as a dispute, read again after a check took them for an act and
//! still read as a dispute, stand as that dispute: the reply owns it, and nothing is
//! reported as not understood.
mod support;

use serde_json::json;
use support::{turn, understand};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::{PreviousReceipt, Speaker, Step};

#[tokio::test]
async fn a_dispute_read_twice_stands() {
    // [1]AZ1234567 [2]has [3]nine [4]characters!?
    let dispute = json!({"analysis": "Disputes the reported number.", "units": [
        {"kind": "dispute", "words": {"from": 1, "to": 4}, "receipt": "r1"}
    ]});
    let correction = json!({"missed": [{"kind": "correction", "words": {"from": 1, "to": 4}, "workflow": "unknown"}]});
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", dispute.clone())
        .answer("turn/coverage", correction.clone())
        .answer("turn/segment.after_coverage", dispute)
        .answer("turn/coverage.after_segment", correction);
    let input = turn("AZ1234567 has nine characters!?")
        .with_earlier(Speaker::Assistant, "Done. The loyalty number is set.")
        .with_receipt(PreviousReceipt::new("r1", "Loyalty number set: AZ1234567."));
    let run = understand(script, &input).await;

    assert_eq!(
        run.understanding.disputes.len(),
        1,
        "{:?}",
        run.understanding
    );
    assert!(
        run.understanding.not_understood.is_empty(),
        "{:?}",
        run.understanding
    );
    let analyses: Vec<String> = run
        .steps
        .steps()
        .into_iter()
        .filter_map(|step| match step {
            Step::Segmented { analysis, .. } => Some(analysis),
            _ => None,
        })
        .collect();
    assert_eq!(
        analyses,
        ["Disputes the reported number.".to_owned(), String::new()],
        "a reading told what a check saw speaks of the check, so its analysis is not shown"
    );
}
