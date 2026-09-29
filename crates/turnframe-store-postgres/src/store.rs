//! [`PgStores`]: one PostgreSQL pool behind every persistence trait.

use std::fmt;
use std::sync::Arc;

use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Executor, Postgres, Transaction};
use turnframe_store::error::StoreError;
use turnframe_store::stores::{Stores, StoresBuilderError};

use crate::config::PgStoreConfig;
use crate::error::{commit_failed, store_error};

/// The migrations of this crate, embedded in the binary at compile time.
///
/// `sqlx::migrate!` reads `migrations/` while the crate is being compiled and
/// bakes the files in, so a deployed binary needs no directory beside it and no
/// database while it is being built. Applying them is
/// [`PgStores::migrate`]; running them from your own pool is
/// `MIGRATOR.run(&pool)`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Every Turnframe store over one PostgreSQL pool.
///
/// Implements [`ConversationStore`](turnframe_store::conversation::ConversationStore),
/// [`InteractionStore`](turnframe_store::interaction::InteractionStore),
/// [`CommandJournal`](turnframe_store::journal::CommandJournal),
/// [`EventJournal`](turnframe_store::events::EventJournal),
/// [`OutboxStore`](turnframe_store::outbox::OutboxStore),
/// [`ReplayStore`](turnframe_store::replay::ReplayStore) and
/// [`CommitStore`](turnframe_store::commit::CommitStore), so a card written
/// through the interaction trait is the card a commit bundle settles.
///
/// Cloning shares the pool; it never opens new connections.
#[derive(Clone)]
pub struct PgStores {
    pool: PgPool,
    schema: Option<String>,
}

impl PgStores {
    /// Opens a pool on `url` with the default [`PgStoreConfig`].
    ///
    /// The URL is a standard PostgreSQL connection string
    /// (`postgres://user:password@host:port/database`). It is never stored in a
    /// field this type can print.
    ///
    /// # Errors
    /// * `Other` when the URL cannot be parsed, `Unavailable` when the first
    ///   connection cannot be opened.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        Self::connect_with(url, &PgStoreConfig::new()).await
    }

    /// Opens a pool on `url` with `config`.
    ///
    /// # Errors
    /// * `Other` when the URL cannot be parsed, `Unavailable` when the first
    ///   connection cannot be opened.
    pub async fn connect_with(url: &str, config: &PgStoreConfig) -> Result<Self, StoreError> {
        let options: PgConnectOptions = url.parse().map_err(|error| store_error(&error))?;
        let pool = config
            .apply_pool(PgPoolOptions::new())
            .connect_with(config.apply_connection(options))
            .await
            .map_err(|error| store_error(&error))?;
        Ok(Self {
            pool,
            schema: config.schema_name().map(ToOwned::to_owned),
        })
    }

    /// Uses a pool the caller already has.
    ///
    /// Nothing is configured on it: the pool's own settings, its `search_path`
    /// and its statement timeout are whatever the caller set. Use this to share
    /// one pool with the rest of an application, so the store and the domain
    /// tables are reached through the same connections and can be enlisted in
    /// the same transaction (see [`Self::commit_in`]).
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool, schema: None }
    }

    /// Puts the tables of a caller-supplied pool in `schema`.
    ///
    /// Only tells [`Self::migrate`] which schema to create; the pool must
    /// already resolve to it, normally through a `search_path` in its connect
    /// options.
    ///
    /// # Errors
    /// * [`ConfigError::InvalidSchemaName`](crate::ConfigError::InvalidSchemaName)
    ///   when the name is not a plain unquoted identifier.
    pub fn with_schema(mut self, schema: impl Into<String>) -> Result<Self, crate::ConfigError> {
        let config = PgStoreConfig::new().schema(schema)?;
        self.schema = config.schema_name().map(ToOwned::to_owned);
        Ok(self)
    }

    /// Applies every migration this crate carries, creating the configured
    /// schema first when there is one.
    ///
    /// It is safe to call on every start-up: `sqlx` records applied versions and
    /// skips them, and the statements are written to be applied twice anyway.
    /// Two processes racing it is safe too — the migrator takes a database-wide
    /// advisory lock — though a deployment normally runs it once, before the
    /// instances that will use the schema come up.
    ///
    /// # Errors
    /// * [`MigrateError`](sqlx::migrate::MigrateError) when the schema cannot be
    ///   created, a migration fails, or an already-applied migration no longer
    ///   matches its recorded checksum.
    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        if let Some(schema) = &self.schema {
            // The name is a validated plain identifier, which is why it can be
            // interpolated: `CREATE SCHEMA` takes no bind parameter.
            let statement = format!("CREATE SCHEMA IF NOT EXISTS {schema}");
            match self.pool.execute(statement.as_str()).await {
                Ok(_) => {}
                // `IF NOT EXISTS` checks and creates in two steps, so two
                // instances starting at the same moment can both pass the check
                // and one of them loses. The schema exists either way, which is
                // all this call was asking for.
                Err(error) if created_concurrently(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }
        tracing::debug!(
            migrations = MIGRATOR.iter().len(),
            "applying turnframe store migrations"
        );
        MIGRATOR.run(&self.pool).await
    }

    /// The pool underneath, for health checks, metrics, or statements this
    /// adapter does not make.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The schema the tables live in, when one was configured.
    #[must_use]
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// Closes the pool and waits for its connections to be released.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Bundles this store behind all seven traits.
    ///
    /// # Errors
    /// * [`StoresBuilderError`] — never in practice. Every role is supplied
    ///   here, and the `Result` exists only because `turnframe-store` offers no
    ///   infallible "one backend for every role" constructor for a backend
    ///   other than its own in-memory one.
    pub fn stores(&self) -> Result<Stores, StoresBuilderError> {
        let backend = Arc::new(self.clone());
        Stores::builder()
            .conversations(backend.clone())
            .interactions(backend.clone())
            .journal(backend.clone())
            .events(backend.clone())
            .outbox(backend.clone())
            .replay(backend.clone())
            .commit(backend)
            .build()
    }

    /// Takes a connection from the pool for a single read.
    pub(crate) async fn connection(&self) -> Result<PoolConnection<Postgres>, StoreError> {
        self.pool
            .acquire()
            .await
            .map_err(|error| store_error(&error))
    }

    /// Begins the transaction every write of this adapter runs inside.
    pub(crate) async fn transaction(&self) -> Result<Transaction<'_, Postgres>, StoreError> {
        self.pool.begin().await.map_err(|error| store_error(&error))
    }
}

