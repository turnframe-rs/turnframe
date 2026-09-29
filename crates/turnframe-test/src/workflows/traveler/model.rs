//! The exploration model and the ready-made fixtures of the traveler sample.

use crate::explore::{SimulatedTransition, WorkflowModel};
use crate::workflows::simulate;
use crate::workflows::traveler::command::{TravelerCommand, TravelerEvent};
use crate::workflows::traveler::definition::TravelerWorkflow;
use crate::workflows::traveler::state::{
    DeclineReason, FieldState, TravelerOutcome, TravelerState, TravelerStatus,
};

/// Full name the model sets.
pub const SAMPLE_NAME: &str = "Marta Bianchi";

/// Contact address the model sets first.
pub const SAMPLE_EMAIL: &str = "marta@aurora.example";

/// Address the model changes to, exercising the sensitive change.
pub const OTHER_EMAIL: &str = "marta.bianchi@aurora.example";

/// Loyalty number the model sets.
pub const SAMPLE_LOYALTY_NUMBER: &str = "AZ1234567";

/// A draft with a name but no address and no loyalty number.
#[must_use]
pub fn incomplete_draft() -> TravelerState {
    TravelerState {
        full_name: Some(SAMPLE_NAME.to_owned()),
        ..TravelerState::default()
    }
}

/// A draft with every field set, waiting for the activation card.
#[must_use]
pub fn awaiting_activation() -> TravelerState {
    TravelerState {
        full_name: Some(SAMPLE_NAME.to_owned()),
        email: Some(SAMPLE_EMAIL.to_owned()),
        loyalty_number: FieldState::answered(SAMPLE_LOYALTY_NUMBER),
        status: TravelerStatus::Draft,
    }
}

/// A draft that is complete because the loyalty number was **declined**, not given.
///
/// It projects to the same phase as [`awaiting_activation`] and carries the
/// same empty obligation list, which is the point: the two states are only
/// distinguishable through the notice the projection attaches, and a domain
/// that dropped the reason could no longer tell them apart at all.
#[must_use]
pub fn declined_loyalty_number(reason: DeclineReason) -> TravelerState {
    TravelerState {
        loyalty_number: FieldState::declined(reason),
        ..awaiting_activation()
    }
}

/// An active traveler.
#[must_use]
pub fn active_traveler() -> TravelerState {
    TravelerState {
        status: TravelerStatus::Active,
        ..awaiting_activation()
    }
}

/// The exploration model of the traveler workflow.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct TravelerModel {
    /// The definition transitions are applied through.
    pub workflow: TravelerWorkflow,
}

impl TravelerModel {
    /// Builds the model.
    #[must_use]
    pub const fn new() -> Self {
        Self::of(TravelerWorkflow::new())
    }

    /// The model of `workflow`, such as [`TravelerWorkflow::with_cards`].
    #[must_use]
    pub const fn of(workflow: TravelerWorkflow) -> Self {
        Self { workflow }
    }
}

impl WorkflowModel<TravelerWorkflow> for TravelerModel {
    fn initial_states(&self) -> Vec<Option<TravelerState>> {
        vec![None]
    }

    fn candidate_commands(&self, state: Option<&TravelerState>) -> Vec<TravelerCommand> {
        let Some(state) = state else {
            return vec![TravelerCommand::CreateDraft];
        };
        let mut commands = Vec::new();
        match state.status {
            TravelerStatus::Draft => {
                if state.full_name.is_none() {
                    commands.push(TravelerCommand::SetName {
                        value: SAMPLE_NAME.to_owned(),
                    });
                }
                if state.email.is_none() {
                    commands.push(TravelerCommand::ChangeEmail {
                        value: SAMPLE_EMAIL.to_owned(),
                    });
                }
                // The three-valued field, explored through all three states:
                // an unanswered question can be answered or declined, and a
                // decline can still be superseded by an answer later.
                if !state.loyalty_number.is_answered() {
                    commands.push(TravelerCommand::SetLoyaltyNumber {
                        value: SAMPLE_LOYALTY_NUMBER.to_owned(),
                    });
                }
                match state.loyalty_number.decline_reason() {
                    None => {
                        commands.push(TravelerCommand::DeclineLoyaltyNumber {
                            reason: DeclineReason::NotApplicable,
                        });
                        commands.push(TravelerCommand::DeclineLoyaltyNumber {
                            reason: DeclineReason::Unknown,
                        });
                    }
                    Some(DeclineReason::Unknown) => {
                        // A reason can be corrected without answering.
                        commands.push(TravelerCommand::DeclineLoyaltyNumber {
                            reason: DeclineReason::Withheld,
                        });
                    }
                    Some(_) => {}
                }
                commands.push(TravelerCommand::Activate);
                commands.push(TravelerCommand::Delete);
            }
            TravelerStatus::Active => {
                if state.email.as_deref() == Some(SAMPLE_EMAIL) {
                    commands.push(TravelerCommand::ChangeEmail {
                        value: OTHER_EMAIL.to_owned(),
                    });
                }
                commands.push(TravelerCommand::Archive);
                commands.push(TravelerCommand::Delete);
            }
            TravelerStatus::Archived | TravelerStatus::Deleted => {}
        }
        if state.status.is_editable() {
            // Always refused: an address that is not one.
            commands.push(TravelerCommand::ChangeEmail {
                value: "not-an-address".to_owned(),
            });
            if state.loyalty_number.is_answered() {
                // Also always refused: declining a field the user already
                // answered would erase the answer.
                commands.push(TravelerCommand::DeclineLoyaltyNumber {
                    reason: DeclineReason::Withheld,
                });
            }
        }
        commands
    }

    fn simulate(
        &self,
        state: Option<&TravelerState>,
        command: &TravelerCommand,
    ) -> SimulatedTransition<TravelerState, TravelerEvent> {
        simulate(&self.workflow, state, command)
    }

    fn declared_outcomes(&self) -> Vec<TravelerOutcome> {
        vec![TravelerOutcome::Archived, TravelerOutcome::Deleted]
    }
}
