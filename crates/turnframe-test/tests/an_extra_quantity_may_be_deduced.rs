//! «A checked bag at 40 euros» is one bag: the sample trip lets an extra's quantity be
//! deduced from the words, where every other value of an extra must be said.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::case::CaseRef;
use turnframe_core::flow::WorkflowDefinition;
use turnframe_core::ids::CaseRevision;
use turnframe_core::operation::ArgumentSource;
use turnframe_test::workflows::trip::{TripWorkflow, incomplete_case, operations};

#[test]
fn an_extra_quantity_may_be_deduced() {
    let workflow = TripWorkflow::default();
    let view = workflow.project(
        CaseRef::new("trip", "trip-1", CaseRevision(3)),
        Some(&incomplete_case()),
    );
    let offered = workflow.operations(&view);
    let add_extra = offered
        .iter()
        .find(|spec| spec.key.as_str() == operations::ADD_EXTRA)
        .unwrap();
    let source = |name: &str| {
        add_extra
            .arguments
            .iter()
            .find(|argument| argument.name == name)
            .map(|argument| argument.source.clone())
            .unwrap()
    };
    assert_eq!(source("quantity"), ArgumentSource::Inferred);
    assert_ne!(source("unit_price"), ArgumentSource::Inferred);
    assert_ne!(source("description"), ArgumentSource::Inferred);
}
