//! Projection invariants (spec §8.4).
//!
//! [`check_view`] runs on a typed view with its definition; [`check_erased_view`]
//! runs on the erased form (which already carries phase ownership). Both
//! return **every** violation found rather than the first, so a map defect is
//! reported completely.

use std::collections::BTreeSet;

use crate::error::{InvariantViolation, InvariantViolationKind};
use crate::flow::{ErasedWorkflowView, PhaseOwnership, ViewOf, WorkflowDefinition};

/// Checks a typed view against the §8.4 invariants.
///
/// Rules:
/// * no outcome while obligations remain;
/// * a terminal phase has an outcome, a non-terminal phase has none;
/// * a user-owned phase has a blocking interaction requirement;
/// * no blocking interaction on terminal, system- or external-owned phases;
/// * the requirement in the blocking slot is flagged blocking;
/// * a requirement that already carries a payload describes an answerable card
///   (I6); a requirement without one is completed by
///   [`build_interaction`](crate::flow::WorkflowDefinition::build_interaction),
///   which validates it there;
/// * obligation identifiers are unique.
///
/// "Exactly one phase" is structural: `phase` is a single value by type.
pub fn check_view<W: WorkflowDefinition>(
    definition: &W,
    view: &ViewOf<W>,
) -> Result<(), Vec<InvariantViolation>> {
    let ownership = definition.phase_ownership(&view.phase);
    match view.erase(ownership) {
        Ok(erased) => check_erased_view(&erased),
        Err(_) => Err(vec![InvariantViolation {
            case_ref: view.case_ref.clone(),
            kind: InvariantViolationKind::UnserializableObligation,
        }]),
    }
}

