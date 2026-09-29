//! An act a keep-unchanged constraint held is reported as left alone at the user's
//! asking, quoting the constraint: never as words that were not understood.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fixture, TripState, op, turn};
use serde_json::json;
use turnframe_core::understanding::{ConstraintKind, NotUnderstoodReason, UnitId};
use turnframe_runtime::reduce::notice;

#[test]
fn an_act_held_by_a_constraint_is_reported_left_alone() {
    let text = "Set the name to Lisbon and the date to 2026-10-01, but leave the date as it is";
    let fixture = Fixture::new().case("i1", 3, "Trip 1", TripState::with_name("Offsite"));
    let understanding = turnframe_test::providers::UnderstandingBuilder::of(text)
        .apply(
            op::SET_NAME,
            fixture.token("i1"),
            json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .not_understood(
            NotUnderstoodReason::KeptUnchanged {
                constraint: UnitId(3),
            },
            "the date to 2026-10-01",
        )
        .constrain(ConstraintKind::KeepUnchanged, "leave the date as it is")
        .build()
        .unwrap();
    let reduced = fixture.reduce(&turn(text), &understanding).unwrap();

    let commands: Vec<_> = reduced
        .batches
        .iter()
        .flat_map(|batch| batch.envelopes.iter().map(|e| e.command.clone()))
        .collect();
    assert_eq!(commands.len(), 1, "only the name runs: {commands:?}");
    let left = reduced
        .notices
        .iter()
        .find(|n| n.code == notice::KEPT_UNCHANGED)
        .expect("the user is told what was left alone");
    assert_eq!(
        left.text.default,
        "Left as it is, as you asked: «leave the date as it is»."
    );
    assert_eq!(
        left.text.resolve(&"it".into()),
        "Lasciato com'è, come mi hai chiesto: «leave the date as it is»."
    );
    assert!(
        !reduced
            .notices
            .iter()
            .any(|n| n.code == notice::NOT_UNDERSTOOD),
        "{:?}",
        reduced.notices
    );
}
