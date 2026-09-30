//! Typed trip commands, events and model-facing argument shapes.

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use turnframe_core::event::ExternalStatus;
use turnframe_core::operation::Money;
use uuid::Uuid;

use crate::workflows::trip::state::{NewExtra, Payer, TripTraveler};

/// Keys of the operations the interpreter may propose.
pub mod operations {
    /// Open the disruption case.
    pub const OPEN: &str = "trip.open";
    /// Name the trip.
    pub const SET_NAME: &str = "trip.set_name";
    /// Set the day the traveler would rather fly.
    pub const SET_TRAVEL_DATE: &str = "trip.set_travel_date";
    /// Add an extra.
    pub const ADD_EXTRA: &str = "trip.add_extra";
    /// Say who pays for an extra.
    pub const ASSIGN_PAYER: &str = "trip.assign_payer";
    /// Change an extra's description, quantity or unit price.
    pub const CHANGE_EXTRA: &str = "trip.change_extra";
    /// Choose the traveler of a case that has none yet.
    pub const SET_TRAVELER: &str = "trip.set_traveler";
    /// Put another traveler on the case.
    pub const CHANGE_TRAVELER: &str = "trip.change_traveler";
    /// Keep a leg as it is.
    pub const PROTECT_LEG: &str = "trip.protect_leg";
    /// Ask for the rebooking card of the quoted offer.
    pub const REQUEST_REBOOKING: &str = "trip.request_rebooking";
    /// Send the rebooking to the airline.
    pub const REBOOK: &str = "trip.rebook";
    /// Withdraw the case before a rebooking is sent.
    pub const WITHDRAW: &str = "trip.withdraw";
    /// Acknowledges the card on screen; only the card may address it.
    pub const ACKNOWLEDGE_CARD: &str = "trip.acknowledge_card";
}

/// One typed trip command.
///
/// Every command names what it does: policy, validation and receipts all key off
/// the variant. `Requote` and `RecordAirlineOutcome` come from the airline, never
/// from the interpreter, so no operation compiles to them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TripCommand {
    /// Bring the case into existence, on the sample booking.
    Open,
    /// Bring the case into existence, for its traveler.
    OpenFor {
        /// The traveler.
        traveler: TripTraveler,
    },
    /// Name the trip.
    SetName {
        /// The name.
        value: String,
    },
    /// Set the day the traveler would rather fly.
    SetTravelDate {
        /// The day.
        value: NaiveDate,
    },
    /// Add an extra.
    AddExtra {
        /// The extra.
        extra: NewExtra,
    },
    /// Say who pays for an extra.
    AssignPayer {
        /// The extra.
        extra_id: Uuid,
        /// Who pays.
        payer: Payer,
    },
    /// Change an extra already on the case; what is not given stays as it was.
    ChangeExtra {
        /// The extra.
        extra_id: Uuid,
        /// Its new description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Its new quantity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quantity: Option<u32>,
        /// Its new unit price in cents.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit_price_cents: Option<i64>,
    },
    /// Put another traveler on the case. Carries the server-authored label as well
    /// as the identifier so cards never have to read it again.
    ChangeTraveler {
        /// The new traveler.
        traveler: TripTraveler,
    },
    /// Keep a leg as it is: nothing may change it from now on.
    ProtectLeg {
        /// The leg's number.
        leg: u32,
    },
    /// The airline quoted, or quoted again, the rebooking of a leg.
    Requote {
        /// The leg it replaces.
        leg: u32,
        /// The new flight.
        flight: String,
        /// When it leaves.
        departs: String,
        /// What it costs beyond the ticket already paid, in cents.
        fare_difference_cents: i64,
    },
    /// Move to the rebooking confirmation phase for a leg.
    RequestRebooking {
        /// The leg's number.
        leg: u32,
    },
    /// Send the rebooking to the airline.
    Rebook,
    /// Withdraw the case before a rebooking is sent.
    Withdraw,
    /// Record what the airline said about the rebooking.
    RecordAirlineOutcome {
        /// Where the rebooking now stands.
        status: ExternalStatus,
        /// The new ticket's number, when the airline issued one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ticket_number: Option<String>,
        /// Stable reason code of a refusal, never free text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason_code: Option<String>,
    },
}

