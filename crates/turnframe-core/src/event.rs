//! Commits, committed events, receipts and external outcome states (spec §16-17).
//!
//! Events are the claim ledger: an operational receipt may only be emitted
//! from committed events or an authoritative external receipt (I16).
//!
//! # Personal data in a payload, and how it leaves
//!
//! The ledger is append-only and a case's identity outlives its content, so
//! nothing in this module removes an event or a case. That would leave personal
//! data in a payload with no exit, which is not a position a library may hand
//! an adopter who is legally obliged to erase it on request, so there is a
//! third way out: the store redacts a payload **in place** and the event keeps
//! its identity, its type, its position and its timestamps. Nothing appears,
//! disappears or moves.
//!
//! What a receipt renderer is then handed is a [`ReceiptEvent`], which is a
//! committed event *or* a [`RedactedEvent`], so a domain is told explicitly
//! that a payload is gone and writes the copy that says so. The write half of
//! the operation lives in the store contract
//! (`turnframe_store::events::EventJournalWriter::redact_payload`).
//!
//! An event payload that carries a **reference** — a traveler id rather than a
//! name, an attachment id rather than the text read out of it — is easier to
//! erase, because erasing the record the reference points at empties the
//! payload without touching the ledger at all. Design payloads that way where
//! you can. The honest caveat is that it does not generalise: a receipt says
//! *what changed*, and "the traveler's name is now Marta Bianchi" cannot be rendered
//! from an id. Wherever the receipt needs the value, the value is in the
//! payload and this path is the exit for it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::command::IdempotencyKey;
use crate::hash::derive_uuid;
use crate::ids::{
    AttemptId, CaseRevision, CommandId, EventId, OutboxId, ReceiptId, RedactionAuthority,
};
use crate::locale::LocalizedText;

/// Result of executing a command batch (spec §17.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit<S, E> {
    /// State after the commit, or `None` when the case no longer exists.
    ///
    /// `None` is written as a missing key, so a `State` type that itself
    /// serializes to JSON `null` (`()`, an empty newtype, an `Option` field at
    /// the root) reads back as "the case is gone". A workflow state must
    /// therefore serialize to an object; the same rule holds for
    /// [`WorkflowDefinition::Outcome`](crate::flow::WorkflowDefinition::Outcome),
    /// whose erased form drives
    /// [`ErasedWorkflowView::is_complete`](crate::flow::ErasedWorkflowView::is_complete).
    /// Concrete `serde_json::Value` fields that must keep an explicit `null`
    /// use [`FieldValue`](crate::interaction::FieldValue) instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<S>,
    /// Revision after the commit.
    pub new_revision: CaseRevision,
    /// Events committed by this batch.
    pub events: Vec<CommittedEvent<E>>,
    /// `true` when the executor recognised the idempotency key and returned the
    /// original outcome without repeating the effect (I14).
    pub idempotency_replay: bool,
}

impl<S, E> Commit<S, E> {
    /// Identifiers of all committed events.
    #[must_use]
    pub fn event_ids(&self) -> Vec<EventId> {
        self.events.iter().map(|e| e.event_id).collect()
    }

    /// Lightweight references to all committed events.
    #[must_use]
    pub fn event_refs(&self) -> Vec<EventRef> {
        self.events.iter().map(CommittedEvent::event_ref).collect()
    }
}

impl<S, E: Clone> Commit<S, E> {
    /// The commit's events in the form
    /// [`WorkflowDefinition::receipts`](crate::flow::WorkflowDefinition::receipts)
    /// takes.
    ///
    /// Every event of a fresh commit still has its payload, so this is the
    /// whole conversion: a [`ReceiptEvent::Redacted`] only ever comes back out
    /// of the store, never out of an execution.
    #[must_use]
    pub fn receipt_events(&self) -> Vec<ReceiptEvent<E>> {
        self.events
            .iter()
            .cloned()
            .map(ReceiptEvent::Committed)
            .collect()
    }
}

/// One committed domain event (spec §17.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedEvent<E> {
    /// Identifier in the ledger.
    pub event_id: EventId,
    /// Stable event type label (e.g. `"trip.extra_added"`).
    pub event_type: String,
    /// When it was committed.
    pub occurred_at: DateTime<Utc>,
    /// Domain payload.
    pub payload: E,
}

