//! A request nothing on offer does, which coverage reads as a question, becomes that
//! question and is framed, rather than being reported as not understood.
mod support;

use serde_json::json;
use support::{one_request, turn, understand};
use turnframe_core::understanding::QuestionTopic;
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn a_request_nothing_does_that_coverage_reads_as_a_question_is_that_question() {
    // [1]may [2]I [3]change [4]it?
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", one_request(1, 4))
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "question", "words": {"from": 1, "to": 4}, "workflow": "unknown"}]}),
        )
        .answer("u1/route", json!({"operations": ["none"]}))
        .answer(
            "u1/frame",
            json!({"topic": "capabilities", "record": "r1"}),
        );
    let run = understand(script, &turn("may I change it?")).await;

    let understanding = &run.understanding;
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
    let [question] = understanding.questions.as_slice() else {
        panic!("one question expected: {understanding:?}");
    };
    assert_eq!(question.topic, QuestionTopic::Capabilities);
    assert!(understanding.acts.is_empty());
}
