//! Two acts of one operation on one case are two acts.
//!
//! Whether a later act replaces an earlier one is the understanding's to say, in
//! `Understanding::superseded`; the runtime never infers it from the operation
//! key, so one message about two names of a parameterized operation runs both.
//! An act the understanding does drop is reported, by operation, and recorded.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::{OperationKey, TurnId};
use turnframe_core::observe::Signal;
use turnframe_core::understanding::ActAction;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations, sample_new_extra};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// The `trip.add_extra` arguments of the sample extra at `position`.
fn extra(position: usize) -> serde_json::Value {
    let line = sample_new_extra(position);
    serde_json::json!({
        "description": line.description,
        "quantity": line.quantity,
        "unit_price": { "minor": line.unit_price_cents, "currency": "EUR" },
    })
}

/// `trip.add_extra` carries its name in its arguments, so two extras in one
/// message are two extras and not one correcting the other.
#[tokio::test]
async fn one_operation_used_twice_about_two_subjects_runs_twice() {
    let turn = turn_one();
    let text = "Add a lounge pass and a hotel night";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::ADD_EXTRA,
            token_for(turn, "trip", "trip-1"),
            extra(1),
            "Add a lounge pass",
        )
        .apply(
            operations::ADD_EXTRA,
            token_for(turn, "trip", "trip-1"),
            extra(2),
            "a hotel night",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;

    harness.handle(harness.turn(turn, text)).await.unwrap();

    let events = harness.events("trip", "trip-1").await;
    assert_eq!(
        events.len(),
        2,
        "both extras the user asked for were added, not just the last one"
    );
    assert_eq!(
        harness.observed().count(Signal::ActSuperseded),
        0,
        "nothing was superseded, so nothing is reported as superseded"
    );
}

const CORRECTED: &str = "Set the name to Lisbon, no, to Porto";

/// A turn whose first name the second replaces, run to completion.
async fn corrected() -> Harness {
    let turn = turn_one();
    let understanding = UnderstandingBuilder::of(CORRECTED)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .superseded_by_next()
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Porto"}),
            "no, to Porto",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(understanding)
        .without_narration()
        .observing()
        .build()
        .await;
    harness.handle(harness.turn(turn, CORRECTED)).await.unwrap();
    harness
}

#[tokio::test]
async fn a_second_value_for_one_subject_is_still_a_correction() {
    let harness = corrected().await;

    assert_eq!(
        harness.trip_name("trip-1").as_deref(),
        Some("Porto"),
        "only the value the user landed on is written"
    );
    assert_eq!(
        harness.events("trip", "trip-1").await.len(),
        1,
        "a value the user replaced is not applied on the way to the one they meant"
    );
    assert_eq!(
        harness.observed().count(Signal::ActSuperseded),
        1,
        "and the act that was dropped is reported, rather than the turn quietly \
         doing less than it was asked"
    );
}

/// The dropped act is named, not merely counted: on the signal and in the record.
#[tokio::test]
async fn the_record_says_which_operation_was_dropped() {
    let harness = corrected().await;

    let labels = harness
        .observed()
        .occurrences(Signal::ActSuperseded)
        .into_iter()
        .filter_map(|observed| observed.labels.operation)
        .map(|operation| operation.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec![operations::SET_NAME.to_owned()],
        "an operator sees which operation went missing, not only that one did"
    );

    let understanding = harness
        .replay(turn_one())
        .await
        .understanding
        .expect("the record keeps what the turn was understood to say");
    let dropped = understanding
        .superseded
        .iter()
        .map(|superseded| superseded.action.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        dropped,
        vec![ActAction::Apply {
            operation: OperationKey::from(operations::SET_NAME)
        }],
        "and a replay can say which act the correction replaced"
    );
}
