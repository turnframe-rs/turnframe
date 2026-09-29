//! [`Stores`]: one value carrying an implementation of every persistence trait.
//!
//! The runtime needs all seven stores and should not care whether they come
//! from one backend or seven. `Stores` holds them behind `Arc<dyn …>`, so it is
//! cheap to clone, shareable across tasks, and mixable: a deployment may keep
//! conversations in PostgreSQL and the outbox somewhere else without any type
//! elsewhere in the library learning about it.
//!
//! # Building one
//!
//! * [`Stores::in_memory`] for tests, examples and single-process runs.
//! * [`Stores::from_memory`] when the test needs to keep the
//!   [`MemoryStores`] handle too, to drive its clock or arm a failure.
//! * [`Stores::builder`] when the implementations differ.
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use turnframe_store::memory::MemoryStores;
//! use turnframe_store::stores::Stores;
//!
//! # fn main() -> Result<(), turnframe_store::stores::StoresBuilderError> {
//! let backend = Arc::new(MemoryStores::new());
//! let stores = Stores::builder()
//!     .conversations(backend.clone())
//!     .interactions(backend.clone())
//!     .journal(backend.clone())
//!     .events(backend.clone())
//!     .outbox(backend.clone())
//!     .replay(backend.clone())
//!     .commit(backend)
//!     .build()?;
//! assert!(format!("{stores:?}").contains("conversations"));
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::sync::Arc;

use crate::commit::CommitStore;
use crate::conversation::{ConversationReader, ConversationStore};
use crate::events::{EventJournal, EventJournalReader};
use crate::interaction::{InteractionReader, InteractionStore};
use crate::journal::{CommandJournal, CommandJournalReader};
use crate::memory::MemoryStores;
use crate::outbox::{OutboxReader, OutboxStore};
use crate::replay::{ReplayReader, ReplayStore};

/// A complete set of stores.
///
/// Cloning shares the same underlying implementations; it never copies data.
#[derive(Clone)]
pub struct Stores {
    conversations: Arc<dyn ConversationStore>,
    interactions: Arc<dyn InteractionStore>,
    journal: Arc<dyn CommandJournal>,
    events: Arc<dyn EventJournal>,
    outbox: Arc<dyn OutboxStore>,
    replay: Arc<dyn ReplayStore>,
    commit: Arc<dyn CommitStore>,
}

impl Stores {
    /// An empty [`MemoryStores`] behind every trait.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::from_memory(Arc::new(MemoryStores::new()))
    }

    /// Every trait served by one [`MemoryStores`] the caller keeps a handle to.
    ///
    /// Use it when the test also has to drive the store's clock or arm a
    /// [`FailurePoint`](crate::memory::FailurePoint).
    #[must_use]
    pub fn from_memory(backend: Arc<MemoryStores>) -> Self {
        Self {
            conversations: backend.clone(),
            interactions: backend.clone(),
            journal: backend.clone(),
            events: backend.clone(),
            outbox: backend.clone(),
            replay: backend.clone(),
            commit: backend,
        }
    }

    /// A builder that requires every store to be supplied.
    #[must_use]
    pub fn builder() -> StoresBuilder {
        StoresBuilder::default()
    }

    /// Conversations, turns and phase markers.
    #[must_use]
    pub fn conversations(&self) -> &Arc<dyn ConversationStore> {
        &self.conversations
    }

    /// Persisted interactions.
    #[must_use]
    pub fn interactions(&self) -> &Arc<dyn InteractionStore> {
        &self.interactions
    }

    /// The command journal.
    #[must_use]
    pub fn journal(&self) -> &Arc<dyn CommandJournal> {
        &self.journal
    }

    /// The claim ledger.
    #[must_use]
    pub fn events(&self) -> &Arc<dyn EventJournal> {
        &self.events
    }

    /// The external-effect outbox.
    #[must_use]
    pub fn outbox(&self) -> &Arc<dyn OutboxStore> {
        &self.outbox
    }

    /// Replay records.
    #[must_use]
    pub fn replay(&self) -> &Arc<dyn ReplayStore> {
        &self.replay
    }

    /// The all-or-nothing bundle writer.
    #[must_use]
    pub fn commit(&self) -> &Arc<dyn CommitStore> {
        &self.commit
    }

    /// The same persistence layer with the write side removed
    /// (spec §23, plan-only path).
    ///
    /// Each store is upcast to its read half, so the returned value has no
    /// method that changes anything and no way to recover one: there is no
    /// downcast back to [`Stores`], and [`CommitStore`] — which is write-only —
    /// is simply absent. It is what a caller hands to a path that must be
    /// unable to write, such as
    /// [`Orchestrator::plan_turn`](https://docs.rs/turnframe-runtime), so that
    /// "this path does not persist anything" is checked by the compiler rather
    /// than by review.
    #[must_use]
    pub fn read_only(&self) -> ReadOnlyStores {
        ReadOnlyStores {
            conversations: self.conversations.clone(),
            interactions: self.interactions.clone(),
            journal: self.journal.clone(),
            events: self.events.clone(),
            outbox: self.outbox.clone(),
            replay: self.replay.clone(),
        }
    }
}

