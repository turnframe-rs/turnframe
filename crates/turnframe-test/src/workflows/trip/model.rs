//! The exploration model and the ready-made fixtures of the trip sample.

use chrono::NaiveDate;
use turnframe_core::event::ExternalStatus;
use uuid::Uuid;

use crate::explore::{SimulatedTransition, WorkflowModel};
use crate::workflows::simulate;
use crate::workflows::trip::apply::{MAX_EXTRAS, extra_id_for};
use crate::workflows::trip::command::{TripCommand, TripEvent};
use crate::workflows::trip::definition::TripWorkflow;
use crate::workflows::trip::state::{
    Extra, Leg, LegStatus, NewExtra, Offer, Payer, TripOutcome, TripState, TripStatus, TripTraveler,
};

/// Identifier of the traveler the model puts on cases.
pub const SAMPLE_TRAVELER_ID: Uuid = Uuid::from_u128(0x7a1e_0000_0000_4000_8000_0000_0000_0001);

/// Identifier of a second traveler, used to exercise the sensitive change.
pub const OTHER_TRAVELER_ID: Uuid = Uuid::from_u128(0x7a1e_0000_0000_4000_8000_0000_0000_0002);

/// Name the model gives the trip.
pub const SAMPLE_NAME: &str = "Lisbon offsite";

/// Descriptions of the extras the model adds, in order.
pub const SAMPLE_EXTRA_DESCRIPTIONS: [&str; 4] = [
    "Checked bag",
    "Seat with extra legroom",
    "Airport meal",
    "Hotel night",
];

/// Ticket number the sample airline issues.
pub const SAMPLE_TICKET_NUMBER: &str = "055-2100000123";

/// Reason code the sample airline returns when it refuses a rebooking.
pub const SAMPLE_REFUSAL_CODE: &str = "SEAT_GONE";

/// The fare difference of the sample offer, in cents: €84.
pub const SAMPLE_FARE_DIFFERENCE_CENTS: i64 = 8_400;

/// The fare difference after the airline quotes again, in cents: €132.
pub const REQUOTED_FARE_DIFFERENCE_CENTS: i64 = 13_200;

/// Most extras the exploration model adds: the domain's own maximum, so the cap
/// and the refusal that guards it are inside the explored states. The state space
/// stays bounded because extras are added in a fixed order and only the first one
/// without a payer is given one, so the paid extras are always a prefix.
pub const MODEL_EXTRA_BUDGET: usize = MAX_EXTRAS;

/// The travel date the model sets.
#[must_use]
pub fn sample_travel_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 6).unwrap_or(NaiveDate::MIN)
}

/// The two flights of the sample booking: the outbound was cancelled, the return
/// flies as booked.
#[must_use]
pub fn sample_legs() -> Vec<Leg> {
    vec![
        Leg {
            number: 1,
            flight: "AZ610".to_owned(),
            from: "FCO".to_owned(),
            to: "LIS".to_owned(),
            departs: "2026-10-05 07:40".to_owned(),
            status: LegStatus::Cancelled,
            protected: false,
        },
        Leg {
            number: 2,
            flight: "AZ611".to_owned(),
            from: "LIS".to_owned(),
            to: "FCO".to_owned(),
            departs: "2026-10-09 18:20".to_owned(),
            status: LegStatus::OnTime,
            protected: false,
        },
    ]
}

/// The rebooking the airline quotes for a leg of the sample booking.
#[must_use]
pub fn sample_offer(leg: u32) -> Offer {
    Offer {
        leg,
        flight: if leg == 1 { "AZ612" } else { "AZ613" }.to_owned(),
        departs: if leg == 1 {
            "2026-10-05 13:10"
        } else {
            "2026-10-10 09:30"
        }
        .to_owned(),
        fare_difference_cents: SAMPLE_FARE_DIFFERENCE_CENTS,
    }
}

/// The airline's quote of the sample offer for `leg`, as the command it applies.
#[must_use]
pub fn sample_quote(leg: u32) -> TripCommand {
    let offer = sample_offer(leg);
    TripCommand::Requote {
        leg: offer.leg,
        flight: offer.flight,
        departs: offer.departs,
        fare_difference_cents: offer.fare_difference_cents,
    }
}