impl<E> CommittedEvent<E> {
    /// Reference without payload.
    #[must_use]
    pub fn event_ref(&self) -> EventRef {
        EventRef {
            event_id: self.event_id,
            event_type: self.event_type.clone(),
        }
    }

    /// Transforms the payload.
    pub fn try_map_payload<F, Err>(
        self,
        f: impl FnOnce(E) -> Result<F, Err>,
    ) -> Result<CommittedEvent<F>, Err> {
        Ok(CommittedEvent {
            event_id: self.event_id,
            event_type: self.event_type,
            occurred_at: self.occurred_at,
            payload: f(self.payload)?,
        })
    }

    /// Transforms the payload by reference, keeping identity and timestamp.
    ///
    /// Used at the erasure boundary, where the payload is deserialized *from*
    /// the borrowed JSON instead of being cloned into the typed side.
    pub fn try_map_payload_ref<F, Err>(
        &self,
        f: impl FnOnce(&E) -> Result<F, Err>,
    ) -> Result<CommittedEvent<F>, Err> {
        Ok(CommittedEvent {
            event_id: self.event_id,
            event_type: self.event_type.clone(),
            occurred_at: self.occurred_at,
            payload: f(&self.payload)?,
        })
    }
}

/// Reference to a committed event without its payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventRef {
    /// Identifier in the ledger.
    pub event_id: EventId,
    /// Stable event type label.
    pub event_type: String,
}

/// The record of an erasure: that a payload was removed, when, and on whose
/// authority.
///
/// It deliberately does not say *what* was removed. An erasure that recorded
/// the value it erased would erase nothing, and one that recorded nothing at
/// all would leave an operator unable to answer why an event reads empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRedaction {
    /// When the payload was removed.
    pub redacted_at: DateTime<Utc>,
    /// Under whose authority — an erasure ticket, a retention policy key or an
    /// operator identifier, never the data that was removed.
    pub authority: RedactionAuthority,
}

/// A committed event whose payload was erased (see the module documentation).
///
/// Everything the claim guard needs survives: the identity a receipt cites, the
/// type label, and the instant it was committed. Its position in the journal
/// survives too, in the store; it is not part of this value because a receipt
/// never cites a position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedEvent {
    /// Identifier in the ledger, unchanged by the erasure.
    pub event_id: EventId,
    /// Stable event type label, unchanged by the erasure.
    pub event_type: String,
    /// When the event was committed, unchanged by the erasure.
    pub occurred_at: DateTime<Utc>,
    /// When the payload was erased, and on whose authority.
    pub redaction: EventRedaction,
}

impl RedactedEvent {
    /// Reference to the event, which is what a receipt cites.
    #[must_use]
    pub fn event_ref(&self) -> EventRef {
        EventRef {
            event_id: self.event_id,
            event_type: self.event_type.clone(),
        }
    }
}

/// One event as offered to
/// [`WorkflowDefinition::receipts`](crate::flow::WorkflowDefinition::receipts):
/// its payload is either still in the ledger or has been erased.
///
/// A domain matches on this rather than on a payload that failed to
/// deserialize, which is the difference between a receipt that says something
/// honest about an erased event and a turn that renders as though the data were
/// still there.
///
/// # It is deliberately not `#[non_exhaustive]`
///
/// A payload is present or it is not; there is no third case, and the
/// exhaustive match is the mechanism. A wildcard arm here is exactly the arm
/// that would render an erased event as though nothing had happened, so a
/// future variant — if one is ever justified — must break the domains that
/// render receipts rather than fall silently into their catch-all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptEvent<E> {
    /// The payload is in the ledger; the receipt may say what changed.
    Committed(CommittedEvent<E>),
    /// The payload was erased; the receipt may say that it happened, and that
    /// the detail is gone.
    Redacted(RedactedEvent),
}

impl<E> ReceiptEvent<E> {
    /// Identifier in the ledger, whether or not the payload survived.
    #[must_use]
    pub fn event_id(&self) -> EventId {
        match self {
            Self::Committed(event) => event.event_id,
            Self::Redacted(event) => event.event_id,
        }
    }

    /// Stable event type label, whether or not the payload survived.
    #[must_use]
    pub fn event_type(&self) -> &str {
        match self {
            Self::Committed(event) => &event.event_type,
            Self::Redacted(event) => &event.event_type,
        }
    }