/// The read half of a persistence layer: six readers, no writer of any kind.
///
/// Build one with [`Stores::read_only`], or with [`ReadOnlyStores::builder`]
/// when the reading implementations are not backed by a full [`Stores`] at all.
/// Cloning shares the same underlying implementations.
///
/// [`CommitStore`] has no read half — a commit is
/// the write — so it has no counterpart here.
#[derive(Clone)]
pub struct ReadOnlyStores {
    conversations: Arc<dyn ConversationReader>,
    interactions: Arc<dyn InteractionReader>,
    journal: Arc<dyn CommandJournalReader>,
    events: Arc<dyn EventJournalReader>,
    outbox: Arc<dyn OutboxReader>,
    replay: Arc<dyn ReplayReader>,
}

impl ReadOnlyStores {
    /// An empty [`MemoryStores`] behind every reader.
    #[must_use]
    pub fn in_memory() -> Self {
        Stores::in_memory().read_only()
    }

    /// A builder that requires every reader to be supplied.
    #[must_use]
    pub fn builder() -> ReadOnlyStoresBuilder {
        ReadOnlyStoresBuilder::default()
    }

    /// Conversations, turns and phase markers.
    #[must_use]
    pub fn conversations(&self) -> &Arc<dyn ConversationReader> {
        &self.conversations
    }

    /// Persisted interactions.
    #[must_use]
    pub fn interactions(&self) -> &Arc<dyn InteractionReader> {
        &self.interactions
    }

    /// The command journal.
    #[must_use]
    pub fn journal(&self) -> &Arc<dyn CommandJournalReader> {
        &self.journal
    }

    /// The claim ledger.
    #[must_use]
    pub fn events(&self) -> &Arc<dyn EventJournalReader> {
        &self.events
    }

    /// The external-effect outbox.
    #[must_use]
    pub fn outbox(&self) -> &Arc<dyn OutboxReader> {
        &self.outbox
    }

    /// Replay records.
    #[must_use]
    pub fn replay(&self) -> &Arc<dyn ReplayReader> {
        &self.replay
    }
}

impl From<&Stores> for ReadOnlyStores {
    fn from(stores: &Stores) -> Self {
        stores.read_only()
    }
}

impl fmt::Debug for ReadOnlyStores {
    /// Names the roles, never the implementations.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadOnlyStores")
            .field("roles", &ReadOnlyStoresBuilder::ROLES)
            .finish()
    }
}

/// Collects one reader per role.
#[derive(Clone, Default)]
pub struct ReadOnlyStoresBuilder {
    conversations: Option<Arc<dyn ConversationReader>>,
    interactions: Option<Arc<dyn InteractionReader>>,
    journal: Option<Arc<dyn CommandJournalReader>>,
    events: Option<Arc<dyn EventJournalReader>>,
    outbox: Option<Arc<dyn OutboxReader>>,
    replay: Option<Arc<dyn ReplayReader>>,
}

impl ReadOnlyStoresBuilder {
    /// The six role names, in the order [`ReadOnlyStoresBuilder::build`] checks
    /// them.
    pub const ROLES: [&'static str; 6] = [
        "conversations",
        "interactions",
        "journal",
        "events",
        "outbox",
        "replay",
    ];

    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the conversation reader.
    #[must_use]
    pub fn conversations(mut self, store: Arc<dyn ConversationReader>) -> Self {
        self.conversations = Some(store);
        self
    }

    /// Sets the interaction reader.
    #[must_use]
    pub fn interactions(mut self, store: Arc<dyn InteractionReader>) -> Self {
        self.interactions = Some(store);
        self
    }

    /// Sets the command journal reader.
    #[must_use]
    pub fn journal(mut self, store: Arc<dyn CommandJournalReader>) -> Self {
        self.journal = Some(store);
        self
    }

    /// Sets the event journal reader.
    #[must_use]
    pub fn events(mut self, store: Arc<dyn EventJournalReader>) -> Self {
        self.events = Some(store);
        self
    }

    /// Sets the outbox reader.
    #[must_use]
    pub fn outbox(mut self, store: Arc<dyn OutboxReader>) -> Self {
        self.outbox = Some(store);
        self
    }

    /// Sets the replay reader.
    #[must_use]
    pub fn replay(mut self, store: Arc<dyn ReplayReader>) -> Self {
        self.replay = Some(store);
        self
    }

