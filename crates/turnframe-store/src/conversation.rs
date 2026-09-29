//! Conversations, persisted turns and the crash-recovery phase marker
//! (spec §22.3, §23.1).
//!
//! # Contract
//!
//! * A conversation belongs to exactly one account. Every lookup is scoped by
//!   `(account, id)`; a conversation of another tenant is `NotFound`.
//! * A user turn is persisted **as received** ([`StoredUserTurn`]) and an
//!   assistant turn is persisted **exactly as returned** to the client: the same
//!   [`AssistantTurn`] value, with the same ordered blocks, must come back from
//!   [`ConversationReader::load_turn`]. Reload never reconstructs cards or receipts
//!   from free text (ADR-012 point 8).
//! * Appending a user turn also creates its phase marker at
//!   [`TurnPhase::Received`]. The marker is the crash-recovery anchor of
//!   spec §23.1: a recovery sweep lists turns whose phase is not terminal and
//!   decides, per turn, whether to re-interpret, resume by idempotency key or
//!   regenerate the response.
//! * A terminal phase ([`TurnPhase::Delivered`], [`TurnPhase::Failed`]) is
//!   final: setting any other phase afterwards is a `Conflict`. Setting the same
//!   phase again is accepted (idempotent).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use turnframe_core::ids::{AccountId, ConversationId, TurnId};
use turnframe_core::replay::TurnPhase;
use turnframe_core::response::AssistantTurn;
use turnframe_core::turn::TurnInput;

use crate::error::StoreError;

/// A conversation (a chat thread) owned by an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationRecord {
    /// Identifier.
    pub id: ConversationId,
    /// Owning tenant.
    pub account_id: AccountId,
    /// Creation time, supplied by the caller.
    pub created_at: DateTime<Utc>,
    /// Application-defined metadata (title, channel, external ids...). Never
    /// interpreted by the library.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ConversationRecord {
    /// Builds a record without metadata.
    #[must_use]
    pub fn new(id: ConversationId, account_id: AccountId, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            account_id,
            created_at,
            metadata: serde_json::Value::Null,
        }
    }

    /// Attaches metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// A user turn as accepted by the runtime, with the time it was received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredUserTurn {
    /// The input, unchanged.
    pub input: TurnInput,
    /// When the runtime accepted it.
    pub received_at: DateTime<Utc>,
}

impl StoredUserTurn {
    /// Pairs an input with its reception time.
    #[must_use]
    pub fn new(input: TurnInput, received_at: DateTime<Utc>) -> Self {
        Self { input, received_at }
    }

    /// The owning account (the actor's account).
    #[must_use]
    pub fn account_id(&self) -> &AccountId {
        &self.input.actor.account_id
    }

    /// The turn identifier.
    #[must_use]
    pub fn turn_id(&self) -> TurnId {
        self.input.turn_id
    }

    /// The conversation the turn belongs to.
    #[must_use]
    pub fn conversation_id(&self) -> ConversationId {
        self.input.conversation_id
    }
}

/// A persisted turn: the user input, the assistant turn once composed, and the
/// current phase marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTurn {
    /// The user side.
    pub user: StoredUserTurn,
    /// The assistant side, present once persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant: Option<AssistantTurn>,
    /// Current phase.
    pub phase: TurnPhase,
}

/// The crash-recovery marker of one turn (spec §23.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnPhaseMarker {
    /// Owning tenant.
    pub account_id: AccountId,
    /// The conversation.
    pub conversation_id: ConversationId,
    /// The turn.
    pub turn_id: TurnId,
    /// Last persisted phase.
    pub phase: TurnPhase,
    /// When the phase was last written, stamped by the store's clock.
    pub updated_at: DateTime<Utc>,
}

/// Which turns a recovery sweep looks at.
///
/// Recovery is a system operation, not a user lookup, so it may legitimately
/// span every tenant; every returned marker still carries its `account_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryScope {
    /// Only turns of one account.
    Account(AccountId),
    /// Turns of every account.
    AllAccounts,
}

impl RecoveryScope {
    /// Returns `true` when `account` falls inside the scope.
    #[must_use]
    pub fn includes(&self, account: &AccountId) -> bool {
        match self {
            Self::Account(scoped) => scoped == account,
            Self::AllAccounts => true,
        }
    }
}

/// The read half of the conversation contract.
///
/// A holder of this trait can answer questions about conversations, turns and
/// phase markers and cannot change any of them. It is what the plan-only path
/// of [`turnframe-runtime`](https://docs.rs/turnframe-runtime) is handed, so
/// "this path does not write" is a fact about its types rather than a promise
/// about its code.
#[async_trait]
pub trait ConversationReader: Send + Sync {
    /// Loads a conversation of `account`.
    ///
    /// # Errors
    /// * `NotFound` when it does not exist for this account.
    async fn load_conversation(
        &self,
        account: &AccountId,
        id: &ConversationId,
    ) -> Result<ConversationRecord, StoreError>;

