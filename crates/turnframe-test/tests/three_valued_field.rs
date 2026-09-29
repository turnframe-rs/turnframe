//! The three-valued field of the traveler sample: untouched, answered,
//! declined.
//!
//! A collection workflow whose fields are an `Option<String>` can only say
//! "filled in" or "empty", and empty covers two different situations. These
//! tests pin the difference that costs the most when it is missing: a declined
//! field must stop being an obligation, and the view must still know it was
//! declined rather than answered.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use turnframe_core::case::CaseRef;
use turnframe_core::error::DomainRejection;
use turnframe_core::flow::{PhaseOwnership, WorkflowDefinition, check_view};
use turnframe_core::ids::CaseRevision;
use turnframe_core::locale::Locale;
use turnframe_test::explore::{ExplorationLimits, explore, reachable_states};
use turnframe_test::workflows::traveler::{
    DeclineReason, FieldState, SAMPLE_LOYALTY_NUMBER, TravelerCommand, TravelerModel,
    TravelerObligation, TravelerPhase, TravelerState, TravelerWorkflow, awaiting_activation,
    declined_loyalty_number, incomplete_draft, rejection, validate,
};

fn case(revision: u64) -> CaseRef {
    CaseRef::new("traveler", "c-1", CaseRevision(revision))
}

fn view_of(state: &TravelerState) -> turnframe_core::flow::ViewOf<TravelerWorkflow> {
    TravelerWorkflow::new()
        .with_cards()
        .project(case(3), Some(state))
}

/// A declined field is settled, so the assistant stops asking for it.
///
/// Without the third state this is the choice between two defects: an
/// obligation that never closes and asks the same question every turn, or an
/// obligation dropped by a projector that can no longer say why.
#[test]
fn a_declined_field_closes_its_obligation() {
    let untouched = incomplete_draft();
    assert!(
        untouched
            .open_obligations()
            .contains(&TravelerObligation::SetLoyaltyNumber),
        "an unanswered field is an open obligation"
    );

    for reason in DeclineReason::ALL {
        let declined = TravelerState {
            loyalty_number: FieldState::declined(reason),
            ..incomplete_draft()
        };
        assert!(
            !declined
                .open_obligations()
                .contains(&TravelerObligation::SetLoyaltyNumber),
            "{reason:?} left the obligation open: the assistant would ask for ever"
        );
        assert!(
            !view_of(&declined)
                .obligations
                .contains(&TravelerObligation::SetLoyaltyNumber),
            "{reason:?} left the obligation in the projected view"
        );
    }
}

/// Completeness is answered from the third state, not from emptiness.
#[test]
fn a_declined_field_counts_as_settled_for_completeness() {
    let mut draft = awaiting_activation();
    draft.loyalty_number = FieldState::Untouched;
    assert!(!draft.is_complete(), "an unanswered field is not settled");
    assert_eq!(
        validate(Some(&draft), &TravelerCommand::Activate)
            .unwrap_err()
            .code
            .as_str(),
        rejection::INCOMPLETE
    );

    let declined = declined_loyalty_number(DeclineReason::NotApplicable);
    assert!(declined.is_complete(), "a declined field is settled");
    assert!(validate(Some(&declined), &TravelerCommand::Activate).is_ok());
    assert_eq!(
        TravelerWorkflow::phase_of(&declined),
        TravelerPhase::AwaitingActivation,
        "a traveler with nothing left to answer is waiting for the activation card"
    );
}

