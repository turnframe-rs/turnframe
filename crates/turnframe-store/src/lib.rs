//! `turnframe-store`: the persistence contract of Turnframe, plus a
//! deterministic in-memory implementation and an executable conformance suite.
//!
//! It defines *what* must be durable and *with which rules*, as seven
//! object-safe traits, and nothing about *where*: no driver, no SQL. The
//! PostgreSQL adapter is one implementation and is optional. The rules an
//! implementation must honour — total account scoping, a closed error surface,
//! compare-and-swap instead of blind overwrite, immutability, specified
//! ordering — and the all-or-nothing commit model are in
//! [`docs/persistence.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/persistence.md).
//! [`conformance::run_all`] proves an implementation right without reading its
//! code, and [`MemoryStores`] is the reference one.
//!
//! Six traits are split into a `…Reader` and a `…Writer` half, aggregated by
//! the trait carrying the historical name, so a caller that must not write can
//! be handed [`ReadOnlyStores`] and the compiler keeps it that way.
//! [`Stores`] bundles one implementation of each.
//!
//! | Trait | Module | What it owns |
//! |---|---|---|
//! | [`ConversationStore`](conversation::ConversationStore) | [`conversation`] | conversations, user turns, assistant turns *as returned*, the crash-recovery phase marker |
//! | [`InteractionStore`](interaction::InteractionStore) | [`interaction`] | persisted cards: payloads, the one-blocking-per-case slot, compare-and-swap resolution, expiry |
//! | [`CommandJournal`](journal::CommandJournal) | [`journal`] | idempotency admission and the persisted outcome of every command |
//! | [`EventJournal`](events::EventJournal) | [`events`] | the append-only claim ledger, paged exactly once, erasable only by redaction |
//! | [`OutboxStore`](outbox::OutboxStore) | [`outbox`] | external side effects awaiting dispatch, with claim/reschedule semantics |
//! | [`ReplayStore`](replay::ReplayStore) | [`replay`] | one replay record per turn, upserted as the turn advances |
//! | [`CommitStore`](commit::CommitStore) | [`commit`] | the all-or-nothing write of everything one commit produces |
//!
//! # Example
//!
//! ```rust
//! use turnframe_core::ids::{AccountId, ConversationId};
//! use turnframe_store::prelude::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let stores = Stores::in_memory();
//! let account = AccountId::from("aurora");
//! let conversation = ConversationId::new();
//! let now = chrono::Utc::now();
//!
//! stores
//!     .conversations()
//!     .create_conversation(ConversationRecord::new(
//!         conversation,
//!         account.clone(),
//!         now,
//!     ))
//!     .await?;
//!
//! let loaded = stores
//!     .conversations()
//!     .load_conversation(&account, &conversation)
//!     .await?;
//! assert_eq!(loaded.account_id, account);
//!
//! // Another tenant cannot tell it apart from one that never existed.
//! let other = AccountId::from("other");
//! assert_eq!(
//!     stores
//!         .conversations()
//!         .load_conversation(&other, &conversation)
//!         .await,
//!     Err(StoreError::NotFound)
//! );
//! # Ok::<(), StoreError>(())
//! # })?;
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its examples cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod commit;
pub mod conformance;
pub mod conversation;
pub mod error;
pub mod events;
pub mod interaction;
pub mod journal;
pub mod memory;
pub mod outbox;
pub mod replay;
pub mod stores;

// The handful of names an application says out loud, at the crate root, so a
// caller writes `turnframe_store::Stores` rather than `stores::Stores` through
// a module whose name repeats the type's. The modules stay public for
// everything else.
pub use crate::error::{StoreError, StoreResult};
pub use crate::memory::{Clock, ManualClock, MemoryStores, SystemClock};
pub use crate::stores::{ReadOnlyStores, ReadOnlyStoresBuilder, Stores, StoresBuilder};

/// The items an application needs to hold and use a set of stores.
///
/// Adapter authors additionally want the record and outcome types from the
/// individual modules; this prelude carries the traits, the aggregate, the
/// error surface and the in-memory implementation.
pub mod prelude {
    pub use crate::commit::{CommitBundle, CommitReceipt, CommitStore};
    pub use crate::conversation::{
        ConversationReader, ConversationRecord, ConversationStore, ConversationWriter,
        RecoveryScope, StoredTurn, StoredUserTurn, TurnPhaseMarker,
    };
    pub use crate::error::{StoreError, StoreResult};
    pub use crate::events::{
        EventBatch, EventCursor, EventJournal, EventJournalReader, EventJournalWriter, EventPage,
        LedgerReceiptGroup, StoredEvent, group_for_receipts,
    };
    pub use crate::interaction::{
        InteractionReader, InteractionRecord, InteractionStore, InteractionWriter,
        InvalidationReason, ResolutionOutcome,
    };
    pub use crate::journal::{
        CommandJournal, CommandJournalEntry, CommandJournalReader, CommandJournalStatus,
        CommandJournalWriter, JournalAdmission, JournalOutcome,
    };
    pub use crate::memory::{Clock, ManualClock, MemoryStores, SystemClock};
    pub use crate::outbox::{OutboxReader, OutboxRecord, OutboxStore, OutboxWriter};
    pub use crate::replay::{ReplayReader, ReplayStore, ReplayWriter};
    pub use crate::stores::{ReadOnlyStores, Stores};
}
