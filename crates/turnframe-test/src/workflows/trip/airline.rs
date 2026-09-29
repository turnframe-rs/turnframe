//! The sample airline: how it answers a rebooking sent to it.
//!
//! A rebooking leaves the system through the outbox; what comes back is the
//! airline's business. The sample can answer, refuse, or never answer, and the
//! last is the case that matters: the outcome is unknown, so nothing may be
//! claimed until an answer arrives.

use serde::{Deserialize, Serialize};
use turnframe_core::event::ExternalStatus;

use crate::workflows::trip::command::TripCommand;
use crate::workflows::trip::model::{SAMPLE_REFUSAL_CODE, SAMPLE_TICKET_NUMBER};

/// How the sample airline answers a rebooking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AirlineMode {
    /// It confirms and issues the new ticket.
    #[default]
    Answers,
    /// It refuses: the seat is gone.
    Rejects,
    /// It never answers.
    NeverAnswers,
}

/// The command the airline's answer applies to the case, or `None` when it does
/// not answer.
#[must_use]
pub fn airline_answer(mode: AirlineMode) -> Option<TripCommand> {
    match mode {
        AirlineMode::Answers => Some(TripCommand::RecordAirlineOutcome {
            status: ExternalStatus::Accepted,
            ticket_number: Some(SAMPLE_TICKET_NUMBER.to_owned()),
            reason_code: None,
        }),
        AirlineMode::Rejects => Some(TripCommand::RecordAirlineOutcome {
            status: ExternalStatus::Rejected,
            ticket_number: None,
            reason_code: Some(SAMPLE_REFUSAL_CODE.to_owned()),
        }),
        AirlineMode::NeverAnswers => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_airline_gives_nothing_to_record() {
        assert_eq!(airline_answer(AirlineMode::NeverAnswers), None);
        assert!(matches!(
            airline_answer(AirlineMode::Answers),
            Some(TripCommand::RecordAirlineOutcome {
                status: ExternalStatus::Accepted,
                ..
            })
        ));
    }
}
