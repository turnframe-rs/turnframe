//! Words segmentation read as a dispute, and a check reads as an act, are read again told
//! what they were read as: a dispute, never small talk, with the rule that tells the two
//! apart. Told they were small talk, a reading takes the check's word for them.
mod support;

use serde_json::json;
use support::{turn, understand};
use turnframe_provider::request::ContentPart;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::{PreviousReceipt, Speaker};

#[tokio::test]
async fn a_dispute_a_check_reads_as_an_act_is_read_again_as_what_it_was() {
    // [1]that [2]is [3]wrong
    let dispute = json!({"analysis": "Disputes the reported name.", "units": [
        {"kind": "dispute", "words": {"from": 1, "to": 3}, "receipt": "r1"}
    ]});
    let correction = json!({"missed": [{"kind": "correction", "words": {"from": 1, "to": 3}, "workflow": "trip"}]});
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", dispute.clone())
        .answer("turn/coverage", correction.clone())
        .answer("turn/segment.after_coverage", dispute)
        .answer("turn/coverage.after_segment", correction);
    let input = turn("that is wrong")
        .with_earlier(
            Speaker::Assistant,
            "Done. The trip is called «needs changing».",
        )
        .with_receipt(PreviousReceipt::new("r1", "Trip named: needs changing."));
    let run = understand(script, &input).await;

    let told = run
        .provider
        .calls()
        .into_iter()
        .filter(|request| request.messages.len() > 1)
        .flat_map(|request| request.messages)
        .flat_map(|message| message.content)
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(told.contains("were read as a dispute"), "{told}");
    assert!(!told.contains("small talk"), "{told}");
    assert!(
        told.contains("words that only say a reported change is wrong"),
        "{told}"
    );
}
