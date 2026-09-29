//! The pure transition function of the trip sample.
//!
//! [`validate`] is what [`WorkflowDefinition::validate_command`] runs before
//! anything executes; [`apply`] is what the executor and the exploration model
//! both run to move the state. `apply` checks `validate` first, so the two can
//! never disagree about what is allowed.
//!
//! [`WorkflowDefinition::validate_command`]: turnframe_core::flow::WorkflowDefinition::validate_command

use turnframe_core::error::DomainRejection;
use turnframe_core::event::ExternalStatus;
use turnframe_core::hash::derive_uuid;
use turnframe_core::locale::{Locale, LocalizedText};
use uuid::Uuid;

use crate::workflows::Applied;
use crate::workflows::trip::command::{TripCommand, TripEvent};
use crate::workflows::trip::model::sample_legs;
use crate::workflows::trip::state::{Extra, NewExtra, Offer, TripState, TripStatus};

/// Stable rejection codes of the trip sample.
pub mod rejection {
    /// The case does not exist yet.
    pub const NOT_FOUND: &str = "trip.not_found";
    /// The case already exists.
    pub const ALREADY_EXISTS: &str = "trip.already_exists";
    /// A rebooking left the system and the case can no longer be edited.
    pub const LOCKED: &str = "trip.locked";
    /// The name is blank.
    pub const NAME_EMPTY: &str = "trip.name_empty";
    /// The name is longer than the field allows.
    pub const NAME_TOO_LONG: &str = "trip.name_too_long";
    /// The travel date falls outside the accepted range.
    pub const TRAVEL_DATE_OUT_OF_RANGE: &str = "trip.travel_date_out_of_range";
    /// The extra has no description.
    pub const EXTRA_DESCRIPTION_EMPTY: &str = "trip.extra_description_empty";
    /// The extra has a quantity of zero.
    pub const EXTRA_QUANTITY_ZERO: &str = "trip.extra_quantity_zero";
    /// The extra has a negative unit price.
    pub const EXTRA_PRICE_NEGATIVE: &str = "trip.extra_price_negative";
    /// The case already has as many extras as it accepts.
    pub const TOO_MANY_EXTRAS: &str = "trip.too_many_extras";
    /// No extra with that identifier.
    pub const UNKNOWN_EXTRA: &str = "trip.unknown_extra";
    /// No leg with that number.
    pub const UNKNOWN_LEG: &str = "trip.unknown_leg";
    /// The leg is protected and nothing may change it.
    pub const LEG_PROTECTED: &str = "trip.leg_protected";
    /// No rebooking is quoted for that leg.
    pub const NO_OFFER: &str = "trip.no_offer";
    /// The amount is in a currency this desk does not use.
    pub const CURRENCY_NOT_ACCEPTED: &str = "trip.currency_not_accepted";
    /// The traveler cannot change once a rebooking left the system.
    pub const TRAVELER_LOCKED: &str = "trip.traveler_locked";
    /// Obligations are still open.
    pub const INCOMPLETE: &str = "trip.incomplete";
    /// The case is not waiting for a rebooking confirmation.
    pub const NOT_AWAITING_CONFIRMATION: &str = "trip.not_awaiting_confirmation";
    /// The case can no longer be withdrawn.
    pub const WITHDRAW_NOT_ALLOWED: &str = "trip.withdraw_not_allowed";
    /// The airline's status cannot follow the current one.
    pub const ILLEGAL_AIRLINE_TRANSITION: &str = "trip.illegal_airline_transition";
    /// The workflow does not compile this kind of act.
    pub const UNSUPPORTED_ACT: &str = "trip.unsupported_act";
    /// A trip name was proposed empty.
    pub const EMPTY_NAME: &str = "trip.empty_name";
    /// The arguments do not match the operation's schema.
    pub const INVALID_ARGUMENTS: &str = "trip.invalid_arguments";
}

/// Longest accepted name, in characters.
pub const MAX_NAME_CHARS: usize = 120;

/// Most extras one case accepts. A bound keeps exploration finite.
pub const MAX_EXTRAS: usize = 4;

/// Earliest accepted travel-date year.
pub const MIN_TRAVEL_YEAR: i32 = 2000;

/// Latest accepted travel-date year.
pub const MAX_TRAVEL_YEAR: i32 = 2100;

