//! The divergence vocabulary: one test per kind, and one for the asymmetry.
//!
//! The comparison exists so two adopters describe the same disagreement the
//! same way, so these tests are written the way a report is read: build the two
//! sides, compare them, and assert both *what* differed and *whose problem it
//! is*. The last two are the ones that keep the vocabulary honest — a refusal
//! on an ambiguous target that the other path performed is a finding against
//! the other path, and it must not be counted twice.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::case::CaseKey;
use turnframe_core::ids::{OperationKey, TurnId};
use turnframe_core::interaction::InteractionKind;
use turnframe_core::response::ClaimClass;
use turnframe_core::understanding::ActTarget;
use turnframe_runtime::divergence::{
    ActSummary, Attribution, ClarificationSummary, Divergence, MutationSummary, RefusalReason,
    RefusedMutation, Side, TurnSummary, compare,
};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn trip(case_id: &str) -> CaseKey {
    CaseKey::new("trip", case_id)
}

fn set_subject() -> OperationKey {
    OperationKey::from(operations::SET_NAME)
}

fn mutation(case_id: &str) -> MutationSummary {
    MutationSummary {
        operation: set_subject(),
        case: Some(trip(case_id)),
        command_ref: None,
    }
}

fn act(case_id: Option<&str>) -> ActSummary {
    ActSummary {
        kind: "apply_operation".to_owned(),
        operation: Some(set_subject()),
        case: case_id.map(trip),
    }
}

fn clarification(case_id: &str) -> ClarificationSummary {
    ClarificationSummary {
        key: "select_target".to_owned(),
        kind: InteractionKind::SelectTarget,
        case: trip(case_id),
    }
}

/// Two sides that did the same thing produce no findings at all.
#[test]
fn agreement_produces_no_findings() {
    let side = TurnSummary {
        cases: vec![trip("trip-1")],
        acts: vec![act(Some("trip-1"))],
        mutations: vec![mutation("trip-1")],
        claims: vec![ClaimClass::Update],
        evidenced_claims: vec![ClaimClass::Update],
        ..TurnSummary::new()
    };

    let report = compare(&side, &side);

    assert!(report.agreed(), "{report}");
    assert_eq!(report.to_string(), "no divergence");
}

