//! A question gets its topic, the record and the declared subjects it is about, and no
//! act. Subjects belong to a question about a record's state or a field's values.
mod support;

use serde_json::json;
use support::{script, today, trip, trips, understand};
use turnframe_core::plan::AnswerBasis;
use turnframe_core::understanding::QuestionTopic;
use turnframe_understand::UnderstandingInput;

#[tokio::test]
async fn a_question_is_framed_on_its_record_and_subjects() {
    let input = UnderstandingInput::new("what does Haddad owe?", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi"), trip(2, "Haddad")]).subject("total"));
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A question.", "units": [{
                "kind": "question", "words": {"from": 1, "to": 4}, "workflow": "trip",
                "basis": "current_committed_state", "continues_previous": false
            }]}),
        )
        .answer(
            "u1/frame",
            json!({"topic": "record_state", "record": "r2", "subjects": ["total"]}),
        );
    let run = understand(script, &input).await;

    let [question] = run.understanding.questions.as_slice() else {
        panic!("one question expected: {:?}", run.understanding);
    };
    assert_eq!(
        question.record.as_ref().map(|t| t.as_str()),
        Some("tok-trip-2")
    );
    assert_eq!(question.topic, QuestionTopic::RecordState);
    assert_eq!(question.subjects, vec!["total".to_owned()]);
    assert_eq!(question.basis, AnswerBasis::CurrentCommittedState);
    assert!(run.understanding.acts.is_empty());
}

#[tokio::test]
async fn a_question_about_what_can_be_done_keeps_no_subjects() {
    let input = UnderstandingInput::new("what can I do?", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi")]).subject("total"));
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A question.", "units": [{
                "kind": "question", "words": {"from": 1, "to": 4}, "workflow": "trip",
                "basis": "general_domain_knowledge", "continues_previous": false
            }]}),
        )
        .answer(
            "u1/frame",
            json!({"topic": "capabilities", "record": "none", "subjects": ["total"]}),
        );
    let run = understand(script, &input).await;

    let [question] = run.understanding.questions.as_slice() else {
        panic!("one question expected: {:?}", run.understanding);
    };
    assert_eq!(question.topic, QuestionTopic::Capabilities);
    assert!(
        question.subjects.is_empty(),
        "a name named beside a question about what can be done is dropped"
    );
}