/// Domain separation of the derived extra identifiers.
const EXTRA_ID_DOMAIN: &str = "turnframe.test.trip.extra.v1";

/// Builds a rejection whose message key mirrors its code.
pub(crate) fn reject(code: &'static str) -> DomainRejection {
    let suffix = code.strip_prefix("trip.").unwrap_or(code);
    DomainRejection::new(code, format!("trip.error.{suffix}"))
}

/// The identifier the n-th extra of a case gets.
///
/// Derived from the position and the description rather than generated, so the
/// same sequence of commands always builds the same case, which lets exploration
/// deduplicate states and replay reproduce commits.
#[must_use]
pub fn extra_id_for(position: usize, description: &str) -> Uuid {
    derive_uuid(EXTRA_ID_DOMAIN, &[&position.to_string(), description])
}

/// The refusal of any change to a protected leg, with its reason.
fn protected(leg: u32) -> DomainRejection {
    reject(rejection::LEG_PROTECTED)
        .on_argument("/leg")
        .with_details(serde_json::json!({ "leg": leg }))
        .with_explanation(
            LocalizedText::new(format!(
                "Leg {leg} is kept as it is, as the traveler asked: it cannot be changed."
            ))
            .with(
                Locale::from("it-IT"),
                format!(
                    "La tratta {leg} resta com'è, come ha chiesto il viaggiatore: non si può \
                     cambiare."
                ),
            ),
        )
}

/// Checks a command against the current state without changing anything.
pub fn validate(state: Option<&TripState>, command: &TripCommand) -> Result<(), DomainRejection> {
    let Some(state) = state else {
        return match command {
            TripCommand::Open | TripCommand::OpenFor { .. } => Ok(()),
            _ => Err(reject(rejection::NOT_FOUND)),
        };
    };
    match command {
        TripCommand::Open | TripCommand::OpenFor { .. } => Err(reject(rejection::ALREADY_EXISTS)),
        TripCommand::SetName { value } => {
            editable(state)?;
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(reject(rejection::NAME_EMPTY));
            }
            if trimmed.chars().count() > MAX_NAME_CHARS {
                return Err(reject(rejection::NAME_TOO_LONG)
                    .with_details(serde_json::json!({ "max_chars": MAX_NAME_CHARS })));
            }
            Ok(())
        }
        TripCommand::SetTravelDate { value } => {
            editable(state)?;
            let year = chrono::Datelike::year(value);
            if !(MIN_TRAVEL_YEAR..=MAX_TRAVEL_YEAR).contains(&year) {
                return Err(reject(rejection::TRAVEL_DATE_OUT_OF_RANGE).with_details(
                    serde_json::json!({ "min_year": MIN_TRAVEL_YEAR, "max_year": MAX_TRAVEL_YEAR }),
                ));
            }
            Ok(())
        }
        TripCommand::AddExtra { extra } => {
            editable(state)?;
            validate_extra(state, extra)
        }
        TripCommand::AssignPayer { extra_id, .. } => {
            editable(state)?;
            if state.extra(*extra_id).is_none() {
                return Err(reject(rejection::UNKNOWN_EXTRA));
            }
            Ok(())
        }
        TripCommand::ChangeExtra {
            extra_id,
            description,
            quantity,
            unit_price_cents,
        } => {
            editable(state)?;
            if state.extra(*extra_id).is_none() {
                return Err(reject(rejection::UNKNOWN_EXTRA));
            }
            if description
                .as_deref()
                .is_some_and(|text| text.trim().is_empty())
            {
                return Err(reject(rejection::EXTRA_DESCRIPTION_EMPTY));
            }
            if *quantity == Some(0) {
                return Err(reject(rejection::EXTRA_QUANTITY_ZERO));
            }
            if unit_price_cents.is_some_and(|cents| cents < 0) {
                return Err(reject(rejection::EXTRA_PRICE_NEGATIVE));
            }
            Ok(())
        }
        TripCommand::ChangeTraveler { .. } => {
            if state.status.is_sent() {
                return Err(reject(rejection::TRAVELER_LOCKED));
            }
            editable(state)?;
            Ok(())
        }
        TripCommand::ProtectLeg { leg } => {
            editable(state)?;
            if state.leg(*leg).is_none() {
                return Err(reject(rejection::UNKNOWN_LEG));
            }
            Ok(())
        }
        TripCommand::Requote { leg, .. } => {
            editable(state)?;
            match state.leg(*leg) {
                None => Err(reject(rejection::UNKNOWN_LEG)),
                Some(found) if found.protected => Err(protected(*leg)),
                Some(_) => Ok(()),
            }
        }
        TripCommand::RequestRebooking { leg } => {
            editable(state)?;
            let Some(found) = state.leg(*leg) else {
                return Err(reject(rejection::UNKNOWN_LEG));
            };
            if found.protected {
                return Err(protected(*leg));
            }
            if state.offer.as_ref().is_none_or(|offer| offer.leg != *leg) {
                return Err(reject(rejection::NO_OFFER).with_explanation(
                    LocalizedText::new("The airline has not quoted a rebooking for this leg yet.")
                        .with(
                            Locale::from("it-IT"),
                            "La compagnia aerea non ha ancora proposto un cambio per questa \
                             tratta.",
                        ),
                ));
            }
            let open = state.open_obligations();
            if !open.is_empty() {
                return Err(reject(rejection::INCOMPLETE)
                    .with_details(serde_json::json!({ "open_obligations": open.len() }))
                    .with_explanation(
                        LocalizedText::new("The trip cannot be rebooked until it is complete.")
                            .with(
                                Locale::from("it-IT"),
                                "Il viaggio non si può cambiare finché non è completo.",
                            ),
                    ));
            }
            Ok(())
        }
        TripCommand::Rebook => {
            if state.status == TripStatus::AwaitingRebookingConfirmation {
                Ok(())
            } else {
                Err(reject(rejection::NOT_AWAITING_CONFIRMATION))
            }
        }
        TripCommand::Withdraw => {
            if state.status.is_editable() {
                Ok(())
            } else {
                Err(reject(rejection::WITHDRAW_NOT_ALLOWED))
            }
        }
        TripCommand::RecordAirlineOutcome { status, .. } => {
            if next_status_for(state.status, *status).is_some() {
                Ok(())
            } else {
                Err(reject(rejection::ILLEGAL_AIRLINE_TRANSITION))
            }
        }
    }
}

