//! Identity and version newtypes (spec §7).
//!
//! Every identifier that crosses a boundary is a dedicated type so that a
//! `CaseId` can never be passed where an `InteractionId` is expected and so that
//! public signatures never carry raw strings or integers for identities.
//!
//! Two families exist:
//!
//! * **UUID-backed** identifiers ([`ConversationId`], [`TurnId`], [`InteractionId`],
//!   [`CommandId`], [`EventId`], [`ReceiptId`], [`BatchId`], [`OutboxId`]) are
//!   generated server-side with [`Uuid::now_v7`] so they sort by creation time.
//! * **String-backed** identifiers ([`CaseId`], [`WorkflowKey`], [`WorkflowVersion`],
//!   [`TargetToken`], [`OptionId`], [`OperationKey`], [`ReadToolKey`], [`AccountId`],
//!   [`UserId`], [`SchemaVersion`], [`AttachmentId`], [`OriginToken`], [`QuestionId`],
//!   [`BlockId`], [`ReadRequestId`], [`ProviderKey`], [`ModelKey`], [`AttemptId`])
//!   are opaque labels owned by the application or the server.
//!
//! Neither family implements [`Default`]: an identifier is either derived from
//! its inputs, generated on purpose, or [`nil`](TurnId::nil) in a fixture.
//!
//! All of them serialize transparently (a UUID string or a plain string) and
//! derive [`schemars::JsonSchema`] so they can appear in model-facing schemas.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
        )]
        #[serde(transparent)]
        $(#[$meta])*
        pub struct $name(pub Uuid);

        impl $name {
            /// Generates a new time-ordered (UUID v7) identifier.
            ///
            /// Identifiers that must be reproducible across a replay are
            /// derived instead: see `derive` on the types that have one.
            // Deliberately no `Default`: minting a fresh identity is an act,
            // not a default value, and `#[derive(Default)]` on a struct that
            // contains one would fabricate identities silently.
            #[allow(clippy::new_without_default)]
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// The all-zero identifier, useful as a sentinel in tests and fixtures.
            #[must_use]
            pub const fn nil() -> Self {
                Self(Uuid::nil())
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Returns `true` when this is the nil identifier.
            #[must_use]
            pub fn is_nil(&self) -> bool {
                self.0.is_nil()
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            ::serde::Serialize,
            ::serde::Deserialize,
            ::schemars::JsonSchema,
        )]
        #[serde(transparent)]
        $(#[$meta])*
        pub struct $name(pub String);

        impl $name {
            /// Wraps an existing label.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrows the label as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes the identifier and returns the owned label.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }

            /// Returns `true` when the label is empty.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

pub(crate) use string_id;

uuid_id! {
    /// Identifies one conversation (a chat thread) within an account.
    ConversationId
}

uuid_id! {
    /// Identifies one user turn. Every command, replay record and response block
    /// produced while handling the turn carries it.
    TurnId
}

uuid_id! {
    /// Identifies one persisted interaction (card, confirmation, selection...).
    InteractionId
}

uuid_id! {
    /// Identifies one typed command envelope. Distinct from the idempotency key:
    /// two envelopes may share an idempotency key when a turn is replayed.
    CommandId
}

uuid_id! {
    /// Identifies one committed domain event in the claim ledger.
    EventId
}

uuid_id! {
    /// Identifies one operational receipt derived from committed events.
    ReceiptId
}

uuid_id! {
    /// Identifies one command batch (a group of envelopes committed under one
    /// [`AtomicityScope`](crate::command::AtomicityScope)).
    BatchId
}

uuid_id! {
    /// Identifies one outbox row for an external side effect.
    OutboxId
}

string_id! {
    /// Application-owned identifier of a workflow case (a trip, a
    /// traveler record...). Opaque to the library; never shown to the model.
    CaseId
}

string_id! {
    /// Stable key of a workflow definition, e.g. `"trip"`.
    WorkflowKey
}

string_id! {
    /// Version label of a workflow definition. Must change whenever projection
    /// semantics change (spec §8.4).
    WorkflowVersion
}

string_id! {
    /// Opaque token shown to the model instead of a raw case identifier. The
    /// server owns the token → [`CaseRef`](crate::case::CaseRef) mapping.
    #[schemars(
        description = "Opaque handle for one record, taken from the list of candidates offered with this turn. It is meaningless outside the turn and is never a database identifier."
    )]
    TargetToken
}

string_id! {
    /// Identifier of a stored option on an interaction. The client echoes it
    /// back; the server derives its meaning from the stored option.
    OptionId
}

string_id! {
    /// Key of a semantic operation offered to the interpreter, e.g.
    /// `"trip.set_travel_date"`. Only keys listed in the catalog of this turn
    /// may be used.
    OperationKey
}

string_id! {
    /// Key of a read-only tool offered to the bounded read loop.
    ReadToolKey
}

string_id! {
    /// Tenant identifier. Every lookup in the library is scoped by it.
    AccountId
}

string_id! {
    /// Identifier of the authenticated user acting within an account.
    UserId
}

string_id! {
    /// Version label of a model-facing schema (e.g. the interpreter output).
    SchemaVersion
}

string_id! {
    /// Identifier of an attachment supplied with a turn.
    AttachmentId
}

string_id! {
    /// Server-issued token that a UI surface attaches to a turn to name the
    /// exact record it was opened from (spec §12.4).
    #[schemars(
        description = "Handle for the record the user has open, supplied by the interface with this turn."
    )]
    OriginToken
}