    /// Loads the most recent `limit` turns of a conversation in chronological
    /// order (oldest of the selected first). Ordering is by `received_at`, then
    /// `turn_id`.
    ///
    /// # Errors
    /// * `NotFound` when the conversation does not exist for `account`.
    async fn load_recent_turns(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<StoredTurn>, StoreError>;

    /// Loads one turn.
    ///
    /// # Errors
    /// * `NotFound` when it does not exist for `account`.
    async fn load_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<StoredTurn, StoreError>;

    /// Reads the phase marker of a turn.
    ///
    /// # Errors
    /// * `NotFound` when the turn does not exist for `account`.
    async fn turn_phase(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<TurnPhaseMarker, StoreError>;

    /// Lists up to `limit` markers of turns whose phase is not terminal, oldest
    /// first (by `received_at`, then `turn_id`).
    ///
    /// # Errors
    /// * [`StoreError`] when the sweep could not be read.
    async fn list_unfinished_turns(
        &self,
        scope: RecoveryScope,
        limit: usize,
    ) -> Result<Vec<TurnPhaseMarker>, StoreError>;
}

/// The write half of the conversation contract.
#[async_trait]
pub trait ConversationWriter: Send + Sync {
    /// Creates a conversation.
    ///
    /// # Errors
    /// * `Conflict` when a conversation with the same `(account_id, id)` exists.
    async fn create_conversation(&self, record: ConversationRecord) -> Result<(), StoreError>;

    /// Appends a user turn to its conversation and creates its phase marker at
    /// [`TurnPhase::Received`].
    ///
    /// The account is the actor's account inside the input.
    ///
    /// # Errors
    /// * `NotFound` when the conversation does not exist for the actor's account.
    /// * `Conflict` when a turn with the same `(account, turn_id)` exists.
    async fn append_user_turn(&self, turn: StoredUserTurn) -> Result<(), StoreError>;

    /// Persists the assistant turn that answers a user turn, exactly as returned
    /// to the client.
    ///
    /// # Errors
    /// * `NotFound` when the user turn does not exist for `account`.
    /// * `Conflict` when the turn already has an assistant turn.
    /// * `Other(IDENTITY_MISMATCH)` when `turn.conversation_id` differs from the
    ///   user turn's conversation.
    async fn append_assistant_turn(
        &self,
        account: &AccountId,
        turn: AssistantTurn,
    ) -> Result<(), StoreError>;

    /// Writes the phase marker of a turn and returns the updated marker.
    ///
    /// # Errors
    /// * `NotFound` when the turn does not exist for `account`.
    /// * `Conflict` when the current phase is terminal and `phase` differs.
    async fn set_turn_phase(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
        phase: TurnPhase,
    ) -> Result<TurnPhaseMarker, StoreError>;
}

/// Persistence of conversations, turns and phase markers: both halves.
///
/// Every method is account-scoped; see the module documentation for the rules
/// implementations must honour. The conformance suite proves them
/// ([`crate::conformance::check_conversation_turn_persistence`]).
///
/// There is nothing to implement here. Write
/// [`ConversationReader`] and [`ConversationWriter`] and this trait follows
/// from the blanket implementation below, so `Arc<dyn ConversationStore>` keeps
/// naming one value that does everything.
pub trait ConversationStore: ConversationReader + ConversationWriter {}

impl<T: ConversationReader + ConversationWriter + ?Sized> ConversationStore for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::locale::Locale;
    use turnframe_core::turn::ActorContext;

    #[test]
    fn stored_user_turn_accessors() {
        let input = TurnInput {
            turn_id: TurnId::nil(),
            conversation_id: ConversationId::nil(),
            actor: ActorContext::new("acct", "u"),
            text: Some("hi".into()),
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: Locale::from("en"),
            effort: None,
        };
        let turn = StoredUserTurn::new(input, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(turn.account_id(), &AccountId::from("acct"));
        assert_eq!(turn.turn_id(), TurnId::nil());
        assert_eq!(turn.conversation_id(), ConversationId::nil());
    }

    #[test]
    fn recovery_scope_membership() {
        let a = AccountId::from("a");
        assert!(RecoveryScope::AllAccounts.includes(&a));
        assert!(RecoveryScope::Account(a.clone()).includes(&a));
        assert!(!RecoveryScope::Account(AccountId::from("b")).includes(&a));
    }

    #[test]
    fn conversation_record_round_trips() {
        let record = ConversationRecord::new(
            ConversationId::nil(),
            AccountId::from("a"),
            DateTime::<Utc>::UNIX_EPOCH,
        )
        .with_metadata(serde_json::json!({"title": "t"}));
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(
            serde_json::from_str::<ConversationRecord>(&json).unwrap(),
            record
        );
    }
}
