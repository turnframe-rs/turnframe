//! Persistent interactions: immutable payloads, the one-blocking-per-case slot
//! and compare-and-swap resolution (spec §15.5, §15.6).
//!
//! # Contract
//!
//! * **Payloads are immutable.** There is no method that changes a persisted
//!   payload; the client only ever echoes identifiers back (I7).
//! * **One blocking interaction per case.** At most one interaction with
//!   `blocking == true` whose status is open (`Active` or `Resolving`) may
//!   exist per `(account, workflow, case_id)`. [`InteractionWriter::insert`]
//!   refuses a second one with `Conflict`;
//!   [`InteractionWriter::insert_replacing_blocking`] invalidates the `Active`
//!   occupant (reason [`InvalidationReason::Superseded`]) and inserts the new
//!   card under a new identifier. A `Resolving` occupant is never replaced: its
//!   commands are executing, so the call fails with `Conflict`.
//! * **Tenant isolation.** Every lookup is scoped by account; an identifier of
//!   another tenant is `NotFound`, indistinguishable from an unknown one.
//! * **Compare-and-swap resolution.** [`InteractionWriter::begin_resolution`]
//!   moves `Active → Resolving` only when the current status equals the
//!   expected one; [`InteractionWriter::finish_resolution`] settles a `Resolving`
//!   interaction to `Resolved`, `Failed` or back to `Active`. Repeating a finish
//!   with the same outcome is accepted (idempotent); a different outcome after a
//!   terminal one is `Conflict`.
//! * **Revision invalidation.** A case revision change invalidates `Active`
//!   interactions bound to another revision unless they declare revision
//!   independence (`revision_independent == true`). `Resolving` interactions
//!   are left alone: their own commit is usually what moved the revision, and
//!   `finish_resolution` settles them.
//! * **Expiry** moves `Active` interactions whose `expires_at` has passed to
//!   `Expired`; it is a system sweep across tenants.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use turnframe_core::case::CaseKey;
use turnframe_core::ids::{
    AccountId, CaseRevision, ConversationId, EventId, InteractionId, OptionId, TurnId,
};
use turnframe_core::interaction::{Interaction, InteractionStatus};

use crate::error::StoreError;

/// How a `Resolving` interaction is settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolutionOutcome {
    /// The associated commands committed; the events authorize the receipt.
    Resolved {
        /// Committed events backing the resolution.
        event_ids: Vec<EventId>,
    },
    /// The associated commands failed and policy keeps the card closed.
    Failed {
        /// Stable failure code (never free text).
        code: String,
    },
    /// The associated commands failed and policy restores the card so the user
    /// may answer again.
    RestoreActive,
}

impl ResolutionOutcome {
    /// The status the outcome leads to.
    #[must_use]
    pub fn target_status(&self) -> InteractionStatus {
        match self {
            Self::Resolved { .. } => InteractionStatus::Resolved,
            Self::Failed { .. } => InteractionStatus::Failed,
            Self::RestoreActive => InteractionStatus::Active,
        }
    }
}

/// Why an interaction was invalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InvalidationReason {
    /// The case moved to another revision.
    RevisionChanged,
    /// A new blocking interaction replaced it.
    Superseded {
        /// The replacement.
        by: InteractionId,
    },
    /// The case reached a terminal phase.
    CaseClosed,
    /// An operator or policy decided, identified by a stable code.
    Administrative {
        /// Stable code.
        code: String,
    },
}

/// What is recorded when an interaction is invalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidationRecord {
    /// The reason.
    pub reason: InvalidationReason,
    /// The revision the case moved to, when the invalidation came from a
    /// revision sweep ([`InteractionWriter::invalidate_for_case`]).
    ///
    /// `None` when a replacement card or a policy closed this one, where no
    /// revision is involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_revision: Option<CaseRevision>,
    /// When it happened, stamped by the store's clock.
    pub at: DateTime<Utc>,
}

/// A persisted interaction together with the store-owned resolution metadata
/// that the core record does not carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionRecord {
    /// The interaction as the core sees it (payload, status, resolved option).
    pub interaction: Interaction,
    /// Turn whose response began the resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by_turn: Option<TurnId>,
    /// Events that backed a `Resolved` outcome. A second click on a resolved
    /// interaction is answered from these (spec §15.5).
    #[serde(default)]
    pub resolution_event_ids: Vec<EventId>,
    /// Stable code of a `Failed` outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    /// Why and when it was invalidated, when status is `Invalidated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation: Option<InvalidationRecord>,
}

