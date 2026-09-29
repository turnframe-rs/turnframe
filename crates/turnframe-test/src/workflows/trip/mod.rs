//! The trip sample: the disruption case of one booking.
//!
//! It is the awkward workflow on purpose: several obligations are open at once,
//! one of them per extra; the rebooking card is a persistent card bound to the
//! case revision and to the quote it shows, so a new quote makes a click on it
//! stale; the rebooking goes to an airline that may answer late or never, and the
//! states after it are never collapsed into "done"; and a leg the traveler asked
//! to keep is refused any change, by the domain, whatever was read.
//!
//! ```
//! use turnframe_core::case::CaseRef;
//! use turnframe_core::flow::{WorkflowDefinition, check_view};
//! use turnframe_core::ids::CaseRevision;
//! use turnframe_test::workflows::trip::{TripPhase, TripWorkflow, awaiting_rebooking_confirmation};
//!
//! let workflow = TripWorkflow::default();
//! let state = awaiting_rebooking_confirmation();
//! let view = workflow.project(CaseRef::new("trip", "trip-1", CaseRevision(7)), Some(&state));
//!
//! assert_eq!(view.phase, TripPhase::AwaitingRebookingConfirmation);
//! assert!(view.obligations.is_empty());
//! assert!(view.blocking_interaction.is_some());
//! assert!(check_view(&workflow, &view).is_ok());
//! ```

pub mod airline;
pub mod apply;
pub mod command;
pub mod definition;
pub mod model;
pub mod state;

pub use airline::{AirlineMode, airline_answer};
pub use apply::{apply, extra_id_for, next_status_for, rejection, validate};
pub use command::{
    AddExtraArgs, AssignPayerArgs, ChangeExtraArgs, ChangeTravelerArgs, LegArgs, OpenArgs,
    SetNameArgs, SetTravelDateArgs, SetTravelerArgs, TravelerRecord, TripCommand, TripEvent,
    operations,
};
pub use definition::{
    REBOOK_CONFIRM_OPTION, REBOOK_DECLINE_OPTION, REBOOKING_CONFIRMATION_KEY, TripWorkflow,
};
pub use model::{
    OTHER_TRAVELER_ID, REQUOTED_FARE_DIFFERENCE_CENTS, SAMPLE_EXTRA_DESCRIPTIONS,
    SAMPLE_FARE_DIFFERENCE_CENTS, SAMPLE_NAME, SAMPLE_REFUSAL_CODE, SAMPLE_TICKET_NUMBER,
    SAMPLE_TRAVELER_ID, TripModel, awaiting_rebooking_confirmation,
    awaiting_rebooking_confirmation_at, complete_case, incomplete_case, other_traveler,
    sample_legs, sample_new_extra, sample_offer, sample_quote, sample_travel_date, sample_traveler,
    unassigned_case, with_offer,
};
pub use state::{
    Extra, Leg, LegStatus, NewExtra, Offer, Payer, TripObligation, TripOutcome, TripPhase,
    TripState, TripStatus, TripTraveler,
};

/// An in-memory executor for the trip workflow.
pub type TripExecutor = crate::workflows::InMemoryExecutor<TripWorkflow>;

/// Case identifier the executor conformance suite runs against.
pub const CONFORMANCE_CASE_ID: &str = "trip-conformance";

/// Revision the conformance case is seeded at, chosen so a failure that reports
/// `0` or `1` is obviously wrong rather than accidentally right.
pub const CONFORMANCE_REVISION: u64 = 4;

/// The trip sample wired up for [`executors::run_all`](crate::executors::run_all).
///
/// The trip is the sample the executor suite uses because `AddExtra` appends: an
/// executor that ran a replayed key again instead of replaying it would leave a
/// state the suite can tell apart, which an assignment would not.
///
/// * `first` adds a second extra to a complete case;
/// * `second` names the trip otherwise, which serializes differently;
/// * `refused` sets an empty name, refused in every editable state.
#[must_use]
pub fn conformance_case() -> crate::executors::InMemoryCase<TripWorkflow> {
    crate::executors::InMemoryCase::new(
        turnframe_core::case::CaseRef::new(
            "trip",
            CONFORMANCE_CASE_ID,
            turnframe_core::ids::CaseRevision(CONFORMANCE_REVISION),
        ),
        Some(complete_case()),
        TripCommand::AddExtra {
            extra: sample_new_extra(1),
        },
        TripCommand::SetName {
            value: "A different name".to_owned(),
        },
        TripCommand::SetName {
            value: String::new(),
        },
    )
}
