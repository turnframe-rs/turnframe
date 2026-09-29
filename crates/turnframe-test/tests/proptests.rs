//! Property tests over the kit's own strategies, samples and executor
//! (spec §27.2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use futures::executor::block_on;
use proptest::prelude::*;
use turnframe_core::case::CaseRef;
use turnframe_core::command::{CommandOrigin, origin_satisfies};
use turnframe_core::error::ExecutionError;
use turnframe_core::flow::{WorkflowDefinition, WorkflowExecutor, check_view};
use turnframe_core::hash::Digest;
use turnframe_core::ids::{CaseId, CaseRevision};
use turnframe_core::turn::TurnInput;
use turnframe_core::understanding::Understanding;
use turnframe_test::assertions::{no_high_risk_without_trusted_origin, origin_satisfies_policy};
use turnframe_test::explore::{SimulatedTransition, WorkflowModel};
use turnframe_test::strategies;
use turnframe_test::workflows::traveler::{TravelerCommand, TravelerWorkflow};
use turnframe_test::workflows::trip::{TripCommand, TripModel, TripState, TripWorkflow};

/// Walks the trip model, picking one candidate command per byte, and
/// returns the state it lands in.
fn walk_trip(choices: &[u8]) -> Option<TripState> {
    let model = TripModel::default();
    let mut state: Option<TripState> = None;
    for choice in choices {
        let candidates = model.candidate_commands(state.as_ref());
        if candidates.is_empty() {
            break;
        }
        let command = &candidates[usize::from(*choice) % candidates.len()];
        if let SimulatedTransition::Applied { state: next, .. } =
            model.simulate(state.as_ref(), command)
        {
            state = next;
        }
    }
    state
}

fn text_and_understanding() -> impl Strategy<Value = (String, Understanding)> {
    strategies::user_text().prop_flat_map(|text| {
        let understanding = strategies::grounded_understanding(&text);
        (Just(text), understanding)
    })
}

fn turn_and_understanding() -> impl Strategy<Value = (TurnInput, Understanding)> {
    strategies::turn_input().prop_flat_map(|turn| {
        let understanding =
            strategies::grounded_understanding(turn.text.as_deref().unwrap_or_default());
        (Just(turn), understanding)
    })
}

/// Every word range of `understanding` covers whole words of `text`.
fn ranges_are_words_of(text: &str, understanding: &Understanding) -> bool {
    let words = turnframe_understand::Words::split(text);
    let ranges = understanding
        .acts
        .iter()
        .map(|act| act.words)
        .chain(understanding.questions.iter().map(|q| q.words))
        .chain(understanding.constraints.iter().map(|c| c.words));
    ranges.into_iter().all(|range| {
        words
            .range(turnframe_understand::Span::new(range.first, range.last))
            .is_ok_and(|found| found == range)
    })
}

