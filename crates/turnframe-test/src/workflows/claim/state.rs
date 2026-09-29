//! Persisted state, phases, obligations and outcomes of the receipt-claim
//! sample.
//!
//! The type that matters here is [`Proposal`]. It is ordinary domain state —
//! nothing in the framework knows what a proposal is — and that is the whole
//! argument: a workflow that models proposed-but-not-applied values as its own
//! state gets everything the shape needs out of the vocabulary that already
//! exists.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use turnframe_core::ids::AttachmentId;

/// A field this domain derives from an incoming document.
///
/// A closed set, because it is also the parameter of an obligation and an
/// argument the interpreter fills in: free-text field names would make two
/// spellings of the same field two different obligations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimField {
    /// Who issued the receipt.
    Merchant,
    /// The amount paid, in cents, as written on the receipt.
    Total,
    /// The date on the receipt.
    ReceiptDate,
}

impl ClaimField {
    /// Every field the domain requires, in the order a card shows them.
    pub const ALL: [Self; 3] = [Self::Merchant, Self::Total, Self::ReceiptDate];

    /// The stable identifier used in obligations and diff entries: fixed, never
    /// derived from copy.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Merchant => "merchant",
            Self::Total => "total",
            Self::ReceiptDate => "receipt_date",
        }
    }

    /// Server-authored label, safe to show on a card.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Merchant => "Merchant",
            Self::Total => "Total (cents)",
            Self::ReceiptDate => "Receipt date",
        }
    }

    /// Italian label.
    #[must_use]
    pub const fn label_it(self) -> &'static str {
        match self {
            Self::Merchant => "Esercente",
            Self::Total => "Totale (centesimi)",
            Self::ReceiptDate => "Data della ricevuta",
        }
    }
}

/// One value read out of a document and offered for review.
///
/// `edited` is the difference between "this is what the extractor read" and
/// "this is what the user typed instead", and it survives into the recorded
/// state, so a later audit can tell a machine reading from a human correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedField {
    /// Which field.
    pub field: ClaimField,
    /// The value, as text: the domain records what the document said, not a
    /// parsed interpretation of it.
    pub value: String,
    /// `true` once the user changed it while the review was open.
    #[serde(default)]
    pub edited: bool,
}

/// Values derived from one document and awaiting review.
///
/// This is the "proposed values awaiting review" concept, in the only place it
/// belongs: the domain's own state. The framework's view does not need a word
/// for it, because everything the shape requires — an obligation per field, a
/// notice naming the document, a card bound to the proposal, an act that edits
/// a proposal distinct from the act that answers it — is expressible without
/// one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    /// The document the values were read from.
    pub attachment_id: AttachmentId,
    /// The proposed values, in [`ClaimField::ALL`] order.
    pub fields: Vec<ProposedField>,
}

impl Proposal {
    /// Builds a proposal, ordering the fields canonically.
    ///
    /// The order is normalized here rather than trusted from the caller so two
    /// extractions that found the same values hash the same, which is what
    /// makes the card's payload hash meaningful.
    #[must_use]
    pub fn new(attachment_id: AttachmentId, fields: Vec<ProposedField>) -> Self {
        let mut fields = fields;
        fields.sort_by_key(|proposed| proposed.field);
        Self {
            attachment_id,
            fields,
        }
    }

    /// The proposed value for a field, when there is one.
    #[must_use]
    pub fn field(&self, field: ClaimField) -> Option<&ProposedField> {
        self.fields.iter().find(|proposed| proposed.field == field)
    }

    /// Required fields this proposal does not carry, in a stable order.
    #[must_use]
    pub fn missing(&self) -> Vec<ClaimField> {
        ClaimField::ALL
            .into_iter()
            .filter(|field| self.field(*field).is_none())
            .collect()
    }

    /// Returns `true` when every required field has a proposed value.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing().is_empty()
    }

    /// How many values the user changed before answering.
    #[must_use]
    pub fn edited_count(&self) -> usize {
        self.fields
            .iter()
            .filter(|proposed| proposed.edited)
            .count()
    }
}

