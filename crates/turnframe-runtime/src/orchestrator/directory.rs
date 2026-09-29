//! Which cases an actor may address: the application's authorization boundary for
//! records (spec §23 step D, §25.4).

use async_trait::async_trait;
use turnframe_core::case::CaseKey;
use turnframe_core::error::StoreError;
use turnframe_core::ids::{ConversationId, OriginToken, WorkflowKey};
use turnframe_core::turn::ActorContext;

/// One case the actor may address in a conversation.
///
/// It carries no revision: the executor is read at the start of every turn, so a
/// turn never plans against a state that has moved (I1, I13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseCandidate {
    /// Workflow and case identifier.
    pub key: CaseKey,
    /// Server-authored label the model and the cards see instead of the id.
    pub label: String,
    /// Every write on this case needs a click, as `AskBeforeApplying` would ask.
    ///
    /// For a record the actor may reach and did not necessarily mean. It raises a
    /// confirmation and never refuses.
    pub confirm_every_write: bool,
    /// The case is in view only because the actor may reach it.
    ///
    /// It stays addressable, and becomes a subject only when an act names it: it does
    /// not hold the door against a new case, brief the writer or ask for its fields.
    /// See [`ReductionContext::subject_only_when_named`](turnframe_core::reduce::ReductionContext::subject_only_when_named).
    pub subject_only_when_named: bool,
}

impl CaseCandidate {
    /// A candidate with a label.
    #[must_use]
    pub fn new(key: CaseKey, label: impl Into<String>) -> Self {
        Self {
            key,
            label: label.into(),
            confirm_every_write: false,
            subject_only_when_named: false,
        }
    }

    /// Declares that every write on this case has to pass through a click.
    #[must_use]
    pub const fn confirming_every_write(mut self) -> Self {
        self.confirm_every_write = true;
        self
    }

    /// Declares that this case is a subject of a turn only when the turn names it.
    #[must_use]
    pub const fn subject_only_when_named(mut self) -> Self {
        self.subject_only_when_named = true;
        self
    }
}

/// Names the cases an actor may address (spec §23 step D, §25.4).
///
/// Whatever it does not return, the model never learns exists. The account is the
/// boundary the runtime enforces; any narrower scope (an organization, a workspace)
/// is enforced here and nowhere else, because the executor's `load` never sees the
/// actor. Every case a turn addresses passes through one of these methods.
#[async_trait]
pub trait CaseDirectory: Send + Sync {
    /// The cases `actor` may address in `conversation`. A record a turn created is
    /// addressable on the next turn only when it is listed here.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the directory could not be read. The turn fails closed (I19).
    async fn candidates(
        &self,
        actor: &ActorContext,
        conversation: &ConversationId,
    ) -> Result<Vec<CaseCandidate>, StoreError>;

    /// Whether `actor` may address `key`, an existing record an open card names that
    /// [`candidates`](Self::candidates) did not return. `None` refuses, and the card
    /// is dropped from the turn.
    ///
    /// **The default refuses**, so the candidate list is the whole definition of what
    /// an actor may address. Override it when that list is deliberately narrower.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the lookup failed. The turn fails closed (I19).
    async fn authorize_case(
        &self,
        actor: &ActorContext,
        conversation: &ConversationId,
        key: &CaseKey,
    ) -> Result<Option<CaseCandidate>, StoreError> {
        let _ = (actor, conversation, key);
        Ok(None)
    }

    /// The records of `workflow` the actor may address that `named` names, for a
    /// message naming one [`candidates`](Self::candidates) did not list. `named` is
    /// the user's words, when they used any. One found record is the act's target;
    /// several are offered on a selection card. The default finds none.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the lookup failed. The turn fails closed (I19).
    async fn find(
        &self,
        actor: &ActorContext,
        conversation: &ConversationId,
        workflow: &WorkflowKey,
        named: Option<&str>,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        let _ = (actor, conversation, workflow, named);
        Ok(Vec::new())
    }

    /// The record a server-issued origin token names (spec §12.4). The default
    /// recognises none.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the lookup failed.
    async fn resolve_origin(
        &self,
        actor: &ActorContext,
        origin: &OriginToken,
    ) -> Result<Option<CaseCandidate>, StoreError> {
        let _ = (actor, origin);
        Ok(None)
    }
}

/// A directory with a fixed list, for tests, examples and single-case surfaces.
///
/// It keeps the default [`authorize_case`](CaseDirectory::authorize_case), so the list
/// is the whole truth.
#[derive(Debug, Clone, Default)]
pub struct StaticCaseDirectory {
    candidates: Vec<CaseCandidate>,
    origins: Vec<(OriginToken, CaseCandidate)>,
}

impl StaticCaseDirectory {
    /// An empty directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a candidate.
    #[must_use]
    pub fn with_case(mut self, candidate: CaseCandidate) -> Self {
        self.candidates.push(candidate);
        self
    }

    /// Binds an origin token to a record.
    #[must_use]
    pub fn with_origin(mut self, origin: OriginToken, candidate: CaseCandidate) -> Self {
        self.origins.push((origin, candidate));
        self
    }
}

#[async_trait]
impl CaseDirectory for StaticCaseDirectory {
    async fn candidates(
        &self,
        _actor: &ActorContext,
        _conversation: &ConversationId,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        Ok(self.candidates.clone())
    }

    async fn resolve_origin(
        &self,
        _actor: &ActorContext,
        origin: &OriginToken,
    ) -> Result<Option<CaseCandidate>, StoreError> {
        Ok(self
            .origins
            .iter()
            .find(|(token, _)| token == origin)
            .map(|(_, candidate)| candidate.clone()))
    }
}
