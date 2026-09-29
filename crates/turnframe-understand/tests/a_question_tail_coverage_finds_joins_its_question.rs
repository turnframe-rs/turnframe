//! A question coverage finds right after another question's words is that question's
//! tail: one question, answered with all its words.
mod support;

use serde_json::json;
use support::{turn, understand};
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn a_question_tail_coverage_finds_joins_its_question() {
    let text = "the name is still empty, right?";
    // [1]the [2]name [3]is [4]still [5]empty, [6]right?
    let script = ScriptedTasks::new("scripted", "small")
        .answer(
            "turn/segment",
            json!({"analysis": "Asks about the name.", "units": [
                {"kind": "question", "words": {"from": 1, "to": 5}, "workflow": "trip",
                 "basis": "current_committed_state", "continues_previous": false}
            ]}),
        )
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "question", "words": {"from": 6, "to": 6}, "workflow": "trip"}]}),
        )
        .answer(
            "u1/frame",
            json!({"topic": "record_state", "record": "r1", "subjects": []}),
        );
    let run = understand(script, &turn(text)).await;

    let questions = &run.understanding.questions;
    assert_eq!(questions.len(), 1, "{questions:?}");
    let words = questions[0].words;
    assert_eq!(&text[words.start..words.end], text);
    assert!(!run.was_called("u2/frame"));
}