impl InteractionRecord {
    /// Wraps a freshly inserted interaction with empty metadata.
    #[must_use]
    pub fn new(interaction: Interaction) -> Self {
        Self {
            interaction,
            resolved_by_turn: None,
            resolution_event_ids: Vec::new(),
            failure_code: None,
            invalidation: None,
        }
    }

    /// The interaction identifier.
    #[must_use]
    pub fn id(&self) -> InteractionId {
        self.interaction.id
    }

    /// The current status.
    #[must_use]
    pub fn status(&self) -> InteractionStatus {
        self.interaction.status
    }
}

/// The read half of the interaction contract.
///
/// It answers what cards exist and what they say, and can create, resolve,
/// invalidate or expire none of them. This is the half the plan-only path of
/// [`turnframe-runtime`](https://docs.rs/turnframe-runtime) holds, which is why
/// a shadow turn cannot leave a card a real user could click.
#[async_trait]
pub trait InteractionReader: Send + Sync {
    /// Loads an interaction of `account`.
    ///
    /// # Errors
    /// * `NotFound` when it does not exist for this account.
    async fn get(
        &self,
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, StoreError>;

    /// Lists the open (`Active` or `Resolving`) interactions of a conversation,
    /// ordered by `created_at` then id.
    ///
    /// # Errors
    /// * [`StoreError`] when the listing could not be read.
    async fn list_open_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Vec<Interaction>, StoreError>;

    /// Lists the open (`Active` or `Resolving`) interactions of a case, ordered
    /// by `created_at` then id.
    ///
    /// # Errors
    /// * [`StoreError`] when the listing could not be read.
    async fn list_open_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<Interaction>, StoreError>;

    /// Whether a blocking card of `case_key` bound to `revision` has already
    /// been answered by the user.
    ///
    /// # The loop this ends
    ///
    /// A `ConfirmCommand` payload must offer a way to decline, and declining
    /// ends the card **without effect**. Nothing is written, the case does not
    /// move, and on the next turn the projection declares the same requirement
    /// and the same card goes back up. The user presses "not now" and is asked
    /// again, for ever.
    ///
    /// The domain cannot see this: a projector reads state, and a decline
    /// leaves none. The runtime can, because a requirement belongs to a case at
    /// a revision, and "not now" means "ask me when the document changes". A
    /// card answered at the revision the case is still on has been answered.
    ///
    /// Answered is `Resolved`, `Declined` or `Dismissed` — the three the user
    /// causes. A card the case outgrew (`Invalidated`) or that timed out
    /// (`Expired`) was not answered by anybody, and a requirement still standing
    /// after one is raised again as it always was.
    ///
    /// # Errors
    /// * [`StoreError`] when the lookup could not be read.
    async fn blocking_answered_at(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        revision: CaseRevision,
    ) -> Result<bool, StoreError>;
}

/// The write half of the interaction contract.
#[async_trait]
pub trait InteractionWriter: Send + Sync {
    /// Inserts a new `Active` interaction.
    ///
    /// # Errors
    /// * `Other(INVALID_RECORD)` when `interaction.status` is not `Active`.
    /// * `Conflict` when the identifier exists for the account, or when the
    ///   interaction is blocking and its case already has an open blocking
    ///   interaction (I5).
    async fn insert(&self, interaction: Interaction) -> Result<(), StoreError>;

    /// Inserts a new `Active` interaction, invalidating the `Active` blocking
    /// occupant of the same case when there is one. Returns the identifiers it
    /// invalidated (empty when the slot was free or the new card is not
    /// blocking).
    ///
    /// # Errors
    /// * `Other(INVALID_RECORD)` when `interaction.status` is not `Active`.
    /// * `Conflict` when the identifier exists, or when the occupant is
    ///   `Resolving` (its commands are executing; it cannot be replaced).
    async fn insert_replacing_blocking(
        &self,
        interaction: Interaction,
    ) -> Result<Vec<InteractionId>, StoreError>;

    /// Compare-and-swap `expected_status → Resolving`, recording the chosen
    /// option and the resolving turn. The only legal source status is
    /// `Active`; the explicit parameter makes a lost race visible to the
    /// caller instead of hiding it behind a re-read.
    ///
    /// # Errors
    /// * `NotFound` when the interaction does not exist for `account`.
    /// * `Conflict` when the current status differs from `expected_status`, or
    ///   when `expected_status → Resolving` is not a legal transition.
    async fn begin_resolution(
        &self,
        account: &AccountId,
        id: &InteractionId,
        expected_status: InteractionStatus,
        option_id: OptionId,
        resolved_by: TurnId,
    ) -> Result<InteractionRecord, StoreError>;