    /// When the event was committed, whether or not the payload survived.
    #[must_use]
    pub fn occurred_at(&self) -> DateTime<Utc> {
        match self {
            Self::Committed(event) => event.occurred_at,
            Self::Redacted(event) => event.occurred_at,
        }
    }

    /// Reference without payload, which is all a receipt cites.
    #[must_use]
    pub fn event_ref(&self) -> EventRef {
        match self {
            Self::Committed(event) => event.event_ref(),
            Self::Redacted(event) => event.event_ref(),
        }
    }

    /// The payload, or `None` when it was erased.
    #[must_use]
    pub fn payload(&self) -> Option<&E> {
        match self {
            Self::Committed(event) => Some(&event.payload),
            Self::Redacted(_) => None,
        }
    }

    /// The erasure record, or `None` while the payload is still there.
    #[must_use]
    pub fn redaction(&self) -> Option<&EventRedaction> {
        match self {
            Self::Committed(_) => None,
            Self::Redacted(event) => Some(&event.redaction),
        }
    }

    /// Returns `true` when the payload was erased.
    #[must_use]
    pub fn is_redacted(&self) -> bool {
        matches!(self, Self::Redacted(_))
    }

    /// Transforms the payload by reference, keeping identity and timestamp.
    ///
    /// A redacted event passes through untouched: there is nothing to convert,
    /// which is why the type-erasure boundary can hand a domain an erased event
    /// without ever trying — and failing — to deserialize an empty payload.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns for a payload that is still present.
    pub fn try_map_payload_ref<F, Err>(
        &self,
        f: impl FnOnce(&E) -> Result<F, Err>,
    ) -> Result<ReceiptEvent<F>, Err> {
        match self {
            Self::Committed(event) => event.try_map_payload_ref(f).map(ReceiptEvent::Committed),
            Self::Redacted(event) => Ok(ReceiptEvent::Redacted(event.clone())),
        }
    }
}

impl<E> From<CommittedEvent<E>> for ReceiptEvent<E> {
    fn from(event: CommittedEvent<E>) -> Self {
        Self::Committed(event)
    }
}

impl<E> From<RedactedEvent> for ReceiptEvent<E> {
    fn from(event: RedactedEvent) -> Self {
        Self::Redacted(event)
    }
}

/// Severity of a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptSeverity {
    /// Neutral information.
    Info,
    /// The operation succeeded.
    Success,
    /// Something needs attention.
    Warning,
    /// The operation failed.
    Error,
}

/// A reference to an artifact produced by a command (PDF, XML, protocol id...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Application-defined artifact identifier.
    pub artifact_id: String,
    /// Kind label (e.g. `"itinerary_pdf"`, `"ticket_number"`).
    pub kind: String,
    /// Human label.
    pub label: LocalizedText,
    /// Where to fetch it, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Media type, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// Domain-separation prefix of [`ReceiptId::derive`].
const RECEIPT_ID_DOMAIN: &str = "turnframe.receipt_id.v1";

impl ReceiptId {
    /// Derives the identifier of a receipt from the events it cites and its
    /// status code.
    ///
    /// Receipts are deterministic (spec §17.3): the same committed events
    /// rendered again are the same receipt, so a replayed turn produces the
    /// same identifiers and
    /// [`claim_guard::verify`](crate::response::claim_guard::verify) can refuse
    /// two different receipts that claim to be one.
    ///
    /// The event order does not change the result; the set does.
    #[must_use]
    pub fn derive(event_ids: &[EventId], status_code: &str) -> Self {
        let mut ids: Vec<String> = event_ids.iter().map(EventId::to_string).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut parts: Vec<&str> = vec![status_code];
        parts.extend(ids.iter().map(String::as_str));
        Self(derive_uuid(RECEIPT_ID_DOMAIN, &parts))
    }
}

/// A deterministic, server-rendered statement of what happened (spec §17.3).
///
/// Receipts are the only place where operational outcomes are stated. A
/// receipt claiming success must reference at least one committed event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationalReceipt {
    /// Identifier of the receipt.
    pub receipt_id: ReceiptId,
    /// Events that authorize the claim (I16).
    pub event_ids: Vec<EventId>,
    /// Severity.
    pub severity: ReceiptSeverity,
    /// Title copy.
    pub title: LocalizedText,
    /// Body copy.
    pub body: LocalizedText,
    /// Stable status code (e.g. `"trip.extra_added"`, `"airline.accepted"`).
    pub status_code: String,
    /// Artifacts produced.
    #[serde(default)]
    pub artifact_refs: Vec<ArtifactRef>,
}