string_id! {
    /// Stable identifier of a question within a turn (spec §19.1).
    QuestionId
}

string_id! {
    /// Stable identifier of a response block within an assistant turn.
    BlockId
}

string_id! {
    /// Identifier the model assigns to a read request so results can be matched
    /// back to it.
    ReadRequestId
}

string_id! {
    /// Stable key of a configured model provider, e.g. `"openai"`. Never a
    /// credential and never a URL.
    ProviderKey
}

string_id! {
    /// Stable key of a configured model, e.g. `"gpt-5.4"`. The configured
    /// identifier, not a marketing name.
    ModelKey
}

string_id! {
    /// Names the authority under which an event payload was erased, e.g. an
    /// erasure ticket, a retention policy key or an operator identifier
    /// ([`EventRedaction`](crate::event::EventRedaction)).
    ///
    /// It is the *only* thing an erasure records beyond its timestamp, so it
    /// must be a stable label the operator can trace back to the request that
    /// justified it, and never the data that was removed, the subject's name or
    /// any other free text: it survives in the ledger precisely because the
    /// payload did not.
    RedactionAuthority
}

string_id! {
    /// Identifier of one attempt at an external effect, scoped to the outbox or
    /// the dispatcher, used to reconcile an unknown outcome (I15).
    ///
    /// # Why this is an identifier and a model call attempt is a number
    ///
    /// [`ProviderAttemptRecord::attempt`](crate::replay::ProviderAttemptRecord::attempt)
    /// counts model calls with a plain `u32`, and that asymmetry is deliberate.
    ///
    /// A model call has no effect outside the process: if it times out, the
    /// only thing anyone ever needs to say about it is *which try it was*,
    /// within a stage whose identity the replay record already fixes (the turn,
    /// the purpose, the provider and the model). Nothing outside the record
    /// refers to it, so it needs a position, not a name.
    ///
    /// An external effect attempt is the opposite. It may have happened even
    /// though the answer never arrived (spec §16.5), which is what
    /// `OutcomeUnknown` means, and settling it later requires naming the exact
    /// attempt to a remote system that has its own idea of what it received.
    /// That name is passed to the dispatcher as an idempotency-scoped token,
    /// stored on the outbox row, quoted in a reconciliation query and compared
    /// against a remote reference. An ordinal would be ambiguous the moment two
    /// workers, two rows or two turns counted separately; an identifier is not.
    AttemptId
}

/// Monotonic revision of a mutable case (spec §7, I13).
///
/// Every command targets an expected revision; a mismatch is a
/// [`RevisionConflict`](crate::error::RevisionConflict).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct CaseRevision(pub u64);

impl CaseRevision {
    /// The revision of a case that does not exist yet.
    pub const ZERO: Self = Self(0);

    /// Returns the revision that follows this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns the raw counter.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl From<u64> for CaseRevision {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for CaseRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_ids_round_trip_and_are_time_ordered() {
        let a = TurnId::new();
        let b = TurnId::new();
        assert!(a <= b, "v7 identifiers sort by creation time");
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.starts_with('"'));
        let back: TurnId = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
        assert_eq!(a.to_string().parse::<TurnId>().unwrap(), a);
        assert!(TurnId::nil().is_nil());
    }

    #[test]
    fn string_ids_are_transparent() {
        let key = WorkflowKey::from("trip");
        assert_eq!(serde_json::to_string(&key).unwrap(), "\"trip\"");
        assert_eq!(key.as_str(), "trip");
        assert_eq!(key.to_string(), "trip");
        let back: WorkflowKey = serde_json::from_str("\"trip\"").unwrap();
        assert_eq!(back, key);
    }

    #[test]
    fn revision_next_and_zero() {
        assert_eq!(CaseRevision::ZERO.next(), CaseRevision(1));
        assert_eq!(CaseRevision(u64::MAX).next(), CaseRevision(u64::MAX));
        assert!(CaseRevision(3) < CaseRevision(4));
    }
}