fn editable(state: &TripState) -> Result<(), DomainRejection> {
    if state.status.is_editable() {
        Ok(())
    } else {
        Err(reject(rejection::LOCKED))
    }
}

fn validate_extra(state: &TripState, extra: &NewExtra) -> Result<(), DomainRejection> {
    if extra.description.trim().is_empty() {
        return Err(reject(rejection::EXTRA_DESCRIPTION_EMPTY));
    }
    if extra.quantity == 0 {
        return Err(reject(rejection::EXTRA_QUANTITY_ZERO));
    }
    if extra.unit_price_cents < 0 {
        return Err(reject(rejection::EXTRA_PRICE_NEGATIVE));
    }
    if state.extras.len() >= MAX_EXTRAS {
        return Err(reject(rejection::TOO_MANY_EXTRAS)
            .with_details(serde_json::json!({ "max_extras": MAX_EXTRAS })));
    }
    Ok(())
}

/// The status an airline answer moves the case to, or `None` when the airline
/// cannot make that transition from here.
#[must_use]
pub fn next_status_for(current: TripStatus, reported: ExternalStatus) -> Option<TripStatus> {
    match (current, reported) {
        (TripStatus::Rebooking, ExternalStatus::ReceivedByIntermediary) => {
            Some(TripStatus::Rebooking)
        }
        (TripStatus::Rebooking, ExternalStatus::Accepted | ExternalStatus::Issued) => {
            Some(TripStatus::Ticketed)
        }
        (TripStatus::Rebooking, ExternalStatus::Rejected) => Some(TripStatus::Refused),
        (TripStatus::Ticketed, ExternalStatus::Delivered | ExternalStatus::Completed) => {
            Some(TripStatus::Notified)
        }
        (TripStatus::Ticketed, ExternalStatus::NotDelivered) => Some(TripStatus::NotNotified),
        _ => None,
    }
}