proptest! {
    /// An understanding grounded in a text points at whole words of it.
    #[test]
    fn grounded_understandings_point_at_words((text, understanding) in text_and_understanding()) {
        prop_assert!(ranges_are_words_of(&text, &understanding));
    }

    /// The same holds against a whole turn, including turns with no text at all, where
    /// the only grounded understanding is the empty one.
    #[test]
    fn grounded_understandings_fit_their_turn((turn, understanding) in turn_and_understanding()) {
        prop_assert_eq!(turn.validate_shape(), Ok(()));
        prop_assert!(ranges_are_words_of(turn.text.as_deref().unwrap_or_default(), &understanding));
    }

    /// Every generated card can be answered and hashes to what it stores.
    #[test]
    fn generated_interactions_are_answerable(interaction in strategies::interaction()) {
        prop_assert!(interaction.verify_payload_hash().unwrap());
        prop_assert!(interaction.payload.validate_for(interaction.kind).is_ok());
    }

    /// The assertion helper says exactly what core says (I12).
    #[test]
    fn the_origin_check_agrees_with_core(
        policy in strategies::command_policy(),
        origin in strategies::command_origin(),
    ) {
        let case_ref = CaseRef::new("trip", "trip-1", CaseRevision::ZERO);
        let batch = support::batch(
            support::turn(0),
            &case_ref,
            &origin,
            vec![TripCommand::Open],
        );
        let envelope = &batch.envelopes[0];
        prop_assert_eq!(
            origin_satisfies_policy(envelope, &policy).is_ok(),
            origin_satisfies(&origin, &policy)
        );
    }

    /// A trusted origin never comes from the user's words alone.
    #[test]
    fn a_trusted_origin_is_never_a_direct_user_act(origin in strategies::trusted_origin()) {
        prop_assert!(origin.is_trusted());
        let is_direct = matches!(origin, CommandOrigin::DirectSafeUserAct { .. });
        prop_assert!(!is_direct);
    }

    /// Projection is pure: two calls with the same inputs agree, and the result
    /// always satisfies the §8.4 invariants.
    #[test]
    fn trip_projection_is_deterministic(
        choices in proptest::collection::vec(any::<u8>(), 0..8),
        case_id in strategies::case_id(),
        revision in strategies::case_revision(),
    ) {
        let workflow = TripWorkflow::default();
        let state = walk_trip(&choices);
        let case_ref = CaseRef::new("trip", case_id, revision);
        let first = workflow.project(case_ref.clone(), state.as_ref());
        let second = workflow.project(case_ref, state.as_ref());

        prop_assert_eq!(&first, &second);
        prop_assert!(check_view(&workflow, &first).is_ok());
        let ownership = workflow.phase_ownership(&first.phase);
        prop_assert_eq!(
            first.erase(ownership).unwrap(),
            second.erase(ownership).unwrap()
        );
    }

    /// The executor replays a repeated idempotency key instead of repeating the
    /// effect, and refuses a batch planned against a revision that has moved
    /// (I13, I14).
    #[test]
    fn the_executor_replays_a_key_and_conflicts_on_a_stale_revision(
        choices in proptest::collection::vec(any::<u8>(), 1..6),
    ) {
        let workflow = TripWorkflow::default();
        let model = TripModel::default();
        let executor = turnframe_test::workflows::trip::TripExecutor::default();
        let origin = support::confirmed_click();
        let case_id = CaseId::from("trip-1");
        let mut state: Option<TripState> = None;
        let mut revision = CaseRevision::ZERO;
        let mut last = None;

        for (step, choice) in choices.iter().enumerate() {
            let candidates = model.candidate_commands(state.as_ref());
            if candidates.is_empty() {
                break;
            }
            let command = candidates[usize::from(*choice) % candidates.len()].clone();
            if workflow.validate_command(state.as_ref(), &command).is_err() {
                continue;
            }
            let case_ref = CaseRef::new("trip", case_id.clone(), revision);
            let batch = support::batch(
                support::turn(u8::try_from(step).unwrap_or(0)),
                &case_ref,
                &origin,
                vec![command],
            );
            let commit = block_on(executor.execute(batch.clone())).unwrap();
            prop_assert!(!commit.idempotency_replay);
            prop_assert_eq!(commit.new_revision, revision.next());
            revision = commit.new_revision;
            state.clone_from(&commit.state);
            last = Some((batch, commit));
        }

        let Some((batch, commit)) = last else {
            return Ok(());
        };
        let replay = block_on(executor.execute(batch)).unwrap();
        prop_assert!(replay.idempotency_replay);
        prop_assert_eq!(&replay.events, &commit.events);
        prop_assert_eq!(replay.new_revision, commit.new_revision);
        prop_assert_eq!(executor.revision_of(&support::account(), &case_id), revision);

        let stale = support::batch(
            support::turn(u8::MAX),
            &CaseRef::new("trip", case_id, CaseRevision::ZERO),
            &origin,
            vec![TripCommand::Open],
        );
        prop_assert!(matches!(
            block_on(executor.execute(stale)),
            Err(ExecutionError::RevisionConflict(_))
        ));
    }
}

#[test]
fn a_consequential_sample_command_needs_a_trusted_origin() {
    let direct = CommandOrigin::DirectSafeUserAct {
        evidence_digest: Digest::of_bytes(b"the user said so"),
    };
    let trip_case = CaseRef::new("trip", "trip-1", CaseRevision(4));
    let untrusted = support::batch(
        support::turn(1),
        &trip_case,
        &direct,
        vec![TripCommand::Rebook],
    );
    let confirmed = support::batch(
        support::turn(1),
        &trip_case,
        &support::confirmed_click(),
        vec![TripCommand::Rebook],
    );

    assert!(
        no_high_risk_without_trusted_origin(&TripWorkflow::default(), None, &untrusted).is_err(),
        "a rebooking sent to the airline must not run on the user's words alone"
    );
    assert!(
        no_high_risk_without_trusted_origin(&TripWorkflow::default(), None, &confirmed).is_ok()
    );

    let traveler_case = CaseRef::new("traveler", "trav-1", CaseRevision(2));
    let deletion = support::batch(
        support::turn(2),
        &traveler_case,
        &direct,
        vec![TravelerCommand::Delete],
    );
    assert!(
        no_high_risk_without_trusted_origin(&TravelerWorkflow::new().with_cards(), None, &deletion)
            .is_err(),
        "a destructive deletion must not run on the user's words alone"
    );
}
