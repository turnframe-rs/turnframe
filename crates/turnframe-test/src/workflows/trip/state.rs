//! Persisted state, phases, obligations and outcomes of the trip sample.

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use turnframe_core::event::ExternalStatus;
use uuid::Uuid;

/// Who pays for one extra. An extra without a payer is an open obligation,
/// which is what makes the obligation parameterized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Payer {
    /// The traveler pays and is not reimbursed.
    Traveler,
    /// The traveler's company pays.
    Company,
    /// The airline pays, because the disruption is its own.
    Airline,
}

/// An extra as the user describes it, before the domain gives it an identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewExtra {
    /// What the extra is.
    pub description: String,
    /// How many, at least one.
    pub quantity: u32,
    /// Price of one in cents, never negative.
    pub unit_price_cents: i64,
}

/// An extra the case adds to the booking: a bag, a seat, a meal, a night.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extra {
    /// Stable identifier; the parameter of [`TripObligation::AssignPayer`].
    pub extra_id: Uuid,
    /// What the extra is.
    pub description: String,
    /// How many.
    pub quantity: u32,
    /// Price of one in cents.
    pub unit_price_cents: i64,
    /// Who pays; `None` while the obligation is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<Payer>,
}

impl Extra {
    /// Total of the extra in cents, saturating instead of overflowing.
    #[must_use]
    pub fn total_cents(&self) -> i64 {
        i64::from(self.quantity).saturating_mul(self.unit_price_cents)
    }
}

/// Whether a leg flies as booked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LegStatus {
    /// It flies as booked.
    #[default]
    OnTime,
    /// It flies late.
    Delayed,
    /// It does not fly.
    Cancelled,
}

/// One flight of the booking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leg {
    /// Its number in the booking, from 1.
    pub number: u32,
    /// The flight, such as `AZ610`.
    pub flight: String,
    /// Where it leaves from.
    pub from: String,
    /// Where it lands.
    pub to: String,
    /// When it leaves, as the booking shows it.
    pub departs: String,
    /// Whether it flies as booked.
    pub status: LegStatus,
    /// Whether the traveler asked to keep it as it is; a protected leg is never changed.
    #[serde(default)]
    pub protected: bool,
}

/// The rebooking the airline quoted for one leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// The leg it replaces.
    pub leg: u32,
    /// The new flight.
    pub flight: String,
    /// When the new flight leaves.
    pub departs: String,
    /// What it costs beyond the ticket already paid, in cents.
    pub fare_difference_cents: i64,
}

/// The traveler the case is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TripTraveler {
    /// Identifier in the application's traveler registry.
    pub traveler_id: Uuid,
    /// Server-authored label, safe to show on a card.
    pub display_name: String,
}

/// The lifecycle status stored on the case.
///
/// The status is persisted; the [`TripPhase`] is projected from it together
/// with the open obligations, so the two never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TripStatus {
    /// Being filled in.
    #[default]
    Draft,
    /// A rebooking is quoted, waiting for the user to confirm it.
    AwaitingRebookingConfirmation,
    /// Sent to the airline; no local edit may happen.
    Rebooking,
    /// The airline confirmed and issued the new ticket.
    Ticketed,
    /// The airline refused the rebooking, and another can be asked for.
    Refused,
    /// The traveler was sent the new ticket.
    Notified,
    /// The traveler could not be reached; the ticket stands as issued.
    NotNotified,
    /// Withdrawn before a rebooking left the system.
    Withdrawn,
}

impl TripStatus {
    /// Returns `true` while the case may still be edited.
    #[must_use]
    pub fn is_editable(self) -> bool {
        matches!(
            self,
            Self::Draft | Self::AwaitingRebookingConfirmation | Self::Refused
        )
    }

    /// Returns `true` once a rebooking has left the system, so no local edit or
    /// withdrawal can undo it.
    #[must_use]
    pub fn is_sent(self) -> bool {
        matches!(
            self,
            Self::Rebooking | Self::Ticketed | Self::Notified | Self::NotNotified
        )
    }
}

