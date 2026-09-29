//! A leg the traveler asked to keep is protected by the trip itself: a rebooking of it is
//! refused with the reason, in the turn that protected it and in every later one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::flow::WorkflowDefinition;
use turnframe_core::locale::Locale;
use turnframe_test::workflows::PureWorkflow;
use turnframe_test::workflows::trip::{TripCommand, TripWorkflow, rejection, with_offer};

#[test]
fn a_protected_leg_cannot_be_rebooked() {
    let workflow = TripWorkflow::default();
    let protected = workflow
        .apply(Some(&with_offer(1)), &TripCommand::ProtectLeg { leg: 1 })
        .unwrap()
        .state;

    let refused = workflow
        .validate_command(Some(&protected), &TripCommand::RequestRebooking { leg: 1 })
        .unwrap_err();

    assert_eq!(refused.code.as_str(), rejection::LEG_PROTECTED);
    let explanation = refused.explanation.expect("the refusal says why");
    assert!(
        explanation
            .resolve(&Locale::from("en"))
            .contains("kept as it is"),
        "{explanation:?}"
    );
    assert!(
        explanation
            .resolve(&Locale::from("it-IT"))
            .contains("resta com'è"),
        "{explanation:?}"
    );
}
