//! The reducer, read as a sequence of things a user actually says.
//!
//! Every test here is a turn a person could plausibly type, and an assertion
//! about what §13 says must happen to it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fixture, TripState, click, confirmed_origin, low_risk_card, op, turn};
use serde_json::json;
use turnframe_core::error::ReductionError;
use turnframe_core::ids::{OperationKey, TargetToken, WorkflowKey};
use turnframe_core::plan::AnswerBasis;
use turnframe_core::reduce::{PlannedActResult, ReductionPlan, SourcePolicy};
use turnframe_core::understanding::{ActAction, ActId, ActTarget, ConstraintKind, Understanding};
use turnframe_runtime::reduce::{notice, rejection};
use turnframe_runtime::resume::{card_act, card_act_id};
use turnframe_test::providers::UnderstandingBuilder;

fn draft() -> Fixture {
    Fixture::new().case("i1", 3, "Luca Ferri", TripState::with_name("Lisbon"))
}

fn code(result: &PlannedActResult) -> &str {
    match result {
        PlannedActResult::Rejected { rejection } => rejection.code.as_str(),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn command_payloads(plan: &ReductionPlan) -> Vec<serde_json::Value> {
    plan.batches
        .iter()
        .flat_map(|batch| batch.envelopes.iter().map(|e| e.command.clone()))
        .collect()
}

fn ids(understanding: &Understanding) -> Vec<ActId> {
    understanding.acts.iter().map(|act| act.id).collect()
}

#[test]
fn independent_fields_on_one_case_commit_together() {
    let text = "Set the name to Porto and the travel date to 2026-10-01";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Porto"}),
            "Set the name to Porto",
        )
        .apply(
            op::SET_DATE,
            fixture.token("i1"),
            json!({"value": "2026-10-01"}),
            "the travel date to 2026-10-01",
        )
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();

    assert_eq!(reduced.batches.len(), 1, "one case, one batch (§13.4)");
    let batch = &reduced.batches[0];
    assert_eq!(batch.len(), 2);
    assert!(batch.is_single_case());
    assert_eq!(
        batch.scope,
        turnframe_core::command::AtomicityScope::PerCase
    );
    assert!(
        reduced
            .acts
            .iter()
            .all(|a| matches!(a.result, PlannedActResult::ReadyToExecute { .. }))
    );
    assert_eq!(reduced.policy_decisions.len(), 2);
    assert!(reduced.policy_decisions.iter().all(|d| d.allowed));
    // Two commands on one case share the batch but never the identity.
    let ids: Vec<_> = batch.envelopes.iter().map(|e| e.command_id).collect();
    assert_ne!(ids[0], ids[1]);
    let keys: Vec<_> = batch
        .envelopes
        .iter()
        .map(|e| e.idempotency_key.clone())
        .collect();
    assert_ne!(keys[0], keys[1]);
}

#[test]
fn an_action_and_a_question_are_both_answered() {
    let text = "Rebook the Ferri trip, and does that change the filing deadline?";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::REBOOK,
            fixture.token("i1"),
            json!(null),
            "Rebook the Ferri trip",
        )
        .ask_about(
            AnswerBasis::ProposedState,
            None,
            &["trip.date"],
            "does that change the filing deadline?",
        )
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();

    // The action needs a click; the question is answered anyway (rule 6).
    let spec = match &reduced.acts[0].result {
        PlannedActResult::AwaitingConfirmation { interaction_spec } => interaction_spec,
        other => panic!("expected a confirmation, got {other:?}"),
    };
    assert_eq!(spec.validate(), Ok(()));
    assert_eq!(
        reduced.pre_execution_interactions.len(),
        1,
        "the card must exist before anything runs"
    );
    assert!(
        reduced.has_no_effects(),
        "an unconfirmed rebooking executes nothing"
    );

    assert_eq!(reduced.answer_tasks.len(), 1);
    let task = &reduced.answer_tasks[0];
    assert_eq!(task.basis, AnswerBasis::ProposedState);
    assert!(task.proposed_diff_ref.is_some());
    assert_eq!(task.required_sources, SourcePolicy::AuthoritativeOnly);
    assert_eq!(task.case_refs, vec![common::case("i1", 3)]);
}