impl OperationalReceipt {
    /// Returns `true` when the receipt is backed by at least one event.
    #[must_use]
    pub fn is_event_backed(&self) -> bool {
        !self.event_ids.is_empty()
    }
}

/// Fine-grained state of a request another system decides (spec §17.4).
///
/// Never collapse these into a generic "done". New states appear whenever that
/// system adds a step, so downstream matches need a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExternalStatus {
    /// Payload prepared locally.
    Prepared,
    /// Payload validated locally.
    Validated,
    /// Waiting for the user's confirmation.
    AwaitingConfirmation,
    /// Transmitted to the first hop.
    Submitted,
    /// An intermediary acknowledged receipt.
    ReceivedByIntermediary,
    /// The deciding system acknowledged receipt.
    ReceivedByAuthority,
    /// Accepted by the deciding system.
    Accepted,
    /// Rejected by the deciding system or an intermediary.
    Rejected,
    /// Issued by the deciding system: a ticket, a permit, a booking.
    Issued,
    /// Could not be delivered to the recipient.
    NotDelivered,
    /// Delivered to the recipient.
    Delivered,
    /// The whole flow is complete.
    Completed,
}

impl ExternalStatus {
    /// Returns `true` for states after which no further external transition is
    /// expected: [`Self::Rejected`], [`Self::NotDelivered`], [`Self::Completed`].
    #[must_use]
    pub fn is_final(self) -> bool {
        matches!(self, Self::Rejected | Self::NotDelivered | Self::Completed)
    }

    /// Returns `true` once the payload has left the system, i.e. an external
    /// effect may exist.
    #[must_use]
    pub fn is_transmitted(self) -> bool {
        self >= Self::Submitted
    }
}

/// An external effect was attempted and its result is unknown (I15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("external outcome unknown for attempt {attempt_id} ({reason})")]
pub struct UnknownOutcome {
    /// Identifier of the attempt (outbox or dispatcher scoped).
    pub attempt_id: AttemptId,
    /// Remote identifier to reconcile with, if the remote returned one.
    pub remote_ref: Option<String>,
    /// Stable reason code (e.g. `"timeout_after_send"`), never free text.
    pub reason: String,
}

/// Status of an outbox row (spec §16.4).
///
/// Dispatch strategies grow, so downstream matches need a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutboxStatus {
    /// Waiting for dispatch.
    Pending,
    /// A dispatcher is calling the external system.
    Dispatching,
    /// The call was made and the outcome is unknown; reconcile, do not retry blindly.
    OutcomeUnknown,
    /// The external system confirmed.
    Completed,
    /// The external system definitively refused.
    Failed,
}

impl OutboxStatus {
    /// Returns `true` when no further dispatch will happen.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }

    /// Legal transitions of the outbox state machine.
    #[must_use]
    pub fn can_transition(from: Self, to: Self) -> bool {
        matches!(
            (from, to),
            (Self::Pending, Self::Dispatching)
                | (Self::Dispatching, Self::Pending)
                | (Self::Dispatching, Self::OutcomeUnknown)
                | (Self::Dispatching, Self::Completed)
                | (Self::Dispatching, Self::Failed)
                | (Self::OutcomeUnknown, Self::Completed)
                | (Self::OutcomeUnknown, Self::Failed)
                | (Self::OutcomeUnknown, Self::Pending)
        )
    }
}