/// One committed trip event: the claim ledger of the sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TripEvent {
    /// The case came into existence.
    Opened,
    /// The trip was named.
    NameSet {
        /// The new name.
        value: String,
    },
    /// The travel date changed.
    TravelDateSet {
        /// The new date.
        value: NaiveDate,
    },
    /// An extra was added.
    ExtraAdded {
        /// Identifier of the new extra.
        extra_id: Uuid,
        /// Its description.
        description: String,
        /// Its quantity.
        quantity: u32,
        /// Its unit price in cents.
        unit_price_cents: i64,
        /// Who pays for it, when it was said as it was added.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payer: Option<Payer>,
    },
    /// An extra was given its payer.
    PayerAssigned {
        /// The extra.
        extra_id: Uuid,
        /// Who pays.
        payer: Payer,
    },
    /// An extra was changed; each field given is its new value.
    ExtraChanged {
        /// The extra.
        extra_id: Uuid,
        /// Its new description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Its new quantity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quantity: Option<u32>,
        /// Its new unit price in cents.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit_price_cents: Option<i64>,
    },
    /// Another traveler was put on the case.
    TravelerChanged {
        /// The previous traveler, when there was one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_traveler_id: Option<Uuid>,
        /// The new traveler.
        traveler_id: Uuid,
        /// The new traveler's name.
        #[serde(default)]
        display_name: String,
    },
    /// A leg is kept as it is.
    LegProtected {
        /// The leg's number.
        leg: u32,
    },
    /// The airline quoted a rebooking.
    OfferQuoted {
        /// The leg it replaces.
        leg: u32,
        /// The new flight.
        flight: String,
        /// When it leaves.
        departs: String,
        /// What it costs beyond the ticket already paid, in cents.
        fare_difference_cents: i64,
    },
    /// The rebooking confirmation was requested.
    RebookingRequested {
        /// The leg.
        leg: u32,
    },
    /// The rebooking was sent to the airline.
    RebookingSent,
    /// The case was withdrawn.
    Withdrawn,
    /// The airline reported where the rebooking stands.
    AirlineOutcomeRecorded {
        /// Where it stands.
        status: ExternalStatus,
        /// The new ticket's number, when issued.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ticket_number: Option<String>,
        /// Stable reason code of a refusal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason_code: Option<String>,
    },
}

impl TripEvent {
    /// The stable event type label stored on the committed event.
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Opened => "trip.opened",
            Self::NameSet { .. } => "trip.name_set",
            Self::TravelDateSet { .. } => "trip.travel_date_set",
            Self::ExtraAdded { .. } => "trip.extra_added",
            Self::PayerAssigned { .. } => "trip.payer_assigned",
            Self::ExtraChanged { .. } => "trip.extra_changed",
            Self::TravelerChanged { .. } => "trip.traveler_changed",
            Self::LegProtected { .. } => "trip.leg_protected",
            Self::OfferQuoted { .. } => "trip.offer_quoted",
            Self::RebookingRequested { .. } => "trip.rebooking_requested",
            Self::RebookingSent => "trip.rebooking_sent",
            Self::Withdrawn => "trip.withdrawn",
            Self::AirlineOutcomeRecorded { .. } => "trip.airline_outcome_recorded",
        }
    }
}

/// Arguments of [`operations::SET_NAME`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetNameArgs {
    /// What the traveler calls the trip.
    pub value: String,
}

/// Arguments of [`operations::SET_TRAVEL_DATE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTravelDateArgs {
    /// The day the traveler would rather fly.
    pub value: NaiveDate,
}

/// Arguments of [`operations::ADD_EXTRA`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddExtraArgs {
    /// What the extra is.
    pub description: String,
    /// How many.
    pub quantity: u32,
    /// Price of one.
    pub unit_price: Money,
    /// Who pays, when the message says so.
    #[serde(default)]
    pub payer: Option<Payer>,
}

/// Arguments of [`operations::ASSIGN_PAYER`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssignPayerArgs {
    /// The extra's number, as the record lists it, from 1.
    pub extra: u32,
    /// Who pays.
    pub payer: Payer,
}

/// Arguments of [`operations::CHANGE_EXTRA`]: the extra, and what changes on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeExtraArgs {
    /// The extra's number, as the record lists it, from 1.
    pub extra: u32,
    /// Its new description.
    #[serde(default)]
    pub description: Option<String>,
    /// Its new quantity.
    #[serde(default)]
    pub quantity: Option<u32>,
    /// Its new unit price.
    #[serde(default)]
    pub unit_price: Option<Money>,
}

/// Arguments of [`operations::OPEN`]: the traveler, when the user named one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenArgs {
    /// The traveler record.
    #[serde(default)]
    pub traveler: Option<TravelerRecord>,
}

/// Arguments of [`operations::SET_TRAVELER`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTravelerArgs {
    /// The traveler record.
    pub traveler: TravelerRecord,
}

/// A traveler record, as a record argument reaches the workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TravelerRecord {
    /// Its identifier.
    pub case_id: String,
    /// Its name, when it was in view.
    #[serde(default)]
    pub label: Option<String>,
}

/// Arguments of [`operations::CHANGE_TRAVELER`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeTravelerArgs {
    /// Who travels instead, as the user names them.
    pub traveler: String,
}

/// Arguments of [`operations::PROTECT_LEG`] and [`operations::REQUEST_REBOOKING`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LegArgs {
    /// The leg's number, as the record lists it, from 1.
    pub leg: u32,
}
