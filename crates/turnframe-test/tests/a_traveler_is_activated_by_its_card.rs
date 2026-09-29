//! The sample traveler's fields are given in text, and once every field is settled a
//! card confirms its activation: the one step of a traveler that asks for a click. Its
//! reachable states keep every invariant.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::case::CaseRef;
use turnframe_core::command::ConfirmationPolicy;
use turnframe_core::flow::WorkflowDefinition;
use turnframe_core::ids::CaseRevision;
use turnframe_test::explore::{ExplorationLimits, explore};
use turnframe_test::workflows::PureWorkflow;
use turnframe_test::workflows::traveler::{
    SAMPLE_EMAIL, SAMPLE_LOYALTY_NUMBER, TravelerCommand, TravelerModel, TravelerPhase,
    TravelerStatus, TravelerWorkflow, active_traveler, incomplete_draft,
};

#[test]
fn a_traveler_is_activated_by_its_card() {
    let workflow = TravelerWorkflow::default();
    let draft = workflow
        .apply(
            Some(&incomplete_draft()),
            &TravelerCommand::ChangeEmail {
                value: SAMPLE_EMAIL.to_owned(),
            },
        )
        .unwrap()
        .state;
    let settled = workflow
        .apply(
            Some(&draft),
            &TravelerCommand::SetLoyaltyNumber {
                value: SAMPLE_LOYALTY_NUMBER.to_owned(),
            },
        )
        .unwrap()
        .state;

    assert_eq!(
        settled.status,
        TravelerStatus::Draft,
        "settling is not activating"
    );
    let view = workflow.project(
        CaseRef::new("traveler", "trav-1", CaseRevision(3)),
        Some(&settled),
    );
    assert_eq!(view.phase, TravelerPhase::AwaitingActivation);
    assert!(view.blocking_interaction.is_some(), "the activation card");
    let policy = workflow.command_policy(Some(&settled), &TravelerCommand::Activate);
    assert_eq!(policy.confirmation, ConfirmationPolicy::ExplicitClick);
}

#[test]
fn nothing_else_about_a_traveler_asks_for_a_card() {
    let workflow = TravelerWorkflow::default();
    for command in [
        TravelerCommand::ChangeEmail {
            value: SAMPLE_EMAIL.to_owned(),
        },
        TravelerCommand::Delete,
        TravelerCommand::Archive,
    ] {
        let policy = workflow.command_policy(Some(&active_traveler()), &command);
        assert_eq!(policy.confirmation, ConfirmationPolicy::None, "{command:?}");
    }
}

#[test]
fn its_reachable_states_keep_every_invariant() {
    let report = explore(
        &TravelerWorkflow::default(),
        &TravelerModel::default(),
        ExplorationLimits::generous(),
    );
    assert!(report.is_clean(), "{}", report.describe());
}