/// The traveler the model puts on cases.
#[must_use]
pub fn sample_traveler() -> TripTraveler {
    TripTraveler {
        traveler_id: SAMPLE_TRAVELER_ID,
        display_name: "Marta Bianchi".to_owned(),
    }
}

/// A second traveler, for testing the sensitive traveler change.
#[must_use]
pub fn other_traveler() -> TripTraveler {
    TripTraveler {
        traveler_id: OTHER_TRAVELER_ID,
        display_name: "Luca Ferri".to_owned(),
    }
}

/// The n-th sample extra, without a payer.
#[must_use]
pub fn sample_new_extra(position: usize) -> NewExtra {
    let description = SAMPLE_EXTRA_DESCRIPTIONS
        .get(position)
        .copied()
        .unwrap_or("Other extra");
    NewExtra {
        description: description.to_owned(),
        quantity: u32::try_from(position).unwrap_or(0) + 1,
        unit_price_cents: 4_000,
    }
}

/// A case with its traveler, the booking's legs and one paid extra, but no name
/// and no travel date: two obligations are still open.
#[must_use]
pub fn incomplete_case() -> TripState {
    let extra = sample_new_extra(0);
    TripState {
        traveler: Some(sample_traveler()),
        legs: sample_legs(),
        extras: vec![Extra {
            extra_id: extra_id_for(0, &extra.description),
            description: extra.description,
            quantity: extra.quantity,
            unit_price_cents: extra.unit_price_cents,
            payer: Some(Payer::Airline),
        }],
        ..TripState::default()
    }
}

/// A case whose one extra has no payer yet, so
/// [`TripObligation::AssignPayer`](super::TripObligation::AssignPayer) is open:
/// the state where a user asks who can pay while the flow is collecting exactly
/// that field.
#[must_use]
pub fn unassigned_case() -> TripState {
    let mut state = incomplete_case();
    for extra in &mut state.extras {
        extra.payer = None;
    }
    state
}

/// A case with every obligation met, still in `Collecting`.
#[must_use]
pub fn complete_case() -> TripState {
    TripState {
        name: Some(SAMPLE_NAME.to_owned()),
        travel_date: Some(sample_travel_date()),
        ..incomplete_case()
    }
}

/// A complete case with the airline's quote for `leg`.
#[must_use]
pub fn with_offer(leg: u32) -> TripState {
    TripState {
        offer: Some(sample_offer(leg)),
        ..complete_case()
    }
}

/// A complete case whose rebooking card for the outbound leg is waiting for a
/// click, at the sample fare difference.
#[must_use]
pub fn awaiting_rebooking_confirmation() -> TripState {
    awaiting_rebooking_confirmation_at(SAMPLE_FARE_DIFFERENCE_CENTS)
}

/// A complete case whose rebooking card for the outbound leg is waiting for a
/// click, at the given fare difference.
#[must_use]
pub fn awaiting_rebooking_confirmation_at(fare_difference_cents: i64) -> TripState {
    let mut offer = sample_offer(1);
    offer.fare_difference_cents = fare_difference_cents;
    TripState {
        status: TripStatus::AwaitingRebookingConfirmation,
        external_status: Some(ExternalStatus::AwaitingConfirmation),
        offer: Some(offer),
        ..complete_case()
    }
}

impl TripState {
    /// The same state with `leg` protected.
    #[must_use]
    pub fn protected(mut self, leg: u32) -> Self {
        for found in &mut self.legs {
            if found.number == leg {
                found.protected = true;
            }
        }
        self
    }
}

/// The exploration model of the trip workflow.
///
/// It offers, in every state, only the commands that make sense there plus one it
/// knows will be refused (an empty name), so every explored state also checks that
/// a refusal changes nothing. All values are fixed constants and identifiers are
/// derived, so the reachable state space is finite and the same on every run.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct TripModel {
    /// The definition transitions are applied through.
    pub workflow: TripWorkflow,
}