#[test]
fn a_card_click_binds_harder_than_the_text_next_to_it() {
    let text = "Yes, go ahead and rebook it";
    let fixture = draft()
        .active(low_risk_card(common::case("i1", 3), &["yes", "no"]))
        // The card put no act of its own into this understanding.
        .confirmed_origin(confirmed_origin(), card_act_id());
    let mut input = turn(text);
    input.interaction_response = Some(click("yes", 3));
    let understanding = UnderstandingBuilder::of(text)
        // The text reads as an answer to the same card, and as a rebooking.
        .answer_card("yes", "Yes")
        .apply(
            op::REBOOK,
            fixture.token("i1"),
            json!(null),
            "go ahead and rebook it",
        )
        .build()
        .unwrap();
    let reduced = fixture.reduce(&input, &understanding).unwrap();

    assert!(
        matches!(
            reduced.acts[0].result,
            PlannedActResult::AwaitingConfirmation { .. }
        ),
        "a click authorizes what its card asked about, not a rebooking read \
         beside it: {:?}",
        reduced.acts[0].result
    );
    assert!(
        reduced.has_no_effects(),
        "nothing runs on the strength of another card's click"
    );
}

#[test]
fn the_cards_own_act_carries_the_click() {
    let fixture = draft()
        .active(low_risk_card(common::case("i1", 3), &["yes", "no"]))
        .confirmed_origin(confirmed_origin(), card_act_id());
    let mut input = turn("");
    input.text = None;
    input.interaction_response = Some(click("yes", 3));
    let understanding = Understanding {
        acts: vec![card_act(
            ActAction::Apply {
                operation: OperationKey::from(op::REBOOK),
            },
            fixture.target("i1"),
        )],
        ..Understanding::default()
    };
    let reduced = fixture.reduce(&input, &understanding).unwrap();

    assert!(
        matches!(reduced.acts[0].result, PlannedActResult::ReadyToExecute { ref command_refs } if !command_refs.is_empty()),
        "the card's own act runs on its click: {:?}",
        reduced.acts[0].result
    );
    let origin = &reduced.batches[0].envelopes[0].origin;
    assert!(
        origin.is_trusted(),
        "an externally regulated command needs a trusted origin (I12)"
    );
    assert_eq!(origin, &confirmed_origin());
}

#[test]
fn ambiguity_raises_a_selection_card_and_leaves_the_other_act_alone() {
    let text = "Set the Ferri travel date to 2026-10-01 and the name of the other one to Porto";
    let fixture = Fixture::new()
        .case("i1", 3, "Ferri", TripState::with_name("One"))
        .case("i2", 1, "ferri", TripState::with_name("Two"))
        .case("i3", 1, "Bianchi", TripState::with_name("Three"));
    let understanding = UnderstandingBuilder::of(text)
        .apply_to(
            op::SET_DATE,
            ActTarget::Ambiguous {
                candidates: vec![fixture.token("i1"), fixture.token("i2")],
            },
            json!({"value": "2026-10-01"}),
            "Set the Ferri travel date to 2026-10-01",
        )
        .apply(
            op::SET_NAME,
            fixture.token("i3"),
            json!({"value": "Porto"}),
            "the name of the other one to Porto",
        )
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();

    let spec = match &reduced.acts[0].result {
        PlannedActResult::NeedsClarification { interaction_spec } => interaction_spec,
        other => panic!("expected a selection card, got {other:?}"),
    };
    assert_eq!(
        spec.kind,
        turnframe_core::interaction::InteractionKind::SelectTarget
    );
    assert_eq!(spec.validate(), Ok(()));
    assert_eq!(
        spec.payload.options.len(),
        3,
        "both candidates, plus a way to say neither"
    );
    assert!(matches!(
        reduced.acts[1].result,
        PlannedActResult::ReadyToExecute { .. }
    ));
    assert_eq!(
        command_payloads(&reduced),
        vec![json!({"set_name": {"value": "Porto"}})],
        "the unambiguous act keeps going (§12.3 point 3)"
    );
    assert!(
        reduced
            .notices
            .iter()
            .any(|n| n.code == notice::PARTIAL_RESULT),
        "the partial result is made explicit (§12.3 point 4)"
    );
}

