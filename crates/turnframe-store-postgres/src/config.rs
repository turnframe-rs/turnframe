//! Pool settings, the optional dedicated schema, and the statement-timeout
//! advisory.

use std::fmt;
use std::time::Duration;

/// How a [`PgStores`](crate::PgStores) opens and shapes its connection pool.
///
/// The defaults are meant for a service that handles turns: a pool small enough
/// that PostgreSQL is not the thing that falls over first, an acquire timeout
/// short enough that a saturated pool surfaces as
/// [`Unavailable`](turnframe_store::error::StoreError::Unavailable) instead of a
/// hung request, and connections recycled often enough that a rolling database
/// upgrade drains cleanly.
///
/// ```rust
/// use std::time::Duration;
///
/// use turnframe_store_postgres::PgStoreConfig;
///
/// # fn main() -> Result<(), turnframe_store_postgres::ConfigError> {
/// let config = PgStoreConfig::new()
///     .max_connections(16)
///     .statement_timeout(Duration::from_secs(5))
///     .schema("turnframe")?;
/// assert_eq!(config.schema_name(), Some("turnframe"));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgStoreConfig {
    max_connections: u32,
    min_connections: u32,
    acquire_timeout: Duration,
    idle_timeout: Option<Duration>,
    max_lifetime: Option<Duration>,
    statement_timeout: Option<Duration>,
    schema: Option<String>,
}

impl PgStoreConfig {
    /// Upper bound on pooled connections. PostgreSQL serves every connection
    /// with a backend process, so this is a budget shared with every other
    /// service on the same cluster.
    pub const DEFAULT_MAX_CONNECTIONS: u32 = 10;
    /// Connections kept open while idle, so a quiet period does not make the
    /// next turn pay for a handshake.
    pub const DEFAULT_MIN_CONNECTIONS: u32 = 1;
    /// How long a caller waits for a connection before the store reports
    /// `Unavailable`.
    pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
    /// How long an unused connection is kept.
    pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
    /// How long any connection is kept, however busy. Recycling bounds the
    /// damage of a leaked server-side state and lets a rolling upgrade drain.
    pub const DEFAULT_MAX_LIFETIME: Duration = Duration::from_secs(1800);

    /// The defaults described above, on the `public` schema, with no statement
    /// timeout of the adapter's own.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_connections: Self::DEFAULT_MAX_CONNECTIONS,
            min_connections: Self::DEFAULT_MIN_CONNECTIONS,
            acquire_timeout: Self::DEFAULT_ACQUIRE_TIMEOUT,
            idle_timeout: Some(Self::DEFAULT_IDLE_TIMEOUT),
            max_lifetime: Some(Self::DEFAULT_MAX_LIFETIME),
            statement_timeout: None,
            schema: None,
        }
    }

    /// Sets the maximum number of pooled connections.
    #[must_use]
    pub fn max_connections(mut self, connections: u32) -> Self {
        self.max_connections = connections;
        self
    }

    /// Sets the number of connections kept open while idle.
    #[must_use]
    pub fn min_connections(mut self, connections: u32) -> Self {
        self.min_connections = connections;
        self
    }

    /// Sets how long a caller waits for a connection from the pool.
    ///
    /// This is *not* a statement timeout: it bounds the wait for a connection,
    /// not the work done once one is held. See [`Self::statement_timeout`].
    #[must_use]
    pub fn acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Sets how long an unused connection is kept; `None` keeps it forever.
    #[must_use]
    pub fn idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Sets how long any connection is kept; `None` keeps it forever.
    #[must_use]
    pub fn max_lifetime(mut self, lifetime: Option<Duration>) -> Self {
        self.max_lifetime = lifetime;
        self
    }

    /// Sets `statement_timeout` on every connection this pool opens.
    ///
    /// # The advisory
    ///
    /// A statement timeout is the only thing that bounds a query the database
    /// decides to run slowly, and without one a single lock wait can hold a
    /// pooled connection until the pool is empty and every turn fails. Set one.
    ///
    /// Set it with these three consequences in mind.
    ///
    /// *It cancels statements, not transactions.* PostgreSQL raises
    /// `query_canceled` on the statement that ran too long; this adapter reports
    /// [`Timeout`](turnframe_store::error::StoreError::Timeout) and abandons the
    /// transaction, so a cancelled statement inside
    /// [`CommitStore::commit`](turnframe_store::commit::CommitStore::commit)
    /// discards the whole bundle. That is the correct outcome — a partially
    /// written bundle is an invariant violation — but it means the timeout must
    /// be generous enough for the largest bundle a turn produces, not for the
    /// median statement.
    ///
    /// *`Timeout` is not a signal to retry.* The contract reads it as "the write
    /// may have landed": the caller re-reads and resumes by idempotency key. A
    /// timeout set so tight that healthy commits trip it turns every one of them
    /// into a recovery.
    ///
    /// *The outbox claim is the exception to keep short.*
    /// [`claim_due`](turnframe_store::outbox::OutboxWriter::claim_due) takes row
    /// locks with `SKIP LOCKED`, so it never waits on another dispatcher; if it
    /// is slow, something else is wrong and cutting it off is right.
    ///
    /// A few seconds suits a service handling turns. Leave it `None` to inherit
    /// whatever the role or the server sets, which is the better choice when the
    /// database is administered separately.
    #[must_use]
    pub fn statement_timeout(mut self, timeout: Duration) -> Self {
        self.statement_timeout = Some(timeout);
        self
    }

    /// Clears the adapter's statement timeout, inheriting the server's.
    #[must_use]
    pub fn inherit_statement_timeout(mut self) -> Self {
        self.statement_timeout = None;
        self
    }

    /// Puts every table in a dedicated schema instead of the connection's
    /// default `search_path`.
    ///
    /// [`PgStores::migrate`](crate::PgStores::migrate) creates the schema when
    /// it is missing, so pointing a fresh deployment at an empty database is one
    /// call. Tests use it to give each run a private namespace inside one
    /// database.
    ///
    /// # Errors
    /// * [`ConfigError::InvalidSchemaName`] when the name is not a plain
    ///   unquoted PostgreSQL identifier. The name is interpolated into `CREATE
    ///   SCHEMA` and into `search_path`, neither of which can take a bind
    ///   parameter, so it is validated here rather than escaped later.
    pub fn schema(mut self, schema: impl Into<String>) -> Result<Self, ConfigError> {
        let schema = schema.into();
        if !is_plain_identifier(&schema) {
            return Err(ConfigError::InvalidSchemaName);
        }
        self.schema = Some(schema);
        Ok(self)
    }

    /// The configured schema, if any.
    #[must_use]
    pub fn schema_name(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// The configured statement timeout, if any.
    #[must_use]
    pub fn statement_timeout_value(&self) -> Option<Duration> {
        self.statement_timeout
    }

    /// The configured pool bounds, as `(min, max)`.
    #[must_use]
    pub fn connection_bounds(&self) -> (u32, u32) {
        (self.min_connections, self.max_connections)
    }

    /// Applies the pool settings to a `sqlx` pool builder.
    pub(crate) fn apply_pool(
        &self,
        options: sqlx::postgres::PgPoolOptions,
    ) -> sqlx::postgres::PgPoolOptions {
        options
            .max_connections(self.max_connections)
            .min_connections(self.min_connections)
            .acquire_timeout(self.acquire_timeout)
            .idle_timeout(self.idle_timeout)
            .max_lifetime(self.max_lifetime)
    }

    /// Applies the per-connection settings to `sqlx` connect options.
    pub(crate) fn apply_connection(
        &self,
        mut options: sqlx::postgres::PgConnectOptions,
    ) -> sqlx::postgres::PgConnectOptions {
        if let Some(schema) = &self.schema {
            options = options.options([("search_path", schema.as_str())]);
        }
        if let Some(timeout) = self.statement_timeout {
            let millis = timeout.as_millis().to_string();
            options = options.options([("statement_timeout", millis.as_str())]);
        }
        options
    }
}