/// Applies a command, producing the next state and the events to commit.
///
/// Never mutates `state`: it works on a clone, which makes the same function
/// safe to use for exploration.
pub fn apply(
    state: Option<&TripState>,
    command: &TripCommand,
) -> Result<Applied<TripState, TripEvent>, DomainRejection> {
    validate(state, command)?;
    let Some(state) = state else {
        let opened = TripState {
            legs: sample_legs(),
            ..TripState::default()
        };
        if let TripCommand::OpenFor { traveler } = command {
            return Ok(Applied::new(
                TripState {
                    traveler: Some(traveler.clone()),
                    ..opened
                },
                vec![
                    TripEvent::Opened,
                    TripEvent::TravelerChanged {
                        previous_traveler_id: None,
                        traveler_id: traveler.traveler_id,
                        display_name: traveler.display_name.clone(),
                    },
                ],
            ));
        }
        return Ok(Applied::new(opened, vec![TripEvent::Opened]));
    };
    let mut next = state.clone();
    let event = match command {
        TripCommand::Open | TripCommand::OpenFor { .. } => {
            return Err(reject(rejection::ALREADY_EXISTS));
        }
        TripCommand::SetName { value } => {
            let value = value.trim().to_owned();
            next.name = Some(value.clone());
            reopen_for_edit(&mut next);
            TripEvent::NameSet { value }
        }
        TripCommand::SetTravelDate { value } => {
            next.travel_date = Some(*value);
            reopen_for_edit(&mut next);
            TripEvent::TravelDateSet { value: *value }
        }
        TripCommand::AddExtra { extra } => {
            let extra_id = extra_id_for(next.extras.len(), &extra.description);
            next.extras.push(Extra {
                extra_id,
                description: extra.description.clone(),
                quantity: extra.quantity,
                unit_price_cents: extra.unit_price_cents,
                payer: None,
            });
            reopen_for_edit(&mut next);
            TripEvent::ExtraAdded {
                extra_id,
                description: extra.description.clone(),
                quantity: extra.quantity,
                unit_price_cents: extra.unit_price_cents,
            }
        }
        TripCommand::AssignPayer { extra_id, payer } => {
            for extra in &mut next.extras {
                if extra.extra_id == *extra_id {
                    extra.payer = Some(*payer);
                }
            }
            reopen_for_edit(&mut next);
            TripEvent::PayerAssigned {
                extra_id: *extra_id,
                payer: *payer,
            }
        }
        TripCommand::ChangeExtra {
            extra_id,
            description,
            quantity,
            unit_price_cents,
        } => {
            for extra in &mut next.extras {
                if extra.extra_id == *extra_id {
                    if let Some(description) = description {
                        extra.description.clone_from(description);
                    }
                    if let Some(quantity) = quantity {
                        extra.quantity = *quantity;
                    }
                    if let Some(cents) = unit_price_cents {
                        extra.unit_price_cents = *cents;
                    }
                }
            }
            reopen_for_edit(&mut next);
            TripEvent::ExtraChanged {
                extra_id: *extra_id,
                description: description.clone(),
                quantity: *quantity,
                unit_price_cents: *unit_price_cents,
            }
        }
        TripCommand::ChangeTraveler { traveler } => {
            let previous_traveler_id = next.traveler.as_ref().map(|t| t.traveler_id);
            next.traveler = Some(traveler.clone());
            reopen_for_edit(&mut next);
            TripEvent::TravelerChanged {
                previous_traveler_id,
                traveler_id: traveler.traveler_id,
                display_name: traveler.display_name.clone(),
            }
        }
        // Keeping another leg leaves the card for the quoted one as it was.
        TripCommand::ProtectLeg { leg } => {
            for found in &mut next.legs {
                if found.number == *leg {
                    found.protected = true;
                }
            }
            if next.offer.as_ref().is_none_or(|offer| offer.leg == *leg) {
                reopen_for_edit(&mut next);
            }
            TripEvent::LegProtected { leg: *leg }
        }
        // A new quote keeps a pending card up, now at the new fare: the card is drawn
        // again at the new revision, and a click on the old one is stale.
        TripCommand::Requote {
            leg,
            flight,
            departs,
            fare_difference_cents,
        } => {
            next.offer = Some(Offer {
                leg: *leg,
                flight: flight.clone(),
                departs: departs.clone(),
                fare_difference_cents: *fare_difference_cents,
            });
            TripEvent::OfferQuoted {
                leg: *leg,
                flight: flight.clone(),
                departs: departs.clone(),
                fare_difference_cents: *fare_difference_cents,
            }
        }
        TripCommand::RequestRebooking { leg } => {
            next.status = TripStatus::AwaitingRebookingConfirmation;
            next.external_status = Some(ExternalStatus::AwaitingConfirmation);
            next.refusal_code = None;
            TripEvent::RebookingRequested { leg: *leg }
        }
        TripCommand::Rebook => {
            next.status = TripStatus::Rebooking;
            next.external_status = Some(ExternalStatus::Submitted);
            TripEvent::RebookingSent
        }
        TripCommand::Withdraw => {
            next.status = TripStatus::Withdrawn;
            next.external_status = None;
            TripEvent::Withdrawn
        }
        TripCommand::RecordAirlineOutcome {
            status,
            ticket_number,
            reason_code,
        } => {
            let Some(new_status) = next_status_for(next.status, *status) else {
                return Err(reject(rejection::ILLEGAL_AIRLINE_TRANSITION));
            };
            next.status = new_status;
            next.external_status = Some(*status);
            if ticket_number.is_some() {
                next.ticket_number.clone_from(ticket_number);
            }
            next.refusal_code = reason_code.clone();
            TripEvent::AirlineOutcomeRecorded {
                status: *status,
                ticket_number: ticket_number.clone(),
                reason_code: reason_code.clone(),
            }
        }
    };
    Ok(Applied::new(next, vec![event]))
}