    /// Settles a `Resolving` interaction.
    ///
    /// Repeating the call with an outcome whose target status is already the
    /// current status and whose data (event ids, failure code) matches is
    /// accepted without change. `RestoreActive` clears the resolving turn, the
    /// chosen option and `resolved_at` so the card is answerable again.
    ///
    /// `Resolved` and `Failed` leave `resolved_at` as
    /// [`InteractionWriter::begin_resolution`] stamped it: it means "when the
    /// answer was accepted". When the commands actually committed is recorded
    /// by the command journal and by the events, and is not duplicated here.
    ///
    /// # Errors
    /// * `NotFound` when the interaction does not exist for `account`.
    /// * `Conflict` when the status is neither `Resolving` nor the outcome's
    ///   target, or when the target status matches but the data differs.
    async fn finish_resolution(
        &self,
        account: &AccountId,
        id: &InteractionId,
        outcome: ResolutionOutcome,
    ) -> Result<InteractionRecord, StoreError>;

    /// Invalidates every `Active` interaction of the case that is bound to a
    /// revision other than `new_revision` and does not declare revision
    /// independence. Returns the invalidated identifiers in list order.
    ///
    /// # Errors
    /// * [`StoreError`] when the sweep could not be written.
    async fn invalidate_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        new_revision: CaseRevision,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError>;

    /// Invalidates every `Active` interaction of the case, whatever revision it
    /// is bound to, and whether or not it declares revision independence.
    /// Returns the invalidated identifiers in list order.
    ///
    /// This is the operator's path, not the runtime's, and it exists because
    /// [`invalidate_for_case`](Self::invalidate_for_case) cannot express it: a
    /// card is invalidated there for having been bound to a revision the case
    /// has left, so a decision that has nothing to do with the revision — a
    /// workflow rolled back to a version that cannot compile the option the
    /// card offers, an account suspended, a card withdrawn — has no way to run.
    /// Without it the honest rollback step is to leave the user holding an
    /// option nothing will honour.
    ///
    /// A `Resolving` interaction is deliberately left alone: a command it
    /// authorized is in flight, and taking the card away underneath it would
    /// settle nothing while making the outcome unattributable.
    ///
    /// # Errors
    /// * [`StoreError`] when the sweep could not be written.
    async fn invalidate_case_cards(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        reason: InvalidationReason,
    ) -> Result<Vec<InteractionId>, StoreError>;

    /// Moves every `Active` interaction whose `expires_at <= now` to `Expired`,
    /// across all accounts. Returns the expired identifiers in list order.
    ///
    /// # Errors
    /// * [`StoreError`] when the sweep could not be written.
    async fn expire_due(&self, now: DateTime<Utc>) -> Result<Vec<InteractionId>, StoreError>;
}

/// Persistence of interactions with the rules of spec §15.5 and §15.6: both
/// halves.
///
/// See the module documentation for the contract; the conformance suite proves
/// it (`check_blocking_interaction_*`, `check_cross_tenant_*`,
/// `check_begin_resolution_cas`, `check_finish_resolution_idempotent`,
/// `check_revision_invalidation_respects_independence`,
/// `check_interaction_expiry` in [`crate::conformance`]).
///
/// There is nothing to implement here: write [`InteractionReader`] and
/// [`InteractionWriter`] and the blanket implementation below supplies this
/// trait.
pub trait InteractionStore: InteractionReader + InteractionWriter {}

impl<T: InteractionReader + InteractionWriter + ?Sized> InteractionStore for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_targets() {
        assert_eq!(
            ResolutionOutcome::Resolved { event_ids: vec![] }.target_status(),
            InteractionStatus::Resolved
        );
        assert_eq!(
            ResolutionOutcome::Failed { code: "x".into() }.target_status(),
            InteractionStatus::Failed
        );
        assert_eq!(
            ResolutionOutcome::RestoreActive.target_status(),
            InteractionStatus::Active
        );
    }

    #[test]
    fn reason_serializes_tagged() {
        let json = serde_json::to_value(InvalidationReason::Superseded {
            by: InteractionId::nil(),
        })
        .unwrap();
        assert_eq!(json["kind"], "superseded");
    }
}