impl Default for PgStoreConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a [`PgStoreConfig`] was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The schema name is not a plain unquoted PostgreSQL identifier: it must
    /// start with a letter or an underscore, continue with letters, digits or
    /// underscores, and fit in 63 bytes.
    #[error("schema name is not a plain unquoted PostgreSQL identifier")]
    InvalidSchemaName,
}

/// Returns `true` for a name that needs neither quoting nor escaping.
fn is_plain_identifier(name: &str) -> bool {
    /// PostgreSQL truncates identifiers past `NAMEDATALEN - 1`.
    const MAX_IDENTIFIER_BYTES: usize = 63;

    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    name.len() <= MAX_IDENTIFIER_BYTES
        && (first.is_ascii_lowercase() || first.is_ascii_uppercase() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

impl fmt::Display for PgStoreConfig {
    /// Names the settings, never a connection string: this value is safe to log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pool {}..{}, schema {}",
            self.min_connections,
            self.max_connections,
            self.schema.as_deref().unwrap_or("<default>")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let config = PgStoreConfig::new();
        assert_eq!(config.connection_bounds(), (1, 10));
        assert_eq!(config.schema_name(), None);
        assert_eq!(config.statement_timeout_value(), None);
        assert_eq!(config, PgStoreConfig::default());
    }

    #[test]
    fn a_schema_name_must_be_a_plain_identifier() {
        assert!(PgStoreConfig::new().schema("turnframe").is_ok());
        assert!(PgStoreConfig::new().schema("_tf_test_1").is_ok());
        for rejected in [
            "",
            "1leading_digit",
            "has space",
            "quote\"injection",
            "semicolon;drop",
            "dotted.name",
            "unicodé",
        ] {
            assert_eq!(
                PgStoreConfig::new().schema(rejected).unwrap_err(),
                ConfigError::InvalidSchemaName,
                "{rejected:?} must be refused"
            );
        }
        let too_long = "a".repeat(64);
        assert_eq!(
            PgStoreConfig::new().schema(too_long).unwrap_err(),
            ConfigError::InvalidSchemaName
        );
    }

    #[test]
    fn display_never_carries_a_connection_string() {
        let config = PgStoreConfig::new().schema("turnframe").unwrap();
        assert_eq!(config.to_string(), "pool 1..10, schema turnframe");
        assert_eq!(
            PgStoreConfig::new().to_string(),
            "pool 1..10, schema <default>"
        );
    }

    #[test]
    fn timeouts_are_settable_and_clearable() {
        let config = PgStoreConfig::new()
            .statement_timeout(Duration::from_secs(3))
            .acquire_timeout(Duration::from_secs(1))
            .idle_timeout(None)
            .max_lifetime(None)
            .min_connections(2)
            .max_connections(4);
        assert_eq!(
            config.statement_timeout_value(),
            Some(Duration::from_secs(3))
        );
        assert_eq!(config.connection_bounds(), (2, 4));
        assert_eq!(
            config.inherit_statement_timeout().statement_timeout_value(),
            None
        );
    }
}