/// The persisted disruption case of one booking.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TripState {
    /// The traveler, once chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traveler: Option<TripTraveler>,
    /// What the traveler calls this trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The day the traveler would rather fly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub travel_date: Option<NaiveDate>,
    /// The flights of the booking, in order.
    #[serde(default)]
    pub legs: Vec<Leg>,
    /// The extras, in the order they were added.
    #[serde(default)]
    pub extras: Vec<Extra>,
    /// The rebooking the airline quoted, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<Offer>,
    /// Lifecycle status.
    pub status: TripStatus,
    /// Where the rebooking stands with the airline, never collapsed into "done".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_status: Option<ExternalStatus>,
    /// The new ticket's number, once the airline issued it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_number: Option<String>,
    /// Stable reason code of the airline's last refusal, never free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_code: Option<String>,
}

impl TripState {
    /// The obligations open in this state, in a stable order.
    ///
    /// The per-extra payer obligation is parameterized by the extra's identifier,
    /// so two extras without a payer are two distinct obligations.
    #[must_use]
    pub fn open_obligations(&self) -> Vec<TripObligation> {
        if !self.status.is_editable() {
            return Vec::new();
        }
        let mut obligations = Vec::new();
        if self.traveler.is_none() {
            obligations.push(TripObligation::SelectTraveler);
        }
        for extra in &self.extras {
            if extra.payer.is_none() {
                obligations.push(TripObligation::AssignPayer {
                    extra_id: extra.extra_id,
                });
            }
        }
        if self.name.is_none() {
            obligations.push(TripObligation::SetName);
        }
        if self.travel_date.is_none() {
            obligations.push(TripObligation::SetTravelDate);
        }
        obligations
    }

    /// Returns `true` when nothing is missing and a rebooking may be asked for.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.status.is_editable() && self.open_obligations().is_empty()
    }

    /// Total of every extra in cents.
    #[must_use]
    pub fn extras_total_cents(&self) -> i64 {
        self.extras.iter().fold(0_i64, |total, extra| {
            total.saturating_add(extra.total_cents())
        })
    }

    /// The extra with this identifier, if any.
    #[must_use]
    pub fn extra(&self, extra_id: Uuid) -> Option<&Extra> {
        self.extras.iter().find(|extra| extra.extra_id == extra_id)
    }

    /// The leg with this number, if any.
    #[must_use]
    pub fn leg(&self, number: u32) -> Option<&Leg> {
        self.legs.iter().find(|leg| leg.number == number)
    }
}

/// Exactly one lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TripPhase {
    /// The case does not exist yet.
    PreDraft,
    /// Obligations are open and the user is filling them in.
    Collecting,
    /// A rebooking is quoted; its card is waiting for a click.
    AwaitingRebookingConfirmation,
    /// The airline has the rebooking.
    Dispatching,
    /// The airline refused it, and another can be asked for.
    Refused,
    /// Ticketed, waiting for the traveler to be told.
    Ticketed,
    /// The traveler has the new ticket.
    Notified,
    /// The traveler could not be reached.
    NotNotified,
    /// Withdrawn before a rebooking was sent.
    Withdrawn,
}

/// An open obligation, possibly parameterized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TripObligation {
    /// No traveler chosen yet.
    SelectTraveler,
    /// One specific extra has no payer.
    AssignPayer {
        /// The extra that needs one.
        extra_id: Uuid,
    },
    /// No name yet.
    SetName,
    /// No travel date yet.
    SetTravelDate,
}

/// A terminal outcome, present only when the case is really complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TripOutcome {
    /// Withdrawn before a rebooking was sent.
    Withdrawn,
    /// The traveler has the new ticket.
    Notified,
    /// Ticketed but the traveler was never reached.
    NotNotified,
}
