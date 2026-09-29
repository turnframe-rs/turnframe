//! Properties of the reducer that no fixed scenario can state.
//!
//! Two claims are checked here. The first is that the reducer decides on
//! meaning, not on the order an understanding happens to list independent acts
//! in. The second is that reducing the same turn twice produces the same plan,
//! hash included, which is what makes crash recovery and replay possible (I20).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fixture, TripState, op, turn};
use proptest::prelude::*;
use serde_json::json;
use turnframe_core::reduce::ReductionPlan;
use turnframe_core::understanding::{ActId, Understanding};
use turnframe_test::providers::UnderstandingBuilder;

/// Every phrase the user could have written, always all present, so any
/// selection of the edits quotes words the message contains.
const TEXT: &str = "set i1 name; set i1 date; set i2 name; set i2 date; set i3 name; set i3 date";

/// The six mutually independent edits: three cases, two fields each. No two of
/// them touch the same field of the same case, so the order they arrive in must
/// not matter.
const EDITS: [(&str, &str, &str); 6] = [
    ("i1", op::SET_NAME, "set i1 name"),
    ("i1", op::SET_DATE, "set i1 date"),
    ("i2", op::SET_NAME, "set i2 name"),
    ("i2", op::SET_DATE, "set i2 date"),
    ("i3", op::SET_NAME, "set i3 name"),
    ("i3", op::SET_DATE, "set i3 date"),
];

fn fixture() -> Fixture {
    Fixture::new()
        .case("i1", 1, "One", TripState::with_name("a"))
        .case("i2", 2, "Two", TripState::with_name("b"))
        .case("i3", 3, "Three", TripState::with_name("c"))
}

/// The understanding of `edits`, one act per edit, numbered in the order given.
fn understanding(fixture: &Fixture, edits: &[usize]) -> Understanding {
    edits
        .iter()
        .fold(UnderstandingBuilder::of(TEXT), |builder, edit| {
            let (case_id, operation, phrase) = EDITS[*edit];
            builder.apply(
                operation,
                fixture.token(case_id),
                json!({ "value": phrase }),
                phrase,
            )
        })
        .build()
        .unwrap()
}

/// The same understanding with its acts listed in `order`; each keeps its id.
fn listed_in(understanding: &Understanding, edits: &[usize], order: &[usize]) -> Understanding {
    let mut reordered = understanding.clone();
    reordered.acts = order
        .iter()
        .map(|edit| {
            let position = edits.iter().position(|e| e == edit).unwrap();
            understanding.acts[position].clone()
        })
        .collect();
    reordered
}

fn ids(understanding: &Understanding) -> Vec<ActId> {
    understanding.acts.iter().map(|act| act.id).collect()
}

/// The commands a plan would run, as `(case, command)` pairs sorted so two
/// plans that run the same work compare equal whatever order they batched it in.
fn command_set(plan: &ReductionPlan) -> Vec<(String, String)> {
    let mut commands: Vec<(String, String)> = plan
        .batches
        .iter()
        .flat_map(|batch| {
            batch.envelopes.iter().map(|envelope| {
                (
                    envelope.case_ref.case_id.to_string(),
                    envelope.command.to_string(),
                )
            })
        })
        .collect();
    commands.sort();
    commands
}

/// A non-empty selection of the independent edits, in some order.
fn edit_selection() -> impl Strategy<Value = Vec<usize>> {
    proptest::sample::subsequence((0..EDITS.len()).collect::<Vec<_>>(), 1..=EDITS.len())
        .prop_shuffle()
}

proptest! {
    /// An understanding may list independent acts in any order; the work the
    /// turn does is the same either way.
    #[test]
    fn shuffling_independent_acts_keeps_the_command_set(
        (ordered, shuffled) in edit_selection()
            .prop_flat_map(|edits| (Just(edits.clone()), Just(edits).prop_shuffle()))
    ) {
        let fixture = fixture();
        let listed = understanding(&fixture, &ordered);
        let reordered = listed_in(&listed, &ordered, &shuffled);
        let first = fixture.reduce(&turn(TEXT), &listed).unwrap();
        let second = fixture.reduce(&turn(TEXT), &reordered).unwrap();

        prop_assert_eq!(command_set(&first), command_set(&second));
        prop_assert_eq!(first.acts.len(), second.acts.len());
        prop_assert_eq!(
            first.batches.iter().map(|b| b.len()).sum::<usize>(),
            second.batches.iter().map(|b| b.len()).sum::<usize>()
        );
        // Every act is accounted for in both, and both survive their own
        // consistency rules.
        prop_assert_eq!(first.validate(&ids(&listed)), Ok(()));
        prop_assert_eq!(second.validate(&ids(&reordered)), Ok(()));
    }

    /// Reducing the same inputs twice is the same reduction, byte for byte.
    #[test]
    fn reducing_the_same_turn_twice_gives_the_same_plan_hash(edits in edit_selection()) {
        let fixture = fixture();
        let first = fixture.reduce(&turn(TEXT), &understanding(&fixture, &edits)).unwrap();
        let second = fixture.reduce(&turn(TEXT), &understanding(&fixture, &edits)).unwrap();

        prop_assert_eq!(&first.plan_hash, &second.plan_hash);
        prop_assert_eq!(&first, &second);
        prop_assert!(first.verify_hash().unwrap());
    }

    /// Whatever the understanding, every act gets exactly one result and no
    /// command escapes a policy decision.
    #[test]
    fn every_act_keeps_a_slot_and_every_command_keeps_a_decision(edits in edit_selection()) {
        let fixture = fixture();
        let understood = understanding(&fixture, &edits);
        let reduced = fixture.reduce(&turn(TEXT), &understood).unwrap();
        prop_assert_eq!(reduced.acts.len(), edits.len());
        let planned: Vec<ActId> = reduced.acts.iter().map(|a| a.act.id).collect();
        prop_assert_eq!(planned, ids(&understood));
        prop_assert_eq!(reduced.policy_decisions.len(), reduced.command_refs().len());
        prop_assert!(reduced.policy_decisions.iter().all(|d| d.allowed));
    }
}