/// Two states with the same phase and the same (empty) obligations are still
/// told apart by the view, because the reason survives as a notice.
#[test]
fn the_view_still_distinguishes_a_declined_field_from_an_answered_one() {
    let answered = view_of(&awaiting_activation());
    assert!(
        answered
            .notices
            .iter()
            .all(|notice| !notice.code.starts_with("traveler.loyalty_number_declined")),
        "an answered field must not claim it was declined"
    );

    for reason in DeclineReason::ALL {
        let declined = view_of(&declined_loyalty_number(reason));
        // The two views agree on everything the obligation model can see ...
        assert_eq!(declined.phase, answered.phase);
        assert_eq!(declined.obligations, answered.obligations);
        // ... and disagree exactly where the reason lives.
        let notice = declined
            .notices
            .iter()
            .find(|notice| notice.code.starts_with("traveler.loyalty_number_declined."))
            .unwrap_or_else(|| panic!("{reason:?} left no trace in the view"));
        assert_eq!(
            notice.code,
            format!("traveler.loyalty_number_declined.{}", reason.code()),
            "the reason must be readable from the code, not only from the prose"
        );
        assert!(!notice.text.resolve(&Locale::from("it")).is_empty());
        assert_ne!(
            notice.text.resolve(&Locale::from("en")),
            notice.text.resolve(&Locale::from("it")),
            "the notice is translated, so a reader is not shown English by accident"
        );
        // The view is still a legal projection.
        check_view(&TravelerWorkflow::new().with_cards(), &declined).expect("the view holds §8.4");
    }
}

/// Only some reasons are worth raising again, and the domain says which.
#[test]
fn only_a_reason_that_can_change_is_worth_asking_again() {
    assert!(DeclineReason::Unknown.worth_asking_again());
    assert!(!DeclineReason::NotApplicable.worth_asking_again());
    assert!(!DeclineReason::Withheld.worth_asking_again());

    // A number that will never exist and a number deliberately withheld are
    // both settled for ever; a number the user has not looked up yet is not.
    let recoverable = declined_loyalty_number(DeclineReason::Unknown);
    assert!(
        validate(
            Some(&recoverable),
            &TravelerCommand::SetLoyaltyNumber {
                value: SAMPLE_LOYALTY_NUMBER.to_owned(),
            }
        )
        .is_ok(),
        "a declined field can still be answered once the user knows the value"
    );
}

/// Declining is how an open question is closed, not how an answer is erased.
#[test]
fn declining_a_field_the_user_already_answered_is_refused() {
    let answered = awaiting_activation();
    let refused: DomainRejection = validate(
        Some(&answered),
        &TravelerCommand::DeclineLoyaltyNumber {
            reason: DeclineReason::Withheld,
        },
    )
    .unwrap_err();
    assert_eq!(
        refused.code.as_str(),
        rejection::LOYALTY_NUMBER_ALREADY_ANSWERED
    );
}

/// The larger state space the third value opens is still fully explored, and
/// still satisfies every projection invariant.
#[test]
fn the_state_space_with_a_declined_field_stays_clean() {
    let report = explore(
        &TravelerWorkflow::new().with_cards(),
        &TravelerModel::of(TravelerWorkflow::new().with_cards()),
        ExplorationLimits::generous(),
    );
    assert!(report.is_clean(), "{}", report.describe());
    assert!(!report.truncated, "{}", report.describe());

    let states: Vec<TravelerState> = reachable_states(
        &TravelerModel::of(TravelerWorkflow::new().with_cards()),
        ExplorationLimits::generous(),
    )
    .into_iter()
    .flatten()
    .collect();

    for reason in [DeclineReason::NotApplicable, DeclineReason::Unknown] {
        assert!(
            states
                .iter()
                .any(|state| state.loyalty_number.decline_reason() == Some(reason)),
            "exploration never reached a traveler whose loyalty number was declined as {reason:?}"
        );
    }
    assert!(
        states
            .iter()
            .any(|state| state.loyalty_number.is_answered() && state.is_complete()),
        "exploration never reached a traveler who answered"
    );
    assert!(
        states.iter().any(|state| {
            state.loyalty_number.decline_reason().is_some()
                && TravelerWorkflow::phase_of(state) == TravelerPhase::Active
        }),
        "a traveler activated after declining must be reachable: that is the whole \
         point of a decline settling the field"
    );
    assert_eq!(
        TravelerWorkflow::new()
            .with_cards()
            .phase_ownership(&TravelerPhase::AwaitingActivation),
        PhaseOwnership::User
    );
}
