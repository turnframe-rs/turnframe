//! The exploration model and the ready-made fixtures of the claim sample.

use turnframe_core::ids::AttachmentId;

use crate::explore::{SimulatedTransition, WorkflowModel};
use crate::workflows::claim::command::{ClaimCommand, ClaimEvent};
use crate::workflows::claim::definition::ClaimWorkflow;
use crate::workflows::claim::state::{
    ClaimField, ClaimOutcome, ClaimState, ClaimStatus, Proposal, ProposedField,
};
use crate::workflows::simulate;

/// The document every fixture reads from.
pub const SAMPLE_ATTACHMENT: &str = "att-receipt-1";

/// The reference the user types by hand. It belongs to no proposal.
pub const SAMPLE_REFERENCE: &str = "PO-2026-0042";

/// A second reference, so a test can change one without touching a proposal.
pub const OTHER_REFERENCE: &str = "PO-2026-0043";

/// Merchant the extractor reads off the document.
pub const EXTRACTED_MERCHANT: &str = "Hotel Tejo";

/// Total the extractor reads off the document.
pub const EXTRACTED_TOTAL: &str = "125000";

/// Date the extractor reads off the document.
pub const EXTRACTED_DATE: &str = "2026-09-01";

/// The merchant a user types when the extractor got it wrong.
pub const CORRECTED_MERCHANT: &str = "Hotel Tejo Lisboa";

/// The document identifier the fixtures use.
#[must_use]
pub fn sample_attachment() -> AttachmentId {
    AttachmentId::from(SAMPLE_ATTACHMENT)
}

fn proposed(field: ClaimField, value: &str) -> ProposedField {
    ProposedField {
        field,
        value: value.to_owned(),
        edited: false,
    }
}

/// A reading that found every required field.
#[must_use]
pub fn complete_proposal() -> Proposal {
    Proposal::new(
        sample_attachment(),
        vec![
            proposed(ClaimField::Merchant, EXTRACTED_MERCHANT),
            proposed(ClaimField::Total, EXTRACTED_TOTAL),
            proposed(ClaimField::ReceiptDate, EXTRACTED_DATE),
        ],
    )
}

/// A reading that could not find the date: one field is proposed short.
#[must_use]
pub fn partial_proposal() -> Proposal {
    Proposal::new(
        sample_attachment(),
        vec![
            proposed(ClaimField::Merchant, EXTRACTED_MERCHANT),
            proposed(ClaimField::Total, EXTRACTED_TOTAL),
        ],
    )
}

/// A case with a document attached and nothing read out of it yet.
#[must_use]
pub fn awaiting_extraction() -> ClaimState {
    ClaimState {
        attachment: Some(sample_attachment()),
        ..ClaimState::default()
    }
}

/// A case whose review card is open on `proposal`.
#[must_use]
pub fn under_review(proposal: Proposal) -> ClaimState {
    ClaimState {
        proposal: Some(proposal),
        ..awaiting_extraction()
    }
}

/// A case that reached the review with a reference already typed in.
///
/// The reference is the part of the state the review card is *not* about, so
/// this is the fixture the payload-hash test varies.
#[must_use]
pub fn under_review_with_reference(proposal: Proposal, reference: &str) -> ClaimState {
    ClaimState {
        reference: Some(reference.to_owned()),
        ..under_review(proposal)
    }
}

/// A case whose reading was thrown away, keeping the document.
#[must_use]
pub fn abandoned() -> ClaimState {
    ClaimState {
        abandoned_from: Some(sample_attachment()),
        ..awaiting_extraction()
    }
}

/// The exploration model of the claim workflow.
///
/// Every value comes from a fixed table and the proposals are two constants, so
/// the reachable state space is finite and identical on every run. The model
/// deliberately offers a refused command in each phase, since the explorer
/// checks that a refusal changes nothing.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct ClaimModel {
    /// The definition transitions are applied through.
    pub workflow: ClaimWorkflow,
}

impl ClaimModel {
    /// Builds the model.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            workflow: ClaimWorkflow::new(),
        }
    }
}

impl WorkflowModel<ClaimWorkflow> for ClaimModel {
    fn initial_states(&self) -> Vec<Option<ClaimState>> {
        vec![None]
    }

    fn candidate_commands(&self, state: Option<&ClaimState>) -> Vec<ClaimCommand> {
        let Some(state) = state else {
            return vec![ClaimCommand::CreateDraft];
        };
        if state.status != ClaimStatus::Draft {
            return Vec::new();
        }
        let mut commands = Vec::new();
        if state.reference.is_none() {
            commands.push(ClaimCommand::SetReference {
                value: SAMPLE_REFERENCE.to_owned(),
            });
        }
        match (state.attachment.as_ref(), state.proposal.as_ref()) {
            (None, _) => {
                commands.push(ClaimCommand::AttachReceipt {
                    attachment_id: sample_attachment(),
                });
                // Refused: nothing has arrived, so nothing can have been read.
                commands.push(ClaimCommand::AcceptProposal);
            }
            (Some(_), None) => {
                for proposal in [complete_proposal(), partial_proposal()] {
                    commands.push(ClaimCommand::ProposeFields {
                        attachment_id: proposal.attachment_id.clone(),
                        fields: proposal.fields.clone(),
                    });
                }
                // Refused: no review is open.
                commands.push(ClaimCommand::AbandonReview);
            }
            (Some(_), Some(proposal)) => {
                if proposal
                    .field(ClaimField::Merchant)
                    .is_some_and(|field| field.value != CORRECTED_MERCHANT)
                {
                    commands.push(ClaimCommand::ReviseProposedField {
                        field: ClaimField::Merchant,
                        value: CORRECTED_MERCHANT.to_owned(),
                    });
                }
                // Filling a value the document did not yield is the same act as
                // correcting one it did.
                for missing in proposal.missing() {
                    commands.push(ClaimCommand::ReviseProposedField {
                        field: missing,
                        value: EXTRACTED_DATE.to_owned(),
                    });
                }
                commands.push(ClaimCommand::AcceptProposal);
                commands.push(ClaimCommand::AbandonReview);
                // Refused: a second reading may not replace an open proposal.
                commands.push(ClaimCommand::ProposeFields {
                    attachment_id: sample_attachment(),
                    fields: complete_proposal().fields,
                });
            }
        }
        commands.push(ClaimCommand::DiscardReceipt);
        // Always refused: a blank reference.
        commands.push(ClaimCommand::SetReference {
            value: "   ".to_owned(),
        });
        commands
    }

    fn simulate(
        &self,
        state: Option<&ClaimState>,
        command: &ClaimCommand,
    ) -> SimulatedTransition<ClaimState, ClaimEvent> {
        simulate(&self.workflow, state, command)
    }

    fn declared_outcomes(&self) -> Vec<ClaimOutcome> {
        vec![ClaimOutcome::Recorded, ClaimOutcome::Discarded]
    }
}
