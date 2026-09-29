//! Typed traveler commands, events and model-facing argument shapes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::workflows::traveler::state::DeclineReason;

/// Keys of the operations the interpreter may propose.
pub mod operations {
    /// Create the draft.
    pub const CREATE_DRAFT: &str = "traveler.create_draft";
    /// Set the full name.
    pub const SET_NAME: &str = "traveler.set_full_name";
    /// Change the contact address.
    pub const CHANGE_EMAIL: &str = "traveler.change_email";
    /// Set the loyalty number.
    pub const SET_LOYALTY_NUMBER: &str = "traveler.set_loyalty_number";
    /// Record that the user will not give the loyalty number.
    pub const DECLINE_LOYALTY_NUMBER: &str = "traveler.decline_loyalty_number";
    /// Make the traveler usable.
    pub const ACTIVATE: &str = "traveler.activate";
    /// Keep the traveler for the record only.
    pub const ARCHIVE: &str = "traveler.archive";
    /// Remove the traveler.
    pub const DELETE: &str = "traveler.delete";
}

/// One typed traveler command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TravelerCommand {
    /// Bring the case into existence.
    CreateDraft,
    /// Bring the case into existence with its full name.
    CreateNamedDraft {
        /// The name.
        full_name: String,
    },
    /// Set the full name.
    SetName {
        /// The name.
        value: String,
    },
    /// Change the contact address. Sensitive: it is where notifications go.
    ChangeEmail {
        /// The new address.
        value: String,
    },
    /// Set the loyalty number.
    SetLoyaltyNumber {
        /// The number.
        value: String,
    },
    /// Record that the user will not give the loyalty number, and
    /// why. Settles the field without a value, closing its obligation.
    DeclineLoyaltyNumber {
        /// Why the user is not giving it.
        reason: DeclineReason,
    },
    /// Make the traveler usable.
    Activate,
    /// Keep the traveler for the record only.
    Archive,
    /// Remove the traveler.
    Delete,
}

/// One committed traveler event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TravelerEvent {
    /// The draft came into existence.
    DraftCreated,
    /// The full name changed.
    NameSet {
        /// The new name.
        value: String,
    },
    /// The contact address changed.
    EmailChanged {
        /// The address it replaced, when there was one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous: Option<String>,
        /// The new address.
        value: String,
    },
    /// The loyalty number changed.
    LoyaltyNumberSet {
        /// The new number.
        value: String,
    },
    /// The user declined to give the loyalty number.
    LoyaltyNumberDeclined {
        /// Why.
        reason: DeclineReason,
    },
    /// The traveler became usable.
    Activated,
    /// The traveler was archived.
    Archived,
    /// The traveler was removed.
    Deleted,
}

impl TravelerEvent {
    /// The stable event type label stored on the committed event.
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::DraftCreated => "traveler.draft_created",
            Self::NameSet { .. } => "traveler.full_name_set",
            Self::EmailChanged { .. } => "traveler.email_changed",
            Self::LoyaltyNumberSet { .. } => "traveler.loyalty_number_set",
            Self::LoyaltyNumberDeclined { .. } => "traveler.loyalty_number_declined",
            Self::Activated => "traveler.activated",
            Self::Archived => "traveler.archived",
            Self::Deleted => "traveler.deleted",
        }
    }
}

/// Arguments of [`operations::CREATE_DRAFT`]: the full name, when the user gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateDraftArgs {
    /// The full name.
    #[serde(default)]
    pub full_name: Option<String>,
}

/// Arguments of the operations that carry a single string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValueArgs {
    /// The value to store.
    pub value: String,
}

/// Arguments of [`operations::DECLINE_LOYALTY_NUMBER`].
///
/// The reason is a closed set rather than free text: the model picks which of
/// three human meanings the user expressed, and the domain — not the prose —
/// decides what each one implies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeclineArgs {
    /// Why the user is not giving the value.
    pub reason: DeclineReason,
}