/// An operation only a card may run is refused when an act names it and no card
/// answer put it there, on that act alone.
#[test]
fn an_operation_only_a_card_may_run_is_refused_when_no_card_names_it() {
    let text = "Set the name to Porto and do the card thing";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Porto"}),
            "Set the name to Porto",
        )
        .apply(
            op::CARD_ONLY,
            fixture.token("i1"),
            json!(null),
            "do the card thing",
        )
        .build()
        .unwrap();
    let reduced = fixture
        .reduce(&turn(text), &understanding)
        .expect("one inadmissible act does not take the turn with it");
    assert_eq!(
        command_payloads(&reduced),
        vec![json!({"set_name": {"value": "Porto"}})],
        "the instruction beside it still runs"
    );
    let PlannedActResult::Rejected { rejection } = &reduced.acts[1].result else {
        panic!(
            "the act nobody may propose is refused: {:?}",
            reduced.acts[1].result
        );
    };
    assert_eq!(rejection.code.as_str(), rejection::NOT_PROPOSABLE);
}

/// An operation nobody offers, or arguments its schema refuses, cost one act:
/// the rest of the turn stands, and the refusal has its own code.
#[test]
fn a_malformed_act_is_refused_and_the_rest_of_the_turn_stands() {
    let text = "Set the name to Porto and frobnicate the trip";
    let fixture = draft();
    let with = |operation: &str, arguments: serde_json::Value| {
        UnderstandingBuilder::of(text)
            .apply(
                op::SET_NAME,
                fixture.token("i1"),
                json!({"value": "Porto"}),
                "Set the name to Porto",
            )
            .apply(
                operation,
                fixture.token("i1"),
                arguments,
                "frobnicate the trip",
            )
            .build()
            .unwrap()
    };

    // An operation nobody offers.
    let reduced = fixture
        .reduce(&turn(text), &with("trip.frobnicate", json!(null)))
        .expect("one unknown operation does not take the turn with it");
    assert_eq!(
        command_payloads(&reduced),
        vec![json!({"set_name": {"value": "Porto"}})],
        "the instruction the user gave and the runtime understood still runs"
    );
    let PlannedActResult::Rejected { rejection } = &reduced.acts[1].result else {
        panic!(
            "the act nobody offers is refused: {:?}",
            reduced.acts[1].result
        );
    };
    assert_eq!(rejection.code.as_str(), rejection::UNKNOWN_OPERATION);
    assert!(
        reduced
            .notices
            .iter()
            .any(|notice| notice.code == rejection::UNKNOWN_OPERATION),
        "and says so under its own code: {:?}",
        reduced.notices
    );

    // Arguments the operation's schema refuses.
    let reduced = fixture
        .reduce(&turn(text), &with(op::SET_DATE, json!({"value": 7})))
        .expect("nor do arguments that miss a schema");
    assert_eq!(
        command_payloads(&reduced),
        vec![json!({"set_name": {"value": "Porto"}})]
    );
    let PlannedActResult::Rejected { rejection } = &reduced.acts[1].result else {
        panic!("the act with bad arguments is refused");
    };
    assert_eq!(rejection.code.as_str(), rejection::INVALID_ARGUMENTS);
}