    /// Builds the set.
    ///
    /// # Errors
    /// * [`StoresBuilderError::MissingStore`] naming the first role that was
    ///   never supplied.
    pub fn build(self) -> Result<ReadOnlyStores, StoresBuilderError> {
        fn required<T: ?Sized>(
            store: Option<Arc<T>>,
            role: &'static str,
        ) -> Result<Arc<T>, StoresBuilderError> {
            store.ok_or(StoresBuilderError::MissingStore { role })
        }

        Ok(ReadOnlyStores {
            conversations: required(self.conversations, Self::ROLES[0])?,
            interactions: required(self.interactions, Self::ROLES[1])?,
            journal: required(self.journal, Self::ROLES[2])?,
            events: required(self.events, Self::ROLES[3])?,
            outbox: required(self.outbox, Self::ROLES[4])?,
            replay: required(self.replay, Self::ROLES[5])?,
        })
    }
}

impl fmt::Debug for ReadOnlyStoresBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let supplied: Vec<&'static str> = Self::ROLES
            .iter()
            .zip([
                self.conversations.is_some(),
                self.interactions.is_some(),
                self.journal.is_some(),
                self.events.is_some(),
                self.outbox.is_some(),
                self.replay.is_some(),
            ])
            .filter_map(|(role, present)| present.then_some(*role))
            .collect();
        f.debug_struct("ReadOnlyStoresBuilder")
            .field("supplied", &supplied)
            .finish()
    }
}

impl fmt::Debug for Stores {
    /// Names the roles, never the implementations: a store may hold a
    /// connection string and this output is safe to log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stores")
            .field("roles", &StoresBuilder::ROLES)
            .finish()
    }
}

/// Why a [`Stores`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoresBuilderError {
    /// One of the seven roles was not supplied.
    #[error("no implementation supplied for the {role} store")]
    MissingStore {
        /// The role, one of [`StoresBuilder::ROLES`].
        role: &'static str,
    },
}

/// Collects one implementation per role.
///
/// Every role is mandatory: a half-configured persistence layer is a bug that
/// should surface at start-up, not at the first turn that needs the missing
/// store.
#[derive(Clone, Default)]
pub struct StoresBuilder {
    conversations: Option<Arc<dyn ConversationStore>>,
    interactions: Option<Arc<dyn InteractionStore>>,
    journal: Option<Arc<dyn CommandJournal>>,
    events: Option<Arc<dyn EventJournal>>,
    outbox: Option<Arc<dyn OutboxStore>>,
    replay: Option<Arc<dyn ReplayStore>>,
    commit: Option<Arc<dyn CommitStore>>,
}

impl StoresBuilder {
    /// The seven role names, in the order [`StoresBuilder::build`] checks them.
    pub const ROLES: [&'static str; 7] = [
        "conversations",
        "interactions",
        "journal",
        "events",
        "outbox",
        "replay",
        "commit",
    ];

    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the conversation store.
    #[must_use]
    pub fn conversations(mut self, store: Arc<dyn ConversationStore>) -> Self {
        self.conversations = Some(store);
        self
    }

    /// Sets the interaction store.
    #[must_use]
    pub fn interactions(mut self, store: Arc<dyn InteractionStore>) -> Self {
        self.interactions = Some(store);
        self
    }

    /// Sets the command journal.
    #[must_use]
    pub fn journal(mut self, store: Arc<dyn CommandJournal>) -> Self {
        self.journal = Some(store);
        self
    }

    /// Sets the event journal.
    #[must_use]
    pub fn events(mut self, store: Arc<dyn EventJournal>) -> Self {
        self.events = Some(store);
        self
    }

    /// Sets the outbox.
    #[must_use]
    pub fn outbox(mut self, store: Arc<dyn OutboxStore>) -> Self {
        self.outbox = Some(store);
        self
    }

    /// Sets the replay store.
    #[must_use]
    pub fn replay(mut self, store: Arc<dyn ReplayStore>) -> Self {
        self.replay = Some(store);
        self
    }

    /// Sets the commit store.
    #[must_use]
    pub fn commit(mut self, store: Arc<dyn CommitStore>) -> Self {
        self.commit = Some(store);
        self
    }

    /// Builds the set.
    ///
    /// # Errors
    /// * [`StoresBuilderError::MissingStore`] naming the first role that was
    ///   never supplied.
    pub fn build(self) -> Result<Stores, StoresBuilderError> {
        fn required<T: ?Sized>(
            store: Option<Arc<T>>,
            role: &'static str,
        ) -> Result<Arc<T>, StoresBuilderError> {
            store.ok_or(StoresBuilderError::MissingStore { role })
        }

        Ok(Stores {
            conversations: required(self.conversations, Self::ROLES[0])?,
            interactions: required(self.interactions, Self::ROLES[1])?,
            journal: required(self.journal, Self::ROLES[2])?,
            events: required(self.events, Self::ROLES[3])?,
            outbox: required(self.outbox, Self::ROLES[4])?,
            replay: required(self.replay, Self::ROLES[5])?,
            commit: required(self.commit, Self::ROLES[6])?,
        })
    }
}

