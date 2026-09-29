//! A trip that owes nothing more offers what comes next: another extra, or rebooking it.
//! One still collecting offers nothing, because what it owes is asked first.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::case::CaseRef;
use turnframe_core::flow::WorkflowDefinition;
use turnframe_core::ids::CaseRevision;
use turnframe_core::locale::Locale;
use turnframe_test::workflows::trip::{TripWorkflow, complete_case, incomplete_case};

#[test]
fn a_complete_trip_offers_its_next_steps() {
    let workflow = TripWorkflow::default();
    let case_ref = || CaseRef::new("trip", "trip-1", CaseRevision(3));
    let english = Locale::new("en");

    let complete = workflow.project(case_ref(), Some(&complete_case()));
    let steps: Vec<String> = workflow
        .next_steps(&complete)
        .iter()
        .map(|step| step.resolve(&english).to_owned())
        .collect();
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(steps[0].contains("extra"), "{steps:?}");
    assert!(steps[1].contains("Rebook"), "{steps:?}");

    let incomplete = workflow.project(case_ref(), Some(&incomplete_case()));
    assert!(workflow.next_steps(&incomplete).is_empty());
}