/// Editing a case that was waiting for a rebooking confirmation takes the card
/// down: the state goes back to `Draft`, so a card is drawn again against the new
/// revision instead of confirming what the user no longer sees.
fn reopen_for_edit(state: &mut TripState) {
    if state.status == TripStatus::AwaitingRebookingConfirmation {
        state.status = TripStatus::Draft;
        state.external_status = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::trip::model::{
        awaiting_rebooking_confirmation, sample_new_extra, with_offer,
    };
    use crate::workflows::trip::state::{Payer, TripObligation, TripTraveler};

    fn opened() -> TripState {
        apply(None, &TripCommand::Open).unwrap().state
    }

    #[test]
    fn only_opening_works_on_a_case_that_does_not_exist() {
        assert!(validate(None, &TripCommand::Open).is_ok());
        assert_eq!(
            validate(None, &TripCommand::Withdraw)
                .unwrap_err()
                .code
                .as_str(),
            rejection::NOT_FOUND
        );
        let created = apply(None, &TripCommand::Open).unwrap();
        assert_eq!(created.state.legs, sample_legs());
        assert_eq!(created.events, vec![TripEvent::Opened]);
        assert_eq!(
            validate(Some(&created.state), &TripCommand::Open)
                .unwrap_err()
                .code
                .as_str(),
            rejection::ALREADY_EXISTS
        );
    }

    #[test]
    fn extra_identifiers_are_derived_and_stable() {
        let add = TripCommand::AddExtra {
            extra: sample_new_extra(0),
        };
        let first = apply(Some(&opened()), &add).unwrap();
        let again = apply(Some(&opened()), &add).unwrap();
        assert_eq!(
            first.state, again.state,
            "the same command builds the same state"
        );
        assert_eq!(
            first.state.extras[0].extra_id,
            extra_id_for(0, &sample_new_extra(0).description)
        );
    }

    #[test]
    fn an_edit_takes_a_pending_rebooking_card_down() {
        let mut ready = with_offer(1);
        ready.status = TripStatus::AwaitingRebookingConfirmation;
        ready.external_status = Some(ExternalStatus::AwaitingConfirmation);
        let edited = apply(
            Some(&ready),
            &TripCommand::SetName {
                value: " Lisbon offsite ".into(),
            },
        )
        .unwrap();
        assert_eq!(edited.state.status, TripStatus::Draft);
        assert_eq!(edited.state.external_status, None);
        assert_eq!(edited.state.name.as_deref(), Some("Lisbon offsite"));
    }

    #[test]
    fn a_new_quote_keeps_the_card_up_at_the_new_fare() {
        let mut ready = with_offer(1);
        ready.status = TripStatus::AwaitingRebookingConfirmation;
        let requoted = apply(
            Some(&ready),
            &TripCommand::Requote {
                leg: 1,
                flight: "AZ612".into(),
                departs: "2026-10-05 13:10".into(),
                fare_difference_cents: 13_200,
            },
        )
        .unwrap();
        assert_eq!(
            requoted.state.status,
            TripStatus::AwaitingRebookingConfirmation
        );
        assert_eq!(
            requoted
                .state
                .offer
                .map(|offer| offer.fare_difference_cents),
            Some(13_200)
        );
    }

    #[test]
    fn protecting_another_leg_keeps_the_rebooking_card_up() {
        let awaiting = awaiting_rebooking_confirmation();
        let kept = apply(Some(&awaiting), &TripCommand::ProtectLeg { leg: 2 })
            .unwrap()
            .state;
        assert_eq!(kept.status, TripStatus::AwaitingRebookingConfirmation);
        let own = apply(Some(&awaiting), &TripCommand::ProtectLeg { leg: 1 })
            .unwrap()
            .state;
        assert_eq!(
            own.status,
            TripStatus::Draft,
            "the card would change a kept leg"
        );
    }

    #[test]
    fn a_protected_leg_is_never_rebooked() {
        let protected = apply(Some(&with_offer(1)), &TripCommand::ProtectLeg { leg: 1 })
            .unwrap()
            .state;
        for command in [
            TripCommand::RequestRebooking { leg: 1 },
            TripCommand::Requote {
                leg: 1,
                flight: "AZ614".into(),
                departs: "2026-10-05 19:00".into(),
                fare_difference_cents: 0,
            },
        ] {
            assert_eq!(
                validate(Some(&protected), &command)
                    .unwrap_err()
                    .code
                    .as_str(),
                rejection::LEG_PROTECTED,
                "{command:?}"
            );
        }
    }

    #[test]
    fn a_sent_rebooking_refuses_local_changes() {
        let mut sent = opened();
        sent.status = TripStatus::Rebooking;
        for command in [
            TripCommand::SetName { value: "x".into() },
            TripCommand::Withdraw,
        ] {
            assert!(validate(Some(&sent), &command).is_err(), "{command:?}");
        }
        assert_eq!(
            validate(
                Some(&sent),
                &TripCommand::ChangeTraveler {
                    traveler: TripTraveler {
                        traveler_id: Uuid::nil(),
                        display_name: "Luca Ferri".into(),
                    },
                },
            )
            .unwrap_err()
            .code
            .as_str(),
            rejection::TRAVELER_LOCKED
        );
    }

    #[test]
    fn airline_answers_follow_the_ladder() {
        assert_eq!(
            next_status_for(TripStatus::Rebooking, ExternalStatus::Accepted),
            Some(TripStatus::Ticketed)
        );
        assert_eq!(
            next_status_for(TripStatus::Rebooking, ExternalStatus::Rejected),
            Some(TripStatus::Refused)
        );
        assert_eq!(
            next_status_for(TripStatus::Ticketed, ExternalStatus::NotDelivered),
            Some(TripStatus::NotNotified)
        );
        assert_eq!(
            next_status_for(TripStatus::Draft, ExternalStatus::Delivered),
            None,
            "a draft was never sent"
        );
        assert_eq!(
            next_status_for(TripStatus::Notified, ExternalStatus::Rejected),
            None,
            "a notified rebooking is final"
        );
    }

    #[test]
    fn obligations_are_parameterized_per_extra() {
        let mut state = opened();
        for position in 0..2 {
            state = apply(
                Some(&state),
                &TripCommand::AddExtra {
                    extra: sample_new_extra(position),
                },
            )
            .unwrap()
            .state;
        }
        let payers = |state: &TripState| {
            state
                .open_obligations()
                .iter()
                .filter(|o| matches!(o, TripObligation::AssignPayer { .. }))
                .count()
        };
        assert_eq!(
            payers(&state),
            2,
            "one obligation per extra without a payer"
        );
        let assigned = apply(
            Some(&state),
            &TripCommand::AssignPayer {
                extra_id: state.extras[0].extra_id,
                payer: Payer::Airline,
            },
        )
        .unwrap()
        .state;
        assert_eq!(payers(&assigned), 1);
    }
}