impl fmt::Debug for StoresBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let supplied: Vec<&'static str> = Self::ROLES
            .iter()
            .zip([
                self.conversations.is_some(),
                self.interactions.is_some(),
                self.journal.is_some(),
                self.events.is_some(),
                self.outbox.is_some(),
                self.replay.is_some(),
                self.commit.is_some(),
            ])
            .filter_map(|(role, present)| present.then_some(*role))
            .collect();
        f.debug_struct("StoresBuilder")
            .field("supplied", &supplied)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::ids::{AccountId, ConversationId};

    #[test]
    fn a_store_set_is_shareable_across_tasks() {
        // The runtime holds one `Stores` and hands it to every turn, on any
        // executor thread. If this stops compiling, a trait lost `Send + Sync`.
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Stores>();
        assert_send_sync::<StoresBuilder>();
        assert_send_sync::<MemoryStores>();
    }

    #[test]
    fn builder_names_the_first_missing_role() {
        assert_eq!(
            Stores::builder().build().unwrap_err(),
            StoresBuilderError::MissingStore {
                role: "conversations"
            }
        );
        let backend = Arc::new(MemoryStores::new());
        let err = Stores::builder()
            .conversations(backend.clone())
            .interactions(backend.clone())
            .build()
            .unwrap_err();
        assert_eq!(err, StoresBuilderError::MissingStore { role: "journal" });
        assert!(
            err.to_string()
                .contains("no implementation supplied for the journal store")
        );
    }

    #[test]
    fn builder_accepts_one_backend_for_every_role() {
        let backend = Arc::new(MemoryStores::new());
        let stores = Stores::builder()
            .conversations(backend.clone())
            .interactions(backend.clone())
            .journal(backend.clone())
            .events(backend.clone())
            .outbox(backend.clone())
            .replay(backend.clone())
            .commit(backend)
            .build()
            .unwrap();
        assert!(format!("{stores:?}").contains("conversations"));
    }

    #[tokio::test]
    async fn the_read_only_view_answers_what_the_full_set_answers() {
        let backend = Arc::new(MemoryStores::new());
        let stores = Stores::from_memory(backend);
        let account = AccountId::from("a");
        let conversation = ConversationId::new();
        stores
            .conversations()
            .create_conversation(crate::conversation::ConversationRecord::new(
                conversation,
                account.clone(),
                chrono::Utc::now(),
            ))
            .await
            .unwrap();

        let reading = stores.read_only();
        assert_eq!(
            reading
                .conversations()
                .load_conversation(&account, &conversation)
                .await
                .unwrap()
                .account_id,
            account
        );
        // There is nothing to call here that writes: the view carries the six
        // readers and no commit store at all.
        assert!(format!("{reading:?}").contains("conversations"));
        assert!(!format!("{reading:?}").contains("commit"));
    }

    #[test]
    fn the_read_only_builder_names_the_first_missing_role() {
        assert_eq!(
            ReadOnlyStores::builder().build().unwrap_err(),
            StoresBuilderError::MissingStore {
                role: "conversations"
            }
        );
        let backend = Arc::new(MemoryStores::new());
        let reading = ReadOnlyStores::builder()
            .conversations(backend.clone())
            .interactions(backend.clone())
            .journal(backend.clone())
            .events(backend.clone())
            .outbox(backend.clone())
            .replay(backend)
            .build()
            .unwrap();
        assert!(format!("{reading:?}").contains("replay"));
    }

    #[test]
    fn a_read_only_view_is_shareable_across_tasks() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<ReadOnlyStores>();
        assert_send_sync::<ReadOnlyStoresBuilder>();
    }

    #[tokio::test]
    async fn in_memory_shares_one_state_across_roles() {
        let backend = Arc::new(MemoryStores::new());
        let stores = Stores::from_memory(backend.clone());
        let account = AccountId::from("a");
        let conversation = ConversationId::new();
        stores
            .conversations()
            .create_conversation(crate::conversation::ConversationRecord::new(
                conversation,
                account.clone(),
                backend.now(),
            ))
            .await
            .unwrap();
        // The same state answers through the aggregate and through the handle.
        assert!(
            crate::conversation::ConversationReader::load_conversation(
                backend.as_ref(),
                &account,
                &conversation
            )
            .await
            .is_ok()
        );
        let cloned = stores.clone();
        assert!(
            cloned
                .conversations()
                .load_conversation(&account, &conversation)
                .await
                .is_ok()
        );
    }
}