#[test]
fn do_not_submit_blocks_the_rebooking_and_nothing_else() {
    let text = "Set the name to Porto and rebook it, but do not rebook anything yet";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Porto"}),
            "Set the name to Porto",
        )
        .apply(op::REBOOK, fixture.token("i1"), json!(null), "rebook it")
        .constrain(ConstraintKind::DoNotSubmit, "do not rebook anything yet")
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();

    assert!(matches!(
        reduced.acts[0].result,
        PlannedActResult::ReadyToExecute { .. }
    ));
    assert_eq!(
        code(&reduced.acts[1].result),
        rejection::BLOCKED_BY_CONSTRAINT
    );
    assert_eq!(
        command_payloads(&reduced),
        vec![json!({"set_name": {"value": "Porto"}})],
        "the name change is not a rebooking"
    );
    assert_eq!(
        reduced.constraints_applied,
        vec![ConstraintKind::DoNotSubmit]
    );
    assert!(
        reduced
            .notices
            .iter()
            .any(|n| n.code == notice::NOTHING_SUBMITTED),
        "the user is told, in the server's own words, that nothing went out"
    );
}

#[test]
fn a_hypothetical_question_produces_no_act() {
    let text = "If I set the travel date to 2026-10-01, would the trip still be on time?";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .ask_about(
            AnswerBasis::ProposedState,
            None,
            &["trip.date"],
            "would the trip still be on time?",
        )
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();

    assert!(
        reduced.acts.is_empty(),
        "a hypothesis is not an act (rule 4)"
    );
    assert!(reduced.has_no_effects());
    assert!(reduced.pre_execution_interactions.is_empty());
    assert_eq!(reduced.answer_tasks.len(), 1);
    assert_eq!(
        reduced.answer_tasks[0].basis,
        AnswerBasis::CurrentCommittedState,
        "nothing was proposed, so a proposed-state answer would be about nothing"
    );
    assert!(reduced.answer_tasks[0].proposed_diff_ref.is_none());
}

#[test]
fn a_post_commit_basis_falls_back_when_nothing_will_commit() {
    let text = "Rebook it and tell me what the state will be afterwards";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(op::REBOOK, fixture.token("i1"), json!(null), "Rebook it")
        .ask_about(
            AnswerBasis::CommittedStateAfterTurn,
            None,
            &[],
            "tell me what the state will be afterwards",
        )
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();
    assert!(reduced.has_no_effects(), "the rebooking waits for a click");
    assert_eq!(
        reduced.answer_tasks[0].basis,
        AnswerBasis::CurrentCommittedState
    );
}

#[test]
fn a_domain_rejection_stops_one_act_and_keeps_the_rest() {
    let text = "Set the name to Porto and do the refused thing";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Porto"}),
            "Set the name to Porto",
        )
        .apply(
            op::REFUSED,
            fixture.token("i1"),
            json!(null),
            "do the refused thing",
        )
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();
    assert!(matches!(
        reduced.acts[0].result,
        PlannedActResult::ReadyToExecute { .. }
    ));
    assert_eq!(code(&reduced.acts[1].result), "trip.refused");

    // A command the domain validates away is a rejection too, not a silent drop.
    let empty = Fixture::new().case("i9", 1, "Empty", TripState::default());
    let text = "Rebook it";
    let understanding = UnderstandingBuilder::of(text)
        .apply(op::REBOOK, empty.token("i9"), json!(null), text)
        .build()
        .unwrap();
    let reduced = empty.reduce(&turn(text), &understanding).unwrap();
    assert_eq!(code(&reduced.acts[0].result), "trip.no_name");
}

#[test]
fn an_act_that_compiles_to_nothing_is_a_no_change_not_a_silence() {
    let text = "Do the thing that changes nothing";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(op::NOOP, fixture.token("i1"), json!(null), text)
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();
    assert_eq!(reduced.acts[0].result, PlannedActResult::NoChange);
    assert_eq!(reduced.acts.len(), 1, "every act keeps a slot (I11)");
}

