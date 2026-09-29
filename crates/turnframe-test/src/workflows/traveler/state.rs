//! Persisted state, phases, obligations and outcomes of the traveler sample
//! (spec §31.4).
//!
//! # Three-valued fields
//!
//! Most fields here are an `Option<String>`, which is the right shape when a
//! field is either filled in or not. [`loyalty_number`](TravelerState::loyalty_number)
//! is deliberately not one of them: see [`FieldState`] and the module
//! documentation of [`traveler`](crate::workflows::traveler) for why a
//! collection workflow usually needs three states rather than two.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why the user did not give a value.
///
/// The distinction is not decoration. "There is no such number", "I would have
/// to look it up" and "I have it and I am not giving it to you" are three
/// different facts about the world, and only the middle one is worth raising
/// again later — which is exactly what [`worth_asking_again`](Self::worth_asking_again)
/// says. Collapsing them into a single "declined" flag throws away the only
/// information that could tell a later prompt whether asking is helpful or
/// merely annoying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeclineReason {
    /// The datum does not exist for this traveler. A traveler who never joined has no
    /// loyalty number until they do.
    NotApplicable,
    /// The user does not know it and would have to look it up.
    Unknown,
    /// The user has it and chooses not to share it.
    Withheld,
}

impl DeclineReason {
    /// Every reason, in declaration order.
    pub const ALL: [Self; 3] = [Self::NotApplicable, Self::Unknown, Self::Withheld];

    /// The stable suffix used in notice codes and event types.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::NotApplicable => "not_applicable",
            Self::Unknown => "unknown",
            Self::Withheld => "withheld",
        }
    }

    /// Returns `true` when raising the question again later could plausibly
    /// succeed.
    ///
    /// Only [`Unknown`](Self::Unknown) qualifies: a number that does not exist
    /// will not start existing, and a user who refused once refused on purpose.
    #[must_use]
    pub fn worth_asking_again(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// A field the user can be asked about: never asked, answered, or declined.
///
/// The two-valued shape — `Option<String>` — cannot tell "we have not asked
/// yet" from "we asked and they said no", and both are empty. That single
/// missing distinction produces two failures, and each of them looks correct
/// one turn at a time:
///
/// * if a declined field keeps its obligation open, the assistant asks for it
///   again on every turn, for ever, and each individual turn is behaving
///   exactly as the projection told it to;
/// * if the projector simply drops the obligation, the view can no longer tell
///   a decline from an answer, so any later question about completeness — may
///   this traveler be activated, may this record be put on a trip — is
///   answered from a state that has forgotten what happened.
///
/// The fix is to make the third state real and let the projection carry the
/// reason, which is what [`Declined`](Self::Declined) does.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FieldState {
    /// Nobody has asked for it yet: the obligation is open.
    #[default]
    Untouched,
    /// The user supplied a value.
    Answered {
        /// The value.
        value: String,
    },
    /// The user was asked and declined, for a reason the domain keeps.
    Declined {
        /// Why.
        reason: DeclineReason,
    },
}

impl FieldState {
    /// An answered field.
    #[must_use]
    pub fn answered(value: impl Into<String>) -> Self {
        Self::Answered {
            value: value.into(),
        }
    }

    /// A declined field.
    #[must_use]
    pub const fn declined(reason: DeclineReason) -> Self {
        Self::Declined { reason }
    }

    /// The value, when there is one.
    #[must_use]
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Answered { value } => Some(value),
            Self::Untouched | Self::Declined { .. } => None,
        }
    }

    /// Returns `true` when the user supplied a value.
    #[must_use]
    pub const fn is_answered(&self) -> bool {
        matches!(self, Self::Answered { .. })
    }

    /// Why the user declined, when they did.
    #[must_use]
    pub const fn decline_reason(&self) -> Option<DeclineReason> {
        match self {
            Self::Declined { reason } => Some(*reason),
            Self::Untouched | Self::Answered { .. } => None,
        }
    }

    /// Returns `true` once the question has an answer of either kind.
    ///
    /// This is the predicate completeness is written against: a declined field
    /// is settled, and a settled field is not an open obligation.
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        !matches!(self, Self::Untouched)
    }
}

/// The lifecycle status stored on the traveler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TravelerStatus {
    /// Being filled in.
    #[default]
    Draft,
    /// Usable by the rest of the application.
    Active,
    /// Kept for the record, no longer usable.
    Archived,
    /// Removed.
    Deleted,
}

impl TravelerStatus {
    /// Returns `true` while fields may still be edited.
    #[must_use]
    pub fn is_editable(self) -> bool {
        matches!(self, Self::Draft | Self::Active)
    }
}

/// The persisted traveler: a flat collection workflow.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TravelerState {
    /// Full name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    /// Contact address. Changing it is a sensitive change (spec §31.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Loyalty number, which the user may decline to give.
    ///
    /// This is the three-valued field of the sample: see [`FieldState`].
    #[serde(default)]
    pub loyalty_number: FieldState,
    /// Lifecycle status.
    pub status: TravelerStatus,
}

impl TravelerState {
    /// The obligations still open, in a stable order.
    #[must_use]
    pub fn open_obligations(&self) -> Vec<TravelerObligation> {
        if self.status != TravelerStatus::Draft {
            return Vec::new();
        }
        let mut obligations = Vec::new();
        if self.full_name.is_none() {
            obligations.push(TravelerObligation::SetName);
        }
        if self.email.is_none() {
            obligations.push(TravelerObligation::SetEmail);
        }
        // A declined field is settled, so its obligation closes. The reason
        // does not vanish with it: the projection carries it as a notice, so
        // the view still knows the difference between answered and declined.
        if !self.loyalty_number.is_settled() {
            obligations.push(TravelerObligation::SetLoyaltyNumber);
        }
        obligations
    }

    /// Returns `true` when every obligation is met and the draft may be
    /// activated.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.full_name.is_some() && self.email.is_some() && self.loyalty_number.is_settled()
    }
}

/// Exactly one lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TravelerPhase {
    /// The case does not exist yet.
    PreDraft,
    /// Fields are missing.
    Collecting,
    /// Everything is in place; the activation card is waiting for a click.
    AwaitingActivation,
    /// Usable by the rest of the application.
    Active,
    /// Kept for the record.
    Archived,
    /// Removed.
    Deleted,
}

/// An open obligation. The traveler workflow is flat: no obligation is
/// parameterized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TravelerObligation {
    /// No full name yet.
    SetName,
    /// No contact address yet.
    SetEmail,
    /// The loyalty number has neither been given nor declined.
    SetLoyaltyNumber,
}

/// A terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TravelerOutcome {
    /// Kept for the record, no longer usable.
    Archived,
    /// Removed.
    Deleted,
}