/// Checks an erased view against the §8.4 invariants. See [`check_view`].
pub fn check_erased_view(view: &ErasedWorkflowView) -> Result<(), Vec<InvariantViolation>> {
    let mut violations = Vec::new();
    let mut push = |kind: InvariantViolationKind| {
        violations.push(InvariantViolation {
            case_ref: view.case_ref.clone(),
            kind,
        });
    };

    if view.outcome.is_some() && !view.obligations.is_empty() {
        push(InvariantViolationKind::OutcomeWithObligations {
            obligation_count: view.obligations.len(),
        });
    }
    match (view.phase_ownership, view.outcome.is_some()) {
        (PhaseOwnership::Terminal, false) => {
            push(InvariantViolationKind::TerminalPhaseWithoutOutcome)
        }
        (PhaseOwnership::User | PhaseOwnership::System | PhaseOwnership::External, true) => {
            push(InvariantViolationKind::OutcomeOnNonTerminalPhase);
        }
        _ => {}
    }
    match (view.phase_ownership, view.blocking_interaction.as_ref()) {
        (PhaseOwnership::User, None) => push(InvariantViolationKind::MissingBlockingInteraction),
        (PhaseOwnership::Terminal, Some(_)) => {
            push(InvariantViolationKind::BlockingInteractionOnTerminalPhase);
        }
        (PhaseOwnership::System | PhaseOwnership::External, Some(_)) => {
            push(InvariantViolationKind::BlockingInteractionOnNonUserPhase);
        }
        _ => {}
    }
    if view
        .blocking_interaction
        .as_ref()
        .is_some_and(|r| !r.blocking)
    {
        push(InvariantViolationKind::NonBlockingRequirementInBlockingSlot);
    }
    if let Some(requirement) = view.blocking_interaction.as_ref()
        && let Some(payload) = requirement.payload.as_ref()
        && let Err(error) = payload.validate_for(requirement.kind)
    {
        push(InvariantViolationKind::UnanswerableBlockingInteraction { error });
    }
    let mut seen = BTreeSet::new();
    let mut reported = BTreeSet::new();
    for obligation in &view.obligations {
        if !seen.insert(&obligation.id) && reported.insert(&obligation.id) {
            push(InvariantViolationKind::DuplicateObligation {
                obligation_id: obligation.id.as_str().to_owned(),
            });
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::CaseRef;
    use crate::flow::{ErasedObligation, InteractionRequirement, ObligationId};
    use crate::ids::{CaseRevision, WorkflowVersion};
    use crate::interaction::InteractionKind;

    fn erased(ownership: PhaseOwnership) -> ErasedWorkflowView {
        ErasedWorkflowView {
            case_ref: CaseRef::new("w", "c", CaseRevision(1)),
            workflow_version: WorkflowVersion::from("1"),
            phase: serde_json::json!("p"),
            phase_ownership: ownership,
            obligations: vec![],
            blocking_interaction: None,
            notices: vec![],
            outcome: None,
            state: vec![],
        }
    }

    fn kinds(result: Result<(), Vec<InvariantViolation>>) -> Vec<InvariantViolationKind> {
        result.unwrap_err().into_iter().map(|v| v.kind).collect()
    }

    #[test]
    fn valid_views_pass() {
        assert!(check_erased_view(&erased(PhaseOwnership::System)).is_ok());
        let mut user = erased(PhaseOwnership::User);
        user.blocking_interaction = Some(InteractionRequirement::blocking(
            "k",
            InteractionKind::Boolean,
        ));
        assert!(check_erased_view(&user).is_ok());
        let mut terminal = erased(PhaseOwnership::Terminal);
        terminal.outcome = Some(serde_json::json!("done"));
        assert!(check_erased_view(&terminal).is_ok());
    }

    #[test]
    fn user_phase_without_interaction() {
        assert_eq!(
            kinds(check_erased_view(&erased(PhaseOwnership::User))),
            vec![InvariantViolationKind::MissingBlockingInteraction]
        );
    }

    #[test]
    fn terminal_rules() {
        let mut v = erased(PhaseOwnership::Terminal);
        v.blocking_interaction = Some(InteractionRequirement::blocking(
            "k",
            InteractionKind::Boolean,
        ));
        assert_eq!(
            kinds(check_erased_view(&v)),
            vec![
                InvariantViolationKind::TerminalPhaseWithoutOutcome,
                InvariantViolationKind::BlockingInteractionOnTerminalPhase,
            ]
        );
        let mut v = erased(PhaseOwnership::Terminal);
        v.outcome = Some(serde_json::json!("done"));
        v.obligations = vec![ErasedObligation {
            id: ObligationId("\"a\"".into()),
            value: serde_json::json!("a"),
            sentence: None,
            act: None,
        }];
        assert_eq!(
            kinds(check_erased_view(&v)),
            vec![InvariantViolationKind::OutcomeWithObligations {
                obligation_count: 1
            }]
        );
    }

    #[test]
    fn outcome_on_non_terminal_and_non_user_blocking() {
        let mut v = erased(PhaseOwnership::External);
        v.outcome = Some(serde_json::json!("x"));
        v.blocking_interaction = Some(InteractionRequirement::blocking(
            "k",
            InteractionKind::Boolean,
        ));
        assert_eq!(
            kinds(check_erased_view(&v)),
            vec![
                InvariantViolationKind::OutcomeOnNonTerminalPhase,
                InvariantViolationKind::BlockingInteractionOnNonUserPhase,
            ]
        );
    }

    #[test]
    fn duplicate_obligations_and_non_blocking_slot() {
        let mut v = erased(PhaseOwnership::User);
        let mut req = InteractionRequirement::blocking("k", InteractionKind::Boolean);
        req.blocking = false;
        v.blocking_interaction = Some(req);
        let dup = ErasedObligation {
            id: ObligationId("\"a\"".into()),
            value: serde_json::json!("a"),
            sentence: None,
            act: None,
        };
        v.obligations = vec![dup.clone(), dup.clone(), dup];
        assert_eq!(
            kinds(check_erased_view(&v)),
            vec![
                InvariantViolationKind::NonBlockingRequirementInBlockingSlot,
                InvariantViolationKind::DuplicateObligation {
                    obligation_id: "\"a\"".into()
                },
            ]
        );
    }
}
