//! A request nothing on offer does, about a workflow whose records in view offer nothing now,
//! is no misreading: nothing can be done on them, so it asks where they stand, and the reply
//! says why. A record that still offers something leaves the request not understood.
#![allow(clippy::panic)]

mod support;

use serde_json::json;
use support::{OPEN, one_request, script, today, understand};
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::understanding::QuestionTopic;
use turnframe_understand::{RecordBrief, UnderstandingInput, WorkflowBrief};

const MESSAGE: &str = "please confirm the rebooking";

fn turn(offering: &[&str]) -> UnderstandingInput {
    let open = OperationSpec::new(OPEN)
        .summary("Open a disruption case.")
        .target(TargetPolicy::NewCaseOnly)
        .mutating();
    let settle = OperationSpec::new("trip.settle")
        .summary("Settle the trip.")
        .target(TargetPolicy::RequiresExistingCase)
        .mutating();
    let record =
        RecordBrief::new("tok-trip-1", "Trip 1", "dispatching").offering(offering.iter().copied());
    UnderstandingInput::new(MESSAGE, "en-GB", today()).with_workflow(
        WorkflowBrief::new("trip")
            .on_new_case(OPEN)
            .operation(open)
            .operation(settle)
            .record(record),
    )
}

#[tokio::test]
async fn a_request_about_records_that_offer_nothing_now_asks_where_they_stand() {
    // [1]please [2]confirm [3]the [4]rebooking
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", json!({"operations": ["none"]}))
        .answer(
            "u1/frame",
            json!({"topic": "capabilities", "record": "none"}),
        );
    let run = understand(script, &turn(&[])).await;

    let understanding = &run.understanding;
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
    let [question] = understanding.questions.as_slice() else {
        panic!("one question: {understanding:?}");
    };
    assert_eq!(
        question.record.as_ref().map(|t| t.as_str()),
        Some("tok-trip-1")
    );
    assert_eq!(question.topic, QuestionTopic::RecordState);
}

#[tokio::test]
async fn a_record_still_offering_something_leaves_it_not_understood() {
    // [1]please [2]confirm [3]the [4]rebooking
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", json!({"operations": ["none"]}));
    let run = understand(script, &turn(&["trip.settle"])).await;

    let understanding = &run.understanding;
    assert!(understanding.questions.is_empty(), "{understanding:?}");
    assert_eq!(understanding.not_understood.len(), 1, "{understanding:?}");
}

#[tokio::test]
async fn so_does_one_whose_only_reading_was_not_asked_for() {
    // [1]please [2]confirm [3]the [4]rebooking
    let not_asked = json!({"reason": "No new record was asked for.", "arguments": {},
                           "overall": "not_requested"});
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", json!({"operations": [OPEN]}))
        .answer("u1/verify", not_asked.clone())
        .answer("u1/verify.after_repair", not_asked)
        .answer(
            "u1/frame",
            json!({"topic": "record_state", "record": "none"}),
        );
    let run = understand(script, &turn(&[])).await;

    let understanding = &run.understanding;
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    let [question] = understanding.questions.as_slice() else {
        panic!("one question: {understanding:?}");
    };
    assert_eq!(
        question.record.as_ref().map(|t| t.as_str()),
        Some("tok-trip-1")
    );
}
