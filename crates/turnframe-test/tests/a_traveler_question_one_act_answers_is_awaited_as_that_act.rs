//! Asked for the traveler's full name or email, a bare answer is that field: the question
//! carries the act that sets it. The loyalty number is answered by giving it or by
//! declining it, so its question carries no act and the answer is routed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::flow::{ObligationAct, WorkflowDefinition};
use turnframe_test::workflows::traveler::{
    TravelerObligation, TravelerWorkflow, incomplete_draft, operations,
};

fn act(obligation: TravelerObligation) -> Option<ObligationAct> {
    TravelerWorkflow::default().obligation_act(Some(&incomplete_draft()), &obligation)
}

#[test]
fn a_traveler_question_one_act_answers_is_awaited_as_that_act() {
    assert_eq!(
        act(TravelerObligation::SetName),
        Some(ObligationAct::new(operations::SET_NAME, ["value"]))
    );
    assert_eq!(
        act(TravelerObligation::SetEmail),
        Some(ObligationAct::new(operations::CHANGE_EMAIL, ["value"]))
    );
}

#[test]
fn the_loyalty_number_question_is_answered_by_either_act() {
    assert_eq!(act(TravelerObligation::SetLoyaltyNumber), None);
}
