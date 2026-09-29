//! Typed claim commands, events and model-facing argument shapes.
//!
//! The interesting line here is that [`ClaimCommand::ReviseProposedField`] and
//! [`ClaimCommand::AcceptProposal`] are two commands with two operation keys
//! and two policies. Editing a proposed value and answering the review are
//! different acts, and keeping them apart in the command vocabulary is what
//! makes them distinguishable everywhere downstream: in the plan, in the
//! journal, in the receipt and in the replay record.

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use turnframe_core::ids::AttachmentId;
use turnframe_core::operation::Money;

use crate::workflows::claim::state::{ClaimField, ProposedField, RecordedField};

/// Keys of the operations the interpreter may propose.
///
/// [`operations`] deliberately has no key for proposing fields: extraction is
/// something the application does to a document, not something a user asks for
/// in a sentence. The command exists, the operation does not, so the
/// interpreter cannot invent a reading of a document that nobody performed.
pub mod operations {
    /// Start an claim case.
    pub const CREATE_DRAFT: &str = "claim.create_draft";
    /// Set the hand-typed reference, which no proposal covers.
    pub const SET_REFERENCE: &str = "claim.set_reference";
    /// Attach the document.
    pub const ATTACH_RECEIPT: &str = "claim.attach_receipt";
    /// Change one proposed value while the review is open.
    pub const REVISE_PROPOSED_FIELD: &str = "claim.revise_proposed_field";
    /// Accept the proposal: the proposed values become the record.
    pub const ACCEPT_PROPOSAL: &str = "claim.accept_proposal";
    /// Throw the reading away and keep the document.
    pub const ABANDON_REVIEW: &str = "claim.abandon_review";
    /// Throw the document away.
    pub const DISCARD_RECEIPT: &str = "claim.discard_receipt";
}

/// One typed claim command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimCommand {
    /// Bring the case into existence.
    CreateDraft,
    /// Set the hand-typed reference.
    SetReference {
        /// The reference.
        value: String,
    },
    /// Record that a document arrived.
    AttachReceipt {
        /// The document.
        attachment_id: AttachmentId,
    },
    /// Offer values read from the attached document for review.
    ///
    /// Server-issued: it carries what an extractor found, so its origin is an
    /// internal policy or a verified callback, never a user's sentence.
    ProposeFields {
        /// The document the values were read from.
        attachment_id: AttachmentId,
        /// What was read.
        fields: Vec<ProposedField>,
    },
    /// Change one proposed value while the review is open.
    ///
    /// Not an answer to the review: the card stays open, the proposal stays a
    /// proposal, and the value is marked as edited by a human.
    ReviseProposedField {
        /// Which field.
        field: ClaimField,
        /// The value the user wants instead.
        value: String,
    },
    /// Answer the review by accepting it: the proposal becomes the record.
    AcceptProposal,
    /// Answer the review by throwing the reading away, keeping the document.
    AbandonReview,
    /// Throw the document away without recording anything.
    DiscardReceipt,
}

/// One committed claim event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimEvent {
    /// The case came into existence.
    DraftCreated,
    /// The hand-typed reference changed.
    ReferenceSet {
        /// The new reference.
        value: String,
    },
    /// A document arrived.
    ReceiptAttached {
        /// The document.
        attachment_id: AttachmentId,
    },
    /// Values were read out of the document and offered for review.
    FieldsProposed {
        /// The document they were read from.
        attachment_id: AttachmentId,
        /// What was read.
        fields: Vec<ProposedField>,
    },
    /// The user changed one proposed value.
    ProposedFieldRevised {
        /// Which field.
        field: ClaimField,
        /// What was proposed before, when the field had a value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous: Option<String>,
        /// What the user put there.
        value: String,
    },
    /// The review was accepted and the values became the record.
    ProposalAccepted {
        /// The document the values came from.
        attachment_id: AttachmentId,
        /// What was recorded.
        fields: Vec<RecordedField>,
    },
    /// The reading was thrown away; the document stayed.
    ProposalAbandoned {
        /// The document whose reading was thrown away.
        attachment_id: AttachmentId,
    },
    /// The document was thrown away.
    ReceiptDiscarded,
}

impl ClaimEvent {
    /// The stable event type label stored on the committed event.
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::DraftCreated => "claim.draft_created",
            Self::ReferenceSet { .. } => "claim.reference_set",
            Self::ReceiptAttached { .. } => "claim.receipt_attached",
            Self::FieldsProposed { .. } => "claim.fields_proposed",
            Self::ProposedFieldRevised { .. } => "claim.proposed_field_revised",
            Self::ProposalAccepted { .. } => "claim.proposal_accepted",
            Self::ProposalAbandoned { .. } => "claim.proposal_abandoned",
            Self::ReceiptDiscarded => "claim.receipt_discarded",
        }
    }
}

/// Arguments of [`operations::SET_REFERENCE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReferenceArgs {
    /// The reference to store.
    pub value: String,
}

/// Arguments of [`operations::ATTACH_RECEIPT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachArgs {
    /// The document that arrived with the turn.
    pub attachment_id: String,
}

/// Arguments of [`operations::REVISE_PROPOSED_FIELD`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviseArgs {
    /// Which proposed field the user is correcting.
    pub field: ClaimField,
    /// The merchant's name, when that is the field.
    #[serde(default)]
    pub merchant: Option<String>,
    /// The total, when that is the field.
    #[serde(default)]
    pub total: Option<Money>,
    /// The document's date, when that is the field.
    #[serde(default)]
    pub date: Option<NaiveDate>,
}