/// Kind 1: the two sides addressed different records.
#[test]
fn a_different_case_selection_is_reported_as_such() {
    let shadow = TurnSummary {
        cases: vec![trip("trip-1")],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        cases: vec![trip("trip-2")],
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    let finding = report.of_kind("case_selection").next().unwrap();
    assert_eq!(finding.attribution, Attribution::Undetermined);
    let Divergence::CaseSelection {
        shadow: ours,
        authoritative: theirs,
    } = &finding.divergence
    else {
        panic!("the finding names both selections");
    };
    assert_eq!(ours, &vec![trip("trip-1")]);
    assert_eq!(theirs, &vec![trip("trip-2")]);
}

/// Kind 2: the two sides read different acts out of the same message.
#[test]
fn differently_extracted_acts_are_reported_as_such() {
    let shadow = TurnSummary {
        acts: vec![act(Some("trip-1"))],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        acts: vec![act(Some("trip-1")), act(Some("trip-1"))],
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    let finding = report.of_kind("acts_extracted").next().unwrap();
    assert_eq!(finding.attribution, Attribution::Undetermined);
    let Divergence::ActsExtracted {
        shadow: ours,
        authoritative: theirs,
    } = &finding.divergence
    else {
        panic!("the finding names both extractions");
    };
    assert_eq!(ours.len(), 1);
    assert_eq!(theirs.len(), 2);
}

/// Kind 3: the two sides would run different mutations, for a reason the
/// comparison cannot attribute on its own.
#[test]
fn different_mutations_are_reported_without_blaming_either_side() {
    let shadow = TurnSummary {
        mutations: vec![mutation("trip-1")],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        mutations: vec![mutation("trip-2")],
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    let finding = report.of_kind("mutations").next().unwrap();
    assert_eq!(finding.attribution, Attribution::Undetermined);
    assert!(
        !report.any_against_shadow(),
        "a difference nobody can attribute is not a regression"
    );
}

/// Kind 4: one side asked the user something while the other acted.
#[test]
fn asking_while_the_other_side_acts_is_its_own_kind() {
    let shadow = TurnSummary {
        clarifications: vec![clarification("trip-1")],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        mutations: vec![mutation("trip-1")],
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    let finding = report
        .of_kind("clarification_versus_action")
        .next()
        .unwrap();
    let Divergence::ClarificationVersusAction {
        asked,
        clarifications,
        mutations,
    } = &finding.divergence
    else {
        panic!("the finding names who asked and what the other did");
    };
    assert_eq!(*asked, Side::Shadow);
    assert_eq!(clarifications.len(), 1);
    assert_eq!(mutations.len(), 1);
    assert_eq!(
        finding.attribution,
        Attribution::Undetermined,
        "asking is only a finding against the other path when the refusal was about which record was meant"
    );
}

/// Kind 4, the other way round: the existing path asked and this library
/// acted, which the comparison will not attribute either.
#[test]
fn the_other_side_asking_is_reported_from_its_own_point_of_view() {
    let shadow = TurnSummary {
        mutations: vec![mutation("trip-1")],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        clarifications: vec![clarification("trip-1")],
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    let finding = report
        .of_kind("clarification_versus_action")
        .next()
        .unwrap();
    let Divergence::ClarificationVersusAction { asked, .. } = &finding.divergence else {
        panic!("the finding names who asked");
    };
    assert_eq!(*asked, Side::Authoritative);
    assert_eq!(finding.attribution, Attribution::Undetermined);
}

/// Kind 5: one side stated an outcome the other has no committed event for.
#[test]
fn a_claim_with_no_event_behind_it_is_reported_against_its_claimant() {
    let shadow = TurnSummary {
        claims: vec![ClaimClass::Submission],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        // The existing path committed nothing, so nothing backs "sent".
        evidenced_claims: Vec::new(),
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    let finding = report.of_kind("claim_without_event").next().unwrap();
    assert_eq!(finding.attribution, Attribution::Shadow);
    let Divergence::ClaimWithoutEvent { claimant, class } = &finding.divergence else {
        panic!("the finding names the claimant and the class");
    };
    assert_eq!(*claimant, Side::Shadow);
    assert_eq!(*class, ClaimClass::Submission);

    // And the same claim backed by an event of the other side is not a finding.
    let backed = TurnSummary {
        evidenced_claims: vec![ClaimClass::Submission],
        ..TurnSummary::new()
    };
    assert!(compare(&shadow, &backed).agreed());
}

/// Kind 6, the asymmetry: this library refused a mutation because it could not
/// tell which record was meant, and the existing path performed it anyway.
///
/// That is a finding **against the existing path**, and a comparison that could
/// not say so would be read as though every difference were a regression.
#[test]
fn a_mutation_on_an_ambiguous_target_is_a_finding_against_the_old_path() {
    let shadow = TurnSummary {
        cases: vec![trip("trip-1"), trip("trip-2")],
        acts: vec![act(None)],
        clarifications: vec![clarification("trip-1")],
        refusals: vec![RefusedMutation {
            operation: Some(set_subject()),
            reason: RefusalReason::AmbiguousTarget,
        }],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        cases: vec![trip("trip-1"), trip("trip-2")],
        acts: vec![act(None)],
        mutations: vec![mutation("trip-1")],
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    let finding = report
        .of_kind("refused_unresolved_target_that_ran")
        .next()
        .unwrap();
    assert_eq!(finding.attribution, Attribution::Authoritative);
    let Divergence::RefusedUnresolvedTargetThatRan { performed, reason } = &finding.divergence
    else {
        panic!("the finding names the mutation and why we would not run it");
    };
    assert_eq!(performed.case, Some(trip("trip-1")));
    assert_eq!(*reason, RefusalReason::AmbiguousTarget);

    assert!(
        !report.any_against_shadow(),
        "nothing here counts against this library: {report}"
    );
    assert_eq!(
        report.against(Side::Authoritative).count(),
        2,
        "the refusal and the clarification-versus-action both land on the old path"
    );
}

/// The explained mutation is not also reported as a plain mutation difference:
/// one disagreement, one finding.
#[test]
fn an_explained_refusal_is_not_counted_twice() {
    let shadow = TurnSummary {
        refusals: vec![RefusedMutation {
            operation: Some(set_subject()),
            reason: RefusalReason::AmbiguousTarget,
        }],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        mutations: vec![mutation("trip-1")],
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    assert_eq!(
        report.of_kind("refused_unresolved_target_that_ran").count(),
        1
    );
    assert_eq!(
        report.of_kind("mutations").count(),
        0,
        "the difference is already explained, and explaining it twice inflates the report"
    );
}

/// A refusal that was not about which record was meant does not shift the
/// blame: a confirmation this library required and the old path skipped is a
/// plain difference, for a person to judge.
#[test]
fn a_confirmation_refusal_does_not_attribute_the_difference() {
    let shadow = TurnSummary {
        refusals: vec![RefusedMutation {
            operation: Some(set_subject()),
            reason: RefusalReason::ConfirmationRequired,
        }],
        ..TurnSummary::new()
    };
    let authoritative = TurnSummary {
        mutations: vec![mutation("trip-1")],
        ..TurnSummary::new()
    };

    let report = compare(&shadow, &authoritative);

    assert_eq!(
        report.of_kind("refused_unresolved_target_that_ran").count(),
        0
    );
    let finding = report.of_kind("mutations").next().unwrap();
    assert_eq!(finding.attribution, Attribution::Undetermined);
}

/// The summary this library contributes is read off a real planned turn, not
/// written by hand: a plan that would execute one command describes one
/// mutation, on the case it resolved.
#[tokio::test]
async fn a_planned_turn_summarizes_itself_in_the_shared_vocabulary() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip_panics_on_write()
        .understands(understanding)
        .build()
        .await;

    let summary = harness
        .orchestrator
        .plan_turn(harness.turn(turn_id, text))
        .await
        .unwrap()
        .summary();

    assert_eq!(summary.cases, vec![trip("trip-1")]);
    assert_eq!(summary.acts.len(), 1);
    assert_eq!(summary.acts[0].kind, "apply_operation");
    assert_eq!(summary.mutations.len(), 1);
    assert_eq!(summary.mutations[0].operation, set_subject());
    assert_eq!(summary.mutations[0].case, Some(trip("trip-1")));
    assert!(
        summary.evidenced_claims.is_empty(),
        "a planned turn commits nothing, so it can cite nothing"
    );
    assert!(summary.refusals.is_empty());

    // An existing path that did the same thing agrees with it, once the
    // command reference — which only this side has — is left out of the match.
    let authoritative = TurnSummary {
        cases: summary.cases.clone(),
        acts: summary.acts.clone(),
        mutations: vec![mutation("trip-1")],
        claims: summary.claims.clone(),
        evidenced_claims: summary.claims.clone(),
        ..TurnSummary::new()
    };
    assert!(compare(&summary, &authoritative).agreed());
}

/// A planned turn that had to ask which record was meant summarizes as a
/// refusal on an ambiguous target, which is what makes the asymmetry above
/// reachable from a real run rather than only from a hand-built summary.
#[tokio::test]
async fn an_ambiguous_planned_turn_summarizes_as_a_refusal() {
    let turn_id = turn_one();
    let text = "Set the name on the Ferri trip to Lisbon";
    let understanding = UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_NAME,
            ActTarget::Ambiguous {
                candidates: vec![
                    token_for(turn_id, "trip", "trip-1"),
                    token_for(turn_id, "trip", "trip-2"),
                ],
            },
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .trip_panics_on_write()
        .understands(understanding)
        .build()
        .await;

    let summary = harness
        .orchestrator
        .plan_turn(harness.turn(turn_id, text))
        .await
        .unwrap()
        .summary();

    assert!(summary.mutations.is_empty());
    assert_eq!(summary.clarifications.len(), 1);
    assert_eq!(
        summary.refusals,
        vec![RefusedMutation {
            operation: Some(set_subject()),
            reason: RefusalReason::AmbiguousTarget,
        }]
    );

    // The old path picked one. The report says whose problem that is.
    let authoritative = TurnSummary {
        cases: summary.cases.clone(),
        acts: summary.acts.clone(),
        mutations: vec![mutation("trip-1")],
        ..TurnSummary::new()
    };
    let report = compare(&summary, &authoritative);
    assert_eq!(
        report.of_kind("refused_unresolved_target_that_ran").count(),
        1
    );
    assert!(!report.any_against_shadow(), "{report}");
}
