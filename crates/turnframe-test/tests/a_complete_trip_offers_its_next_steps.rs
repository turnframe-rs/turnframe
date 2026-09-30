//! A trip that owes nothing more offers what comes next, as operations: another extra always,
//! and the rebooking of the quoted leg once a quote is in. One still collecting offers nothing:
//! what it owes is asked first.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::case::CaseRef;
use turnframe_core::flow::WorkflowDefinition;
use turnframe_core::ids::CaseRevision;
use turnframe_test::workflows::trip::{
    TripWorkflow, complete_case, incomplete_case, operations, with_offer,
};

fn steps(state: &turnframe_test::workflows::trip::TripState) -> Vec<(String, serde_json::Value)> {
    let workflow = TripWorkflow::default();
    let case_ref = CaseRef::new("trip", "trip-1", CaseRevision(3));
    let view = workflow.project(case_ref, Some(state));
    workflow
        .next_steps(Some(state), &view)
        .into_iter()
        .map(|step| {
            (
                step.operation.to_string(),
                serde_json::Value::Object(step.arguments),
            )
        })
        .collect()
}

#[test]
fn a_complete_trip_offers_its_next_steps() {
    assert_eq!(
        steps(&complete_case()),
        [(operations::ADD_EXTRA.to_owned(), serde_json::json!({}))]
    );
    assert_eq!(
        steps(&with_offer(1)),
        [
            (operations::ADD_EXTRA.to_owned(), serde_json::json!({})),
            (
                operations::REQUEST_REBOOKING.to_owned(),
                serde_json::json!({ "leg": 1 })
            ),
        ]
    );
    assert!(steps(&incomplete_case()).is_empty());
}