/// Returns `true` when a `CREATE SCHEMA IF NOT EXISTS` lost a race with another
/// process running the same statement.
fn created_concurrently(error: &sqlx::Error) -> bool {
    /// `duplicate_schema`.
    const DUPLICATE_SCHEMA: &str = "42P06";
    /// The unique violation on `pg_namespace` a tighter race raises instead.
    const UNIQUE_VIOLATION: &str = "23505";

    error
        .as_database_error()
        .and_then(|database| database.code())
        .is_some_and(|code| code == DUPLICATE_SCHEMA || code == UNIQUE_VIOLATION)
}

/// Commits a transaction, classifying a commit that does not answer as
/// [`StoreError::Timeout`].
pub(crate) async fn commit(transaction: Transaction<'_, Postgres>) -> Result<(), StoreError> {
    transaction
        .commit()
        .await
        .map_err(|error| commit_failed(&error))
}

impl fmt::Debug for PgStores {
    /// Names the schema and the pool's size, never the connection string: this
    /// output is safe to log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgStores")
            .field("schema", &self.schema.as_deref().unwrap_or("<default>"))
            .field("connections", &self.pool.size())
            .field("closed", &self.pool.is_closed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_migrations_are_embedded() {
        assert_eq!(MIGRATOR.iter().len(), 2, "both migrations are embedded");
        assert!(
            MIGRATOR
                .iter()
                .all(|migration| !migration.sql.trim().is_empty())
        );
    }

    #[tokio::test]
    async fn a_schema_name_is_validated_before_it_reaches_ddl() {
        let store = PgStores::from_pool(PgPool::connect_lazy("postgres://x/y").unwrap());
        assert!(store.clone().with_schema("tf_test").is_ok());
        assert!(store.with_schema("tf\";DROP SCHEMA public").is_err());
    }

    #[tokio::test]
    async fn debug_output_carries_no_connection_string() {
        let store =
            PgStores::from_pool(PgPool::connect_lazy("postgres://secret:hunter2@host/db").unwrap());
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("<default>"), "{rendered}");
    }
}