#[test]
fn unresolvable_targets_each_get_their_own_reason() {
    let text = "Change that one";
    let fixture = draft();
    let workflow = WorkflowKey::from(common::WORKFLOW);
    let cases = [
        (
            ActTarget::Record {
                token: TargetToken::from("t_deadbeef0000"),
            },
            rejection::TARGET_UNAUTHORIZED,
        ),
        (
            ActTarget::NotListed {
                workflow: workflow.clone(),
                words: None,
            },
            rejection::TARGET_MISSING,
        ),
        (
            ActTarget::New { workflow },
            rejection::TARGET_POLICY_MISMATCH,
        ),
    ];
    for (target, expected) in cases {
        let understanding = UnderstandingBuilder::of(text)
            .apply_to(op::SET_NAME, target, json!({"value": "that one"}), text)
            .build()
            .unwrap();
        let reduced = fixture.reduce(&turn(text), &understanding).unwrap();
        assert_eq!(code(&reduced.acts[0].result), expected);
        assert!(
            reduced.has_no_effects(),
            "an unresolved target mutates nothing (I8)"
        );
    }
}

#[test]
fn a_new_case_is_minted_and_compiled_against_no_state() {
    let text = "Create a trip for Porto";
    let fixture = Fixture::new();
    let understanding = || {
        UnderstandingBuilder::of(text)
            .open(op::OPEN, common::WORKFLOW, json!({"value": "Porto"}), text)
            .build()
            .unwrap()
    };
    let reduced = fixture.reduce(&turn(text), &understanding()).unwrap();
    assert!(matches!(
        reduced.acts[0].result,
        PlannedActResult::ReadyToExecute { .. }
    ));
    let envelope = &reduced.batches[0].envelopes[0];
    assert_eq!(
        envelope.case_ref.expected_revision,
        turnframe_core::ids::CaseRevision::ZERO,
        "a case that does not exist yet is expected at revision zero (I13)"
    );
    assert!(!envelope.case_ref.case_id.as_str().is_empty());
    // Reducing the same turn again mints the same identifier.
    let again = fixture.reduce(&turn(text), &understanding()).unwrap();
    assert_eq!(again.plan_hash, reduced.plan_hash);
}

#[test]
fn ask_before_applying_turns_a_free_edit_into_a_card() {
    let text = "Set the name to Porto, but ask me first";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Porto"}),
            "Set the name to Porto",
        )
        .constrain(ConstraintKind::AskBeforeApplying, "ask me first")
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();
    let spec = match &reduced.acts[0].result {
        PlannedActResult::AwaitingConfirmation { interaction_spec } => interaction_spec,
        other => panic!("expected a confirmation, got {other:?}"),
    };
    assert_eq!(spec.validate(), Ok(()));
    assert!(reduced.has_no_effects());
    assert_eq!(
        reduced.constraints_applied,
        vec![ConstraintKind::AskBeforeApplying]
    );
}

#[test]
fn a_condition_the_reducer_cannot_evaluate_becomes_a_question() {
    let text = "Set the name to Porto if the airline already confirmed";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Porto"}),
            "Set the name to Porto",
        )
        .constrain(
            ConstraintKind::ApplyOnlyIf,
            "if the airline already confirmed",
        )
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();
    let spec = match &reduced.acts[0].result {
        PlannedActResult::NeedsClarification { interaction_spec } => interaction_spec,
        other => panic!("expected a clarification, got {other:?}"),
    };
    assert_eq!(
        spec.kind,
        turnframe_core::interaction::InteractionKind::Boolean
    );
    assert_eq!(spec.validate(), Ok(()));
    assert!(
        reduced.has_no_effects(),
        "guessing whether the condition holds is exactly what must not happen"
    );
    assert_eq!(reduced.pre_execution_interactions.len(), 1);
}