/// One external side effect awaiting dispatch (spec §16.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Identifier of the row.
    pub outbox_id: OutboxId,
    /// Command that produced it.
    pub command_id: CommandId,
    /// Destination label (e.g. `"airline"`).
    pub destination: String,
    /// Payload for the dispatcher.
    pub payload: serde_json::Value,
    /// Idempotency key forwarded to the external system.
    pub idempotency_key: IdempotencyKey,
    /// Current status.
    pub status: OutboxStatus,
    /// Attempts made so far.
    pub attempt_count: u32,
    /// Earliest next attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Completion time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_status_finality() {
        assert!(ExternalStatus::Completed.is_final());
        assert!(ExternalStatus::Rejected.is_final());
        assert!(ExternalStatus::NotDelivered.is_final());
        assert!(!ExternalStatus::Accepted.is_final());
        assert!(ExternalStatus::Submitted.is_transmitted());
        assert!(!ExternalStatus::Validated.is_transmitted());
    }

    #[test]
    fn receipt_ids_are_derived_from_events_and_status() {
        let a = EventId::nil();
        let b = EventId::from(uuid::Uuid::from_u128(7));
        let id = ReceiptId::derive(&[a, b], "trip.rebooking_sent");
        assert_eq!(id, ReceiptId::derive(&[a, b], "trip.rebooking_sent"));
        assert_eq!(
            id,
            ReceiptId::derive(&[b, a], "trip.rebooking_sent"),
            "order free"
        );
        assert_ne!(id, ReceiptId::derive(&[a], "trip.rebooking_sent"));
        assert_ne!(id, ReceiptId::derive(&[a, b], "trip.refused"));
    }

    #[test]
    fn a_redacted_event_keeps_everything_a_claim_rests_on() {
        let committed = CommittedEvent {
            event_id: EventId::from(uuid::Uuid::from_u128(11)),
            event_type: "trip.extra_added".to_owned(),
            occurred_at: DateTime::<Utc>::UNIX_EPOCH,
            payload: serde_json::json!({ "description": "a name that must go" }),
        };
        let redacted = RedactedEvent {
            event_id: committed.event_id,
            event_type: committed.event_type.clone(),
            occurred_at: committed.occurred_at,
            redaction: EventRedaction {
                redacted_at: DateTime::<Utc>::UNIX_EPOCH,
                authority: RedactionAuthority::from("erasure-request-8842"),
            },
        };

        let present = ReceiptEvent::from(committed.clone());
        let gone = ReceiptEvent::<serde_json::Value>::from(redacted);

        // Identity, type and instant are what the claim guard and a receipt
        // need, and they are the same on both sides.
        assert_eq!(present.event_id(), gone.event_id());
        assert_eq!(present.event_type(), gone.event_type());
        assert_eq!(present.occurred_at(), gone.occurred_at());
        assert_eq!(present.event_ref(), gone.event_ref());

        // The payload is the only thing that differs, and it differs visibly.
        assert!(!present.is_redacted());
        assert!(gone.is_redacted());
        assert!(present.payload().is_some());
        assert!(gone.payload().is_none());
        assert!(present.redaction().is_none());
        assert_eq!(
            gone.redaction().map(|r| r.authority.as_str()),
            Some("erasure-request-8842")
        );

        // An erased payload has nothing to convert, so the type-erasure
        // boundary cannot turn an erasure into a deserialization failure.
        let mapped = gone.try_map_payload_ref(|_: &serde_json::Value| Err::<(), &str>("never run"));
        assert!(mapped.is_ok(), "a redacted event maps without calling f");
        assert!(mapped.unwrap_or_else(|_| unreachable!()).is_redacted());
        assert!(
            present
                .try_map_payload_ref(|_: &serde_json::Value| Err::<(), &str>("boom"))
                .is_err(),
            "a present payload still goes through f"
        );
    }

    #[test]
    fn a_commit_offers_its_events_for_receipt_rendering() {
        let commit: Commit<(), u32> = Commit {
            state: None,
            new_revision: CaseRevision(1),
            events: vec![CommittedEvent {
                event_id: EventId::nil(),
                event_type: "t".to_owned(),
                occurred_at: DateTime::<Utc>::UNIX_EPOCH,
                payload: 7,
            }],
            idempotency_replay: false,
        };
        let events = commit.receipt_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload(), Some(&7));
        assert!(
            !events[0].is_redacted(),
            "a fresh commit never carries an erased payload"
        );
    }

    #[test]
    fn outbox_transitions() {
        assert!(OutboxStatus::can_transition(
            OutboxStatus::Pending,
            OutboxStatus::Dispatching
        ));
        assert!(OutboxStatus::can_transition(
            OutboxStatus::Dispatching,
            OutboxStatus::OutcomeUnknown
        ));
        assert!(!OutboxStatus::can_transition(
            OutboxStatus::Completed,
            OutboxStatus::Pending
        ));
        assert!(!OutboxStatus::can_transition(
            OutboxStatus::Pending,
            OutboxStatus::Completed
        ));
    }
}
