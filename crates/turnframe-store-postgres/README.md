# turnframe-store-postgres

The PostgreSQL implementation of the [Turnframe](https://github.com/turnframe-rs/turnframe)
persistence contract: seven store traits, one commit store, a migration set, and the whole
`turnframe-store` conformance suite run against a live PostgreSQL 16 as its proof.

```toml
[dependencies]
turnframe-store-postgres = "0.1"
```

## This crate is optional

The contract is `turnframe-store`, not this crate. That crate defines *what must be durable and
under which rules* as seven object-safe traits (six of them split into a `…Reader` half and a
`…Writer` half, with the familiar name as the aggregate of the two) and ships an executable
conformance suite that proves an implementation right through the public API alone. Anything that passes the suite is a
valid store: your existing schema, another database, a document store, a service.

This crate is one such implementation. Depend on it if you want a schema that has already been
argued about; skip it entirely if you have your own. The runtime cannot tell which one it is
holding, and neither can the conformance suite.

## Pointing it at a database

```rust,no_run
use std::time::Duration;

use turnframe_store::prelude::*;
use turnframe_store_postgres::{PgStoreConfig, PgStores};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let config = PgStoreConfig::new()
    .max_connections(16)
    .statement_timeout(Duration::from_secs(5))
    .schema("turnframe")?;

let store = PgStores::connect_with("postgres://turnframe@localhost/turnframe", &config).await?;
store.migrate().await?;

let stores: Stores = store.stores()?;
# let _ = stores;
# Ok(())
# }
```

`PgStores::connect(url)` takes the defaults. `PgStores::from_pool(pool)` takes a pool you already
have, which is what to use when the application and the store should share connections; see
[enlisting your own writes](#enlisting-your-own-writes).

The defaults are meant for a service handling turns: at most 10 connections, one kept warm, a
five-second wait for one before the store reports `Unavailable`, connections recycled after ten
idle minutes or thirty minutes of life so a rolling database upgrade drains cleanly.

### The statement-timeout advisory

`PgStoreConfig::statement_timeout` sets `statement_timeout` on every connection the pool opens, and
you should set it. Without one, a single lock wait can hold a pooled connection until the pool is
empty and every turn fails. Three consequences shape the value:

- **It cancels statements, not transactions.** PostgreSQL raises `query_canceled`, this adapter
  reports `StoreError::Timeout`, and the transaction is abandoned, so a cancelled statement inside
  `commit` discards the whole bundle. That is correct (a half-written bundle is an invariant
  violation), but the timeout has to be generous enough for the largest bundle a turn produces, not
  for the median statement.
- **`Timeout` is not a signal to retry.** The contract reads it as "the write may have landed": the
  caller re-reads and resumes by idempotency key. A timeout tight enough to trip healthy commits
  turns every one of them into a recovery.
- **The outbox claim is the one to keep short.** `claim_due` takes its rows with `SKIP LOCKED` and
  never waits on another dispatcher, so if it is slow something else is wrong.

A few seconds suits a service handling turns. Leaving it unset inherits whatever the role or the
server sets, which is the better choice when the database is administered separately.
`acquire_timeout` is *not* a statement timeout: it bounds the wait for a connection, not the work
done while holding one.

## Running the migrations

The SQL lives in `migrations/` and is embedded in your binary at compile time, so a deployment
needs no directory beside the executable:

```rust,no_run
use turnframe_store_postgres::PgStores;

# async fn example(store: &PgStores) -> Result<(), Box<dyn std::error::Error>> {
store.migrate().await?;
# Ok(())
# }
```

It is safe on every start-up. `sqlx` records applied versions and skips them, the statements are
`IF NOT EXISTS` (and the one constraint that has no such form swallows its own duplicate) so
applying them to a database that already carries the schema is a no-op, and two processes racing the
call are serialised by a database-wide advisory lock. When a schema is configured, `migrate()`
creates it first, including when several instances create it at once.

There are two versions. `20260101000000` creates the schema; `20260102000000` adds `redacted_at` and
`redaction_authority` to `tf_domain_event`, which is where an erased payload's audit trail lives (see
[Erasing a payload](#erasing-a-payload-from-an-append-only-ledger)).

`turnframe_store_postgres::migrate(&pool)` does the same from a pool you own, for a deployment that
migrates from a separate binary. It does not create a schema.

**Rollback.** There are no down-migrations, and no migration drops or rewrites anything: rolling the
application back to an earlier release leaves the schema usable as it is, because the columns the
older code does not know about are nullable. To remove it, drop the dedicated schema, or drop the
tables and the recorded versions:

```sql
DROP SCHEMA turnframe CASCADE;
-- or, in a shared schema:
DROP TABLE IF EXISTS tf_replay, tf_outbox, tf_domain_event, tf_command_journal,
                     tf_interaction, tf_turn_phase, tf_turn, tf_conversation;
DELETE FROM _sqlx_migrations WHERE version IN (20260101000000, 20260102000000);
```

## Erasing a payload from an append-only ledger

The ledger may not lose an event (a receipt cites committed events, and a consumer pages it by a
sequence that must never skip), and a payload carrying personal data must still be erasable on
request. `EventJournalWriter::redact_payload` is the one `UPDATE` this adapter runs against
`tf_domain_event`:

```sql
UPDATE tf_domain_event
   SET payload = 'null'::jsonb,
       redacted_at = COALESCE(redacted_at, now()),
       redaction_authority = COALESCE(redaction_authority, $3)
 WHERE account_id = $1 AND event_id = $2
RETURNING redacted_at, redaction_authority;
```

Three things are worth reading twice. The statement writes three columns and no others, so the
identity column, the case columns and `occurred_at` are untouched by the statement rather than by a
promise: nothing here can move an event. The `COALESCE` pair makes a retried erasure request a no-op
that keeps the **first** record, because who erased what is not something a retry may rewrite. And
the `WHERE` clause is account-scoped, so an identifier belonging to another tenant updates nothing
and is reported as `NotFound`, exactly like one that never existed.

The audit trail lives on the redacted row rather than in a table beside it, so one statement performs
the erasure and records it, and a redacted payload with no record of who removed it is not a state
this schema can hold: a `CHECK` constraint keeps the two columns from disagreeing. Neither column
says *what* was removed, which is the point.

## The schema, and why each constraint is there

Eight tables, all prefixed `tf_`. Every timestamp is `timestamptz`, every payload is `jsonb`, every
revision is `CHECK (… >= 0)`, and `account_id` leads every primary key and every index, so a query
that forgets the tenant cannot use an index, which is a cheap way to notice one.

| Table | Holds |
|---|---|
| `tf_conversation` | conversations |
| `tf_turn` | the user turn as received and the assistant turn exactly as returned |
| `tf_turn_phase` | the crash-recovery phase marker of each turn |
| `tf_interaction` | cards: the immutable payload plus the lifecycle this store owns |
| `tf_command_journal` | idempotency admission and the persisted outcome of every command |
| `tf_domain_event` | the append-only claim ledger |
| `tf_outbox` | external side effects awaiting dispatch |
| `tf_replay` | one replay record per turn |

### The constraints that carry the rules

**`tf_one_open_blocking_interaction_per_case`**: a partial unique index on
`(account_id, workflow_key, case_id) WHERE blocking AND status IN ('active','resolving')`. This is
invariant I5: at most one open blocking card per case. It is an index rather than a check in the
adapter so that two concurrent inserts do not both read "the slot is free" and both write; one of
them loses on the index and is refused with `Conflict` having written nothing. It is *partial*
because a card that has been resolved, expired or invalidated no longer holds the slot, and a
non-blocking card never held it.

**`UNIQUE (account_id, idempotency_key)` on `tf_command_journal`**: invariant I14, the rule that
stops a retried request from charging a traveler twice. Admission is
`INSERT … ON CONFLICT DO NOTHING` followed by a re-read: two callers arriving with the same key at
the same instant do not both see "absent", because the second one's insert waits on the first one's
speculative row and then does nothing. There is exactly one `Fresh` per key, ever, and every repeat
carries the outcome the first attempt persisted.

**`UNIQUE (destination, idempotency_key)` on `tf_outbox`**: a given external action exists at most
once per destination, so a bundle that is applied twice cannot enqueue the same call twice.

**`sequence bigint GENERATED ALWAYS AS IDENTITY` on `tf_domain_event`**: the store assigns the
position, not the caller, so readback order is append order. A batch is inserted with
`WITH ORDINALITY … ORDER BY`, which fixes the order rows are inserted in, and the primary key
`(account_id, event_id)` makes a duplicate (whether it collides with a stored event or with another
event of the same batch) fail the statement whole. A receipt can never be backed by half a commit.

**`CHECK (case_revision >= 0)`, `CHECK (expected_revision >= 0)`, `CHECK (attempt_count >= 0)`**:
a revision is a `u64` in Rust and a `bigint` in PostgreSQL. The check is what stops a writer that is
not this adapter from putting a value in that the adapter would then read as corrupt.

**Composite primary keys `(account_id, …)`**: the account is part of the identity, not a column
beside it. A card, an event or a turn of another tenant is not "found and then filtered": it is a
different row entirely, and `NotFound` for a foreign identifier is the same answer, from the same
index, as `NotFound` for one that never existed.

**`tf_outbox` has no `account_id`, on purpose.** It is a system-owned dispatch queue addressed by
`outbox_id`, never by user input, and it carries `command_id` so a row traces back to the
account-scoped journal entry that produced it. This is the one exception the persistence contract
names, and it is why nothing in `OutboxReader` or `OutboxWriter` takes an account.

**Foreign keys `tf_turn → tf_conversation` and `tf_turn_phase → tf_turn`**: a turn cannot exist
without its conversation and a marker cannot exist without its turn, so recovery never finds a
marker it cannot resolve. Both cascade on delete, which is what makes an erasure request one
statement.

### Compare-and-swap, in the statement that writes

Every status change puts the state the caller expected into the `WHERE` clause of the write:

```sql
UPDATE tf_interaction
   SET status = 'resolving', resolved_option_id = $3, resolved_at = $4, resolved_by_turn = $5
 WHERE account_id = $1 AND interaction_id = $2 AND status = 'active'
RETURNING …;
```

Zero affected rows means the precondition failed, and a second, read-only statement then says
whether that was because the row does not exist for this tenant (`NotFound`) or because it had
moved on (`Conflict`). The same shape carries the journal transition table, the phase marker's
refusal to leave a terminal phase, every outbox transition, and the revision invalidation, where
it is what makes two commits moving the same case agree on which of them retired a card.

The adapter assumes `READ COMMITTED`, PostgreSQL's default: the idempotency admission and these
retries rely on a statement seeing what another transaction has just committed.

### One transaction per write

Every method that writes runs inside a transaction, including the ones that look like a single
statement, and `CommitStore::commit` puts the whole bundle in one. An item that fails returns its
error, the transaction is dropped, PostgreSQL rolls it back, and nothing the bundle carried was ever
visible to another connection.

Because of that, a transport failure means nothing was written and is reported as `Unavailable`.
The exception is the `COMMIT` itself, where a connection that stops answering is genuinely
indeterminate: that is reported as `Timeout`, which the contract reads as "re-read, do not retry".

### Enlisting your own writes

The workflow executor's own state commit is deliberately *outside* the bundle's transaction:
Turnframe does not attempt a distributed transaction, and safety across that seam comes from the
journal instead. When the domain tables do live in the same database, `PgStores::commit_in` closes
the gap:

```rust,no_run
use turnframe_core::ids::AccountId;
use turnframe_store::commit::CommitBundle;
use turnframe_store_postgres::PgStores;

# async fn example(store: &PgStores, bundle: CommitBundle) -> Result<(), Box<dyn std::error::Error>> {
let mut transaction = store.pool().begin().await?;
// ... your own writes, on the same transaction ...
store.commit_in(&mut transaction, &AccountId::from("aurora"), bundle).await?;
transaction.commit().await?;
# Ok(())
# }
```

## Running the conformance suite against it

The suite is the proof this crate offers. It exercises the persistence contract (tenant isolation,
the blocking-card slot, compare-and-swap resolution, journal idempotency, event ordering and cursor
paging, outbox claim exclusivity, assistant-turn round-tripping and bundle atomicity) through the
public traits only.

```sh
export TURNFRAME_TEST_DATABASE_URL=postgres://turnframe:turnframe@localhost:5432/turnframe_test
cargo test -p turnframe-store-postgres
```

Without that variable every database test prints a `SKIPPED` note and passes, so the suite is green
on a machine with no PostgreSQL and proves something on a machine with one. The continuous
integration workflow sets it against a `postgres:16` service.

In your own project, hand `conformance::run_all` a factory that returns an **empty** set of stores
(a fresh schema, a fresh database, or a truncation):

```rust,no_run
use turnframe_store::conformance;
use turnframe_store::stores::Stores;
use turnframe_store_postgres::PgStores;

# async fn example(store: PgStores) -> Result<(), Box<dyn std::error::Error>> {
let factory = move || -> Stores {
    // empty the schema here, then:
    store.stores().expect("every role is supplied")
};
let report = conformance::run_all(&factory).await;
assert!(report.passed(), "{report}");
# Ok(())
# }
```

Beside it are the tests the in-memory store cannot write: two transactions racing the same expected
revision where exactly one wins, two workers claiming from the outbox where no row is handed out
twice, the partial unique index refusing a second blocking card when the insert goes around the
adapter entirely, and a bundle held open then rolled back, watched from a second connection.

## Runtime-checked queries, on purpose

Every statement goes through `sqlx::query` and reads its columns by name. None of them uses the
`sqlx::query!` family.

Those macros check SQL against a live database at compile time, which is a real benefit and the
wrong trade for a published library: the crate would be unbuildable in a clean checkout unless a
database is reachable or a `.sqlx` cache is committed and kept in step with every edit. A
contributor with no PostgreSQL, a `cargo install`, a `docs.rs` build and a downstream `cargo vendor`
would all fail on something unrelated to their change. The check the macros would have given is
bought back by the conformance suite: a column renamed on one side and not the other fails a test
rather than a build. (`sqlx::migrate!` is still a macro and still used: it reads `migrations/`
while compiling and needs no database.)

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
