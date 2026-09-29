//! `turnframe-store-postgres`: the PostgreSQL implementation of the Turnframe
//! persistence contract.
//!
//! # This crate is optional
//!
//! The contract is [`turnframe-store`](turnframe_store): seven object-safe
//! traits and an executable conformance suite that proves an implementation
//! right. This crate is *one* implementation of them. An adopter with an
//! existing schema, another database, or a different operational story can
//! implement the traits themselves and never depend on this crate — the runtime
//! cannot tell the difference, and the conformance suite will say so either way.
//!
//! What you get by using it is a schema that has already been argued about: the
//! constraints below are where the rules of the contract live, so a race that
//! would break an invariant loses on an index rather than on a lucky
//! interleaving.
//!
//! # Getting started
//!
//! ```rust,no_run
//! use turnframe_core::ids::{AccountId, ConversationId};
//! use turnframe_store::prelude::*;
//! use turnframe_store_postgres::PgStores;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let store = PgStores::connect("postgres://turnframe@localhost/turnframe").await?;
//! store.migrate().await?;
//!
//! let stores: Stores = store.stores()?;
//! let account = AccountId::from("aurora");
//! let conversation = ConversationId::new();
//!
//! stores
//!     .conversations()
//!     .create_conversation(ConversationRecord::new(
//!         conversation,
//!         account.clone(),
//!         chrono::Utc::now(),
//!     ))
//!     .await?;
//!
//! // Another tenant cannot tell it apart from a conversation that never existed.
//! assert_eq!(
//!     stores
//!         .conversations()
//!         .load_conversation(&AccountId::from("other"), &conversation)
//!         .await,
//!     Err(StoreError::NotFound)
//! );
//! # Ok(())
//! # }
//! ```
//!
//! # Where each rule lives
//!
//! | Rule | How it is enforced |
//! |---|---|
//! | at most one open blocking card per case (I5) | the partial unique index `tf_one_open_blocking_interaction_per_case` |
//! | one admission per idempotency key (I14) | `UNIQUE (account_id, idempotency_key)` plus `INSERT … ON CONFLICT DO NOTHING`, then a re-read that replays the winner |
//! | one external action per destination | `UNIQUE (destination, idempotency_key)` |
//! | a claimed outbox row is one dispatcher's | `SELECT … FOR UPDATE SKIP LOCKED` inside the claiming statement |
//! | compare-and-swap, never blind overwrite | the expected state is the `WHERE` clause of the write; zero affected rows means the precondition failed |
//! | a bundle is all or nothing | one transaction, committed only after the last item succeeded |
//! | tenant isolation | `account_id` leads every primary key and every index, so a query that forgets it cannot use one |
//!
//! # Runtime-checked queries, on purpose
//!
//! Every statement in this crate goes through `sqlx::query` and reads its
//! columns by name. None of them uses the `sqlx::query!` family.
//!
//! Those macros check SQL against a live database *at compile time*, which is a
//! real benefit and the wrong trade for a published library: it makes the crate
//! unbuildable in a clean checkout unless a database is reachable or a
//! `.sqlx` cache is committed and kept in step with every edit. A contributor
//! with no PostgreSQL, a `cargo install`, a `docs.rs` build and a downstream
//! `cargo vendor` would all fail on something that has nothing to do with their
//! change.
//!
//! The check that macros would have given is bought back by the conformance
//! suite instead: it runs the whole persistence contract against a real
//! PostgreSQL 16, so a column renamed on one side and not the other fails a test
//! rather than a build — later, but against behaviour rather than shape.
//! [`sqlx::migrate!`] is still a macro and still used: it reads `migrations/`
//! while compiling and needs no database.
//!
//! # Assumptions this adapter makes
//!
//! * **`READ COMMITTED`.** PostgreSQL's default. The idempotency admission and
//!   every compare-and-swap rely on a statement re-reading a row another
//!   transaction has just committed. At `REPEATABLE READ` those become
//!   serialization failures, reported as `Conflict` — correct, but it turns
//!   routine contention into caller-visible refusals.
//! * **Microsecond timestamps.** `timestamptz` keeps microseconds; instants this
//!   adapter stamps are truncated to match, so a value written and read back
//!   compares equal.
//! * **One schema.** Set [`PgStoreConfig::schema`] to keep the tables out of
//!   `public`; [`PgStores::migrate`] creates it.
//!
//! See the [README](https://github.com/turnframe-rs/turnframe/blob/main/crates/turnframe-store-postgres/README.md)
//! for the schema table by table and for running the conformance suite.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its examples cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

mod codec;
mod commit;
mod config;
mod conversations;
pub mod error;
mod events;
mod interactions;
mod journal;
mod outbox;
mod replay;
mod store;

pub use crate::config::{ConfigError, PgStoreConfig};
pub use crate::store::{MIGRATOR, PgStores};

/// Applies every migration this crate carries to a pool the caller owns.
///
/// Equivalent to [`PgStores::migrate`] without the store, for a deployment that
/// migrates from a separate binary. It does not create a schema: point the
/// pool's `search_path` at one that exists, or use [`PgStores::migrate`], which
/// creates the configured schema first.
///
/// # Errors
/// * [`MigrateError`](sqlx::migrate::MigrateError) when a migration fails or an
///   already-applied migration no longer matches its recorded checksum.
pub async fn migrate(pool: &sqlx::PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}