impl TripModel {
    /// Builds the model.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workflow: TripWorkflow::new(),
        }
    }

    fn editing_commands(state: &TripState, commands: &mut Vec<TripCommand>) {
        // The sensitive change is offered in both directions from every editable
        // state; the traveler is one of three values, so the space grows by a
        // constant factor.
        commands.push(TripCommand::ChangeTraveler {
            traveler: match state.traveler.as_ref() {
                Some(current) if current.traveler_id == SAMPLE_TRAVELER_ID => other_traveler(),
                _ => sample_traveler(),
            },
        });
        if state.name.is_none() {
            commands.push(TripCommand::SetName {
                value: SAMPLE_NAME.to_owned(),
            });
        }
        if state.travel_date.is_none() {
            commands.push(TripCommand::SetTravelDate {
                value: sample_travel_date(),
            });
        }
        if state.extras.len() < MODEL_EXTRA_BUDGET || state.extras.len() >= MAX_EXTRAS {
            // Past the domain's own cap the command is refused, which costs one
            // simulation and proves the cap holds.
            commands.push(TripCommand::AddExtra {
                extra: sample_new_extra(state.extras.len()),
            });
        }
        if let Some(extra) = state.extras.iter().find(|e| e.payer.is_none()) {
            commands.push(TripCommand::AssignPayer {
                extra_id: extra.extra_id,
                payer: Payer::Airline,
            });
        }
        if state.offer.is_none() {
            commands.push(sample_quote(1));
        }
        if state
            .legs
            .iter()
            .any(|leg| leg.number == 2 && !leg.protected)
        {
            commands.push(TripCommand::ProtectLeg { leg: 2 });
        }
    }
}

impl WorkflowModel<TripWorkflow> for TripModel {
    fn initial_states(&self) -> Vec<Option<TripState>> {
        vec![None]
    }

    fn candidate_commands(&self, state: Option<&TripState>) -> Vec<TripCommand> {
        let Some(state) = state else {
            return vec![TripCommand::Open];
        };
        let mut commands = Vec::new();
        match state.status {
            TripStatus::Draft | TripStatus::Refused => {
                Self::editing_commands(state, &mut commands);
                if let Some(offer) = &state.offer
                    && state.is_complete()
                {
                    commands.push(TripCommand::RequestRebooking { leg: offer.leg });
                }
                commands.push(TripCommand::Withdraw);
            }
            TripStatus::AwaitingRebookingConfirmation => {
                Self::editing_commands(state, &mut commands);
                commands.push(TripCommand::Rebook);
                commands.push(TripCommand::Withdraw);
            }
            TripStatus::Rebooking => {
                for status in [
                    ExternalStatus::ReceivedByIntermediary,
                    ExternalStatus::Accepted,
                    ExternalStatus::Rejected,
                ] {
                    commands.push(TripCommand::RecordAirlineOutcome {
                        status,
                        ticket_number: (status == ExternalStatus::Accepted)
                            .then(|| SAMPLE_TICKET_NUMBER.to_owned()),
                        reason_code: (status == ExternalStatus::Rejected)
                            .then(|| SAMPLE_REFUSAL_CODE.to_owned()),
                    });
                }
            }
            TripStatus::Ticketed => {
                for status in [ExternalStatus::Delivered, ExternalStatus::NotDelivered] {
                    commands.push(TripCommand::RecordAirlineOutcome {
                        status,
                        ticket_number: None,
                        reason_code: None,
                    });
                }
            }
            TripStatus::Notified | TripStatus::NotNotified | TripStatus::Withdrawn => {}
        }
        if state.status.is_editable() {
            // Always refused: a blank name, which proves a refusal leaves the state
            // untouched.
            commands.push(TripCommand::SetName {
                value: String::new(),
            });
        }
        commands
    }

    fn simulate(
        &self,
        state: Option<&TripState>,
        command: &TripCommand,
    ) -> SimulatedTransition<TripState, TripEvent> {
        simulate(&self.workflow, state, command)
    }

    fn declared_outcomes(&self) -> Vec<TripOutcome> {
        vec![
            TripOutcome::Withdrawn,
            TripOutcome::Notified,
            TripOutcome::NotNotified,
        ]
    }
}