#[test]
fn the_command_budget_refuses_the_turn_rather_than_a_prefix() {
    use turnframe_runtime::config::{ExecutionConfig, OrchestratorConfig};
    let text = "Set the name to Porto and the travel date to 2026-10-01";
    let fixture = draft().config(
        OrchestratorConfig::conservative()
            .with_execution(ExecutionConfig::conservative().with_max_commands_per_turn(1)),
    );
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Porto"}),
            "Set the name to Porto",
        )
        .apply(
            op::SET_DATE,
            fixture.token("i1"),
            json!({"value": "2026-10-01"}),
            "the travel date to 2026-10-01",
        )
        .build()
        .unwrap();
    assert_eq!(
        fixture.reduce(&turn(text), &understanding),
        Err(ReductionError::CommandBudgetExceeded {
            limit: 1,
            actual: 2
        })
    );
}

#[test]
fn a_sandboxed_run_refuses_a_regulated_submission_outright() {
    use turnframe_runtime::config::{
        OrchestrationMode, OrchestratorConfig, ResourceBudget, SandboxAcknowledgement,
    };
    let text = "Rebook it";
    let sandboxed = draft().config(OrchestratorConfig::conservative().with_mode(
        OrchestrationMode::sandboxed_autonomous(
            ResourceBudget::conservative(),
            SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes(),
        ),
    ));
    let understanding = UnderstandingBuilder::of(text)
        .apply(op::REBOOK, sandboxed.token("i1"), json!(null), text)
        .build()
        .unwrap();
    // Even the card's own, click-confirmed act.
    let fixture = sandboxed.confirmed_origin(confirmed_origin(), understanding.acts[0].id);
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();
    assert_eq!(code(&reduced.acts[0].result), rejection::BLOCKED_BY_MODE);
    assert!(
        reduced.pre_execution_interactions.is_empty(),
        "no card helps"
    );
}

#[test]
fn every_reduction_passes_the_plans_own_consistency_rules() {
    // `reduce` runs `ReductionPlan::validate` before returning; assert it once
    // more from the outside on the busiest plan.
    let text = "Set the name to Porto, set the travel date to 2026-10-01 and rebook it";
    let fixture = draft();
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Porto"}),
            "Set the name to Porto",
        )
        .apply(
            op::SET_DATE,
            fixture.token("i1"),
            json!({"value": "2026-10-01"}),
            "set the travel date to 2026-10-01",
        )
        .apply(op::REBOOK, fixture.token("i1"), json!(null), "rebook it")
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();
    assert_eq!(reduced.validate(&ids(&understanding)), Ok(()));
    assert!(reduced.verify_hash().unwrap());
    assert_eq!(reduced.acts.len(), 3);
    assert_eq!(reduced.policy_decisions.len(), 3);
    assert_eq!(
        reduced.command_refs().len(),
        2,
        "the two edits run; the rebooking waits for its card"
    );
}

/// A refusal the runtime decided reaches the writer with the sentence the notice
/// shows, not as a bare code: the two are one source.
#[test]
fn a_runtime_refusal_carries_its_sentence_to_the_writer() {
    let fixture = draft();
    let text = "Do the card thing";
    // Refused by the runtime, so no domain writes an explanation and the copy
    // is the only source.
    let understanding = UnderstandingBuilder::of(text)
        .apply(op::CARD_ONLY, fixture.token("i1"), json!(null), text)
        .build()
        .unwrap();
    let reduced = fixture
        .reduce(&turn(text), &understanding)
        .expect("the turn reduces");

    let refusal = reduced
        .refusals
        .iter()
        .find_map(|fact| match fact {
            turnframe_core::response::NarratableFact::ActRefused {
                code, explanation, ..
            } => Some((code.clone(), explanation.clone())),
            _ => None,
        })
        .expect("the writer is told the act was refused");
    assert_eq!(refusal.0, rejection::NOT_PROPOSABLE);
    assert!(
        !refusal.1.is_empty(),
        "a code with no sentence is what the writer invents from"
    );
    assert!(
        reduced.notices.iter().any(|notice| notice
            .text
            .resolve(&turnframe_core::locale::Locale::new("en"))
            == refusal.1),
        "the writer and the notice must not tell two different stories: {:?}",
        reduced.notices
    );
}