/// A value that has been reviewed and is now part of the record.
///
/// It keeps its provenance: which document it came from, and whether a human
/// corrected it. A recorded field is no longer a proposal — the review is over
/// — but "where did this number come from" stays answerable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedField {
    /// Which field.
    pub field: ClaimField,
    /// The accepted value.
    pub value: String,
    /// The document it was read from.
    pub from_attachment: AttachmentId,
    /// `true` when the user changed the extracted value before accepting it.
    pub corrected: bool,
}

/// The lifecycle status stored on the case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimStatus {
    /// Being assembled: a document may arrive, be read, and be reviewed.
    #[default]
    Draft,
    /// The proposal was accepted and its values are on the record.
    Recorded,
    /// The document was thrown away without being recorded.
    Discarded,
}

impl ClaimStatus {
    /// Returns `true` while the case still accepts changes.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Draft)
    }
}

/// The persisted claim case.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimState {
    /// A reference the user types by hand.
    ///
    /// It exists to be *outside* every proposal: editing it while a review is
    /// open must not change what the review card is about, which is what makes
    /// "the payload hash covers the proposal rather than the whole state" a
    /// testable sentence rather than a slogan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The document that arrived, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<AttachmentId>,
    /// Values read from the document and awaiting review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Proposal>,
    /// Values that survived a review, with their provenance.
    #[serde(default)]
    pub recorded: Vec<RecordedField>,
    /// The document whose reading the user threw away, when they did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandoned_from: Option<AttachmentId>,
    /// Lifecycle status.
    pub status: ClaimStatus,
}

impl ClaimState {
    /// The obligations open in this state, in a stable order.
    ///
    /// While a review is open there is one obligation per **proposed** field
    /// and one per **missing** required field. Both are parameterized, so three
    /// proposed values are three distinct obligations rather than one
    /// checkpoint that flickers as the user works through them.
    #[must_use]
    pub fn open_obligations(&self) -> Vec<ClaimObligation> {
        if !self.status.is_open() {
            return Vec::new();
        }
        let Some(attachment) = self.attachment.as_ref() else {
            return vec![ClaimObligation::AttachReceipt];
        };
        let Some(proposal) = self.proposal.as_ref() else {
            return vec![ClaimObligation::ExtractFields {
                attachment_id: attachment.clone(),
            }];
        };
        let mut obligations: Vec<ClaimObligation> = proposal
            .fields
            .iter()
            .map(|proposed| ClaimObligation::ReviewProposedField {
                field: proposed.field,
            })
            .collect();
        obligations.extend(
            proposal
                .missing()
                .into_iter()
                .map(|field| ClaimObligation::ProvideField { field }),
        );
        obligations
    }

    /// The recorded value of a field, when the review put one there.
    #[must_use]
    pub fn recorded_value(&self, field: ClaimField) -> Option<&str> {
        self.recorded
            .iter()
            .find(|recorded| recorded.field == field)
            .map(|recorded| recorded.value.as_str())
    }

    /// Returns `true` when a review is open.
    #[must_use]
    pub const fn is_awaiting_review(&self) -> bool {
        self.proposal.is_some() && self.status.is_open()
    }
}

/// Exactly one lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimPhase {
    /// The case does not exist yet.
    PreDraft,
    /// No document has arrived.
    AwaitingDocument,
    /// A document is here and nothing has been read out of it yet.
    Extracting,
    /// Values are proposed and the review card is waiting for an answer.
    ///
    /// The phase is user-owned, and — as the request that prompted this sample
    /// pointed out — for a reason the user did not initiate: a document
    /// arrived, not a decision.
    AwaitingReview,
    /// The values are on the record.
    Recorded,
    /// The document was thrown away.
    Discarded,
}

/// An open obligation, parameterized where the domain has more than one of it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimObligation {
    /// No document has arrived yet.
    AttachReceipt,
    /// A document is here and nothing has been read out of it.
    ExtractFields {
        /// The document waiting to be read.
        attachment_id: AttachmentId,
    },
    /// One proposed value is waiting for the user to accept or change it.
    ReviewProposedField {
        /// The field that was proposed.
        field: ClaimField,
    },
    /// One required field the document did not yield.
    ProvideField {
        /// The field nobody has a value for.
        field: ClaimField,
    },
}

/// A terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimOutcome {
    /// The reviewed values are on the record.
    Recorded,
    /// The document was thrown away without being recorded.
    Discarded,
}
