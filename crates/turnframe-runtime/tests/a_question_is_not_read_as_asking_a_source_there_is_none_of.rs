//! Without a knowledge source, no question is read as asking one: the question's framing is
//! not offered the topic, so a question is answered from what the records hold and what can
//! be done, and never with «the sources were not available».
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::complete_case;

#[tokio::test]
async fn a_question_is_not_read_as_asking_a_source_there_is_none_of() {
    let text = "what do I do now";
    let tasks = Arc::new(
        ScriptedTasks::new("tasks", "small")
            .answer(
                "turn/segment",
                json!({"analysis": "A question.", "units": [
                    {"kind": "question", "words": {"from": 1, "to": 4}, "workflow": "trip",
                     "basis": "current_committed_state", "continues_previous": false}
                ]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer(
                "u1/frame",
                json!({"topic": "record_state", "record": "r1", "subjects": []}),
            ),
    );
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understanding_tasks(Arc::clone(&tasks))
        .without_narration()
        .build()
        .await;

    harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), text))
        .await
        .unwrap();

    let framed = tasks
        .calls()
        .into_iter()
        .find(|call| call.purpose == ModelPurpose::QuestionFrame)
        .expect("the question was framed");
    let schema = serde_json::to_string(&framed.output).unwrap();
    assert!(schema.contains("record_state"), "{schema}");
    assert!(!schema.contains("\"knowledge\""), "{schema}");
}
