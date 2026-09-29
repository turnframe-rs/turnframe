# turnframe-store

The persistence contract of [Turnframe](https://github.com/turnframe-rs/turnframe): seven
object-safe traits that say **what must be durable and under which rules**, a deterministic
in-memory implementation of all of them, and an executable conformance suite that proves any
implementation right.

There is no SQL and no database driver here. `turnframe-store-postgres` is one implementation of
these traits and it is optional: the traits are specified well enough to be implemented from this
document and the rustdoc alone, over whatever database you already run.

```toml
[dependencies]
turnframe-store = "0.1"
```

## Scope

| Trait | Owns |
|---|---|
| `ConversationStore` | conversations, user turns, assistant turns *as returned*, and the crash-recovery phase marker |
| `InteractionStore` | persisted cards: immutable payloads, the one-blocking-per-case slot, compare-and-swap resolution, revision invalidation, expiry |
| `CommandJournal` | idempotency admission under `UNIQUE (account_id, idempotency_key)` and the persisted outcome of every command |
| `EventJournal` | the append-only claim ledger (the only thing an operational receipt may cite), read as one case's history or paged exactly once as a stream |
| `OutboxStore` | external side effects awaiting dispatch, with claim, reschedule and reap semantics |
| `ReplayStore` | one replay record per turn, upserted as the turn advances |
| `CommitStore` | the all-or-nothing write of everything one commit produces |

`Stores` bundles one implementation of each behind `Arc<dyn …>` so the runtime carries a single
value. `MemoryStores` implements all seven over one shared state and is what tests and examples use.

### Read half and write half

Six of the seven traits are split in two. `ConversationReader` declares the methods that answer
questions and `ConversationWriter` the methods that change something; `ConversationStore` is the
aggregate of the two and comes free from a blanket implementation. The same holds for
`InteractionStore`, `CommandJournal`, `EventJournal`, `OutboxStore` and `ReplayStore`. `CommitStore`
is not split, because a commit *is* the write.

There is nothing to implement on an aggregate trait: write the two halves and
`Arc<dyn ConversationStore>` keeps naming one value that does everything. The split exists so that a
caller which must not write can be *given* only the read half: `Stores::read_only()` returns a
`ReadOnlyStores` with no write method and no way back to one, which is what the plan-only turn path
of `turnframe-runtime` holds. A guarantee the compiler enforces survives a refactoring; one a
reviewer enforces does not.

```rust
use turnframe_core::ids::{AccountId, ConversationId};
use turnframe_store::prelude::*;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
# tokio::runtime::Runtime::new()?.block_on(async {
let stores = Stores::in_memory();
let account = AccountId::from("aurora");
let conversation = ConversationId::new();

stores
    .conversations()
    .create_conversation(ConversationRecord::new(
        conversation,
        account.clone(),
        chrono::Utc::now(),
    ))
    .await?;

// Another tenant cannot tell it apart from a conversation that never existed.
assert_eq!(
    stores
        .conversations()
        .load_conversation(&AccountId::from("other"), &conversation)
        .await,
    Err(StoreError::NotFound)
);
# Ok::<(), StoreError>(())
# })?;
# Ok(())
# }
```

## Bring your own store

### 1. The invariants you are signing up to

Five rules hold for every method of every trait. An implementation that breaks one is wrong even
when its own tests pass.

1. **Account scoping is total.** Every lookup takes an `AccountId` and answers only about that
   tenant. A record belonging to another account must be reported exactly like one that never
   existed: `StoreError::NotFound`, with no difference in message, code or timing. The outbox is the
   one deliberate exception (it is a system-owned dispatch queue addressed by `OutboxId`, never by
   user input), and so are the two operator sweeps, `expire_due` and `list_unfinished_turns`, which
   still stamp every row they return with its account.
2. **The error surface is closed.** Speak `StoreError` and nothing else. `Conflict` means a
   uniqueness or compare-and-swap precondition failed *and nothing was written*. `Timeout` means the
   write may or may not have landed and the caller must re-read rather than retry. `Other { code }`
   carries a stable code; the three this crate defines live in `error::codes`.
3. **Compare-and-swap, never blind overwrite.** Every status change names the state it expects.
   Repeating a settled transition with the *same* data is accepted; settling it differently is
   `Conflict`. That is what makes crash recovery safe to run more than once.
4. **Nothing is rewritten.** Interaction payloads, journal commands and committed events are
   immutable once written; only lifecycle columns move. An assistant turn is stored as the exact
   `AssistantTurn` that was returned and must reload with identical, identically ordered blocks: a
   reload never rebuilds a card or a receipt from free text.
   The one exception is `EventJournalWriter::redact_payload`, and it is an exception on purpose:
   personal data reaches event payloads because receipts say what changed, and an adopter in the
   European Union has to be able to erase it. It empties a payload **in place**: identity, type,
   sequence, revision and timestamps all survive, so nothing appears, disappears or moves, every
   receipt that cited the event is still backed, and no consumer's cursor skips a position. Meeting
   the same obligation by deleting the row breaks all of that silently, which is why the conformance
   suite checks for it rather than trusting the sentence you are reading.
5. **Ordering is specified.** Wherever a method returns a list, its rustdoc names the sort key. Two
   implementations given the same writes return the same order.

### 2. Write the adapter

Implement the six reader traits, the six writer traits and `CommitStore`, then assemble them with
`Stores::builder()`, which refuses to build until every role is supplied. One type may implement all
of them, as `MemoryStores` and the PostgreSQL adapter both do; the aggregate traits then follow from
the blanket implementations and need no `impl` block of their own. Four conventions save work:

- `CommandJournalWriter::fail` has a default body that maps an `ExecutionError` through
  `JournalOutcome::from_execution_error` and calls `complete`. Leave it alone unless you can save a
  round trip.
- `CommitBundle::validate` performs the account and empty-batch checks every implementation owes.
  Call it first, before any write.
- Build the replay answer with `JournalAdmission::replay(entry)` rather than the `Replay` variant
  directly; the entry is boxed so the common `Fresh` answer stays small.
- The three refusals this contract defines have constructors (`error::identity_mismatch()`,
  `error::invalid_record()`, `error::bundle_account_mismatch()`), and `error::has_code()` to test
  for them. Use them instead of spelling out `StoreError::Other { code }`, so every adapter refuses
  identically.

The reference PostgreSQL shapes are in the spec (§22.2) and mirrored by the in-memory store: a
partial unique index for the blocking-interaction slot, `UNIQUE (account_id, idempotency_key)` on the
journal, `UNIQUE (destination, idempotency_key)` on the outbox, and a `bigserial` sequence on the
event journal.

### 3. Run the conformance suite

```rust
use turnframe_store::conformance;
use turnframe_store::stores::Stores;

# fn my_stores() -> Stores {
#     Stores::in_memory()
# }
# fn main() -> Result<(), Box<dyn std::error::Error>> {
# tokio::runtime::Runtime::new()?.block_on(async {
let report = conformance::run_all(&my_stores).await;
assert!(report.passed(), "{report}");
# });
# Ok(())
# }
```

The factory is called once per check, so it must hand back an **empty** set of stores each time:
a fresh schema, a fresh database, or a truncation. Nothing in the suite panics: every check returns
`Result<(), ConformanceFailure>` and `run_all` collects them into a report you can print, assert on,
or turn into a `Result` with `into_result()`. While the adapter is still being built, call the
checks one at a time; each is a public `check_*` function taking a `&Stores`.

Nineteen checks cover the one-blocking-card slot and its replacement semantics, cross-tenant
`NotFound`, compare-and-swap resolution, idempotent settlement, revision invalidation and revision
independence, expiry, journal idempotency (exactly one `Fresh` per key, ever), unfinished entries
after an interrupted turn, event append ordering and read-by-identifier, exactly-once paging of the
event stream while the journal grows, outbox claim exclusivity and rescheduling, replay upsert,
assistant-turn round-tripping with identical blocks, and the four bundle rules below.

## Reading the event journal

The journal answers two questions, with two cursors, and picking the wrong one is a correctness bug
rather than a preference.

`list_since(account, case_key, since_revision, limit)` is the **history of one case**: everything
that happened to it after the revision you hold. Its `limit` caps the answer, it is not a page
boundary: one command commits several events at the same revision, so a limit can cut a revision in
half and there is no way to say "continue after the third event of revision 4", because the next call
can only name a revision again.

`read_from(account, cursor, limit)` is the **stream**, for a projector, an exporter or anything else
that keeps an offset of its own. It pages by the store-assigned `sequence`, which is a position:
unique, immutable, and increasing across the whole store. Start at `EventCursor::START`, hand each
`EventPage`'s `next_cursor` back on the next call, and every event of the account arrives exactly
once, in commit order, however much the journal grew in between. An empty page means you have caught
up and leaves the cursor untouched, so you can persist it and resume days later.

```rust
use turnframe_core::ids::AccountId;
use turnframe_store::prelude::*;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
# tokio::runtime::Runtime::new()?.block_on(async {
let stores = Stores::in_memory();
let account = AccountId::from("aurora");
let mut cursor = EventCursor::START;
loop {
    let page = stores.events().read_from(&account, cursor, 100).await?;
    if page.is_empty() {
        break;
    }
    for event in &page.events {
        // hand the event to whatever is being built from the stream
        assert_eq!(event.account_id, account);
    }
    cursor = page.next_cursor;
}
# Ok::<(), StoreError>(())
# })?;
# Ok(())
# }
```

## The atomicity model

A turn produces several writes that are meaningless apart: the journal outcome of each command, the
committed events, the resolution of the card that authorized them, the cards the new state requires,
the cards the new revision invalidates, outbox rows, the replay record and the phase marker. A
journal entry reading `Committed` with no events would let a receipt be rendered from nothing;
events with no journal outcome would let recovery run the command twice.

So they do not travel one by one. `CommitBundle` carries all of them and `CommitStore::commit`
applies it **all or nothing**, in this normative order: later items depend on earlier ones, so it is
part of the contract, not an implementation detail:

1. journal completions
2. event batches
3. interaction finishes
4. case invalidations
5. interaction inserts
6. outbox entries
7. the replay record
8. the turn phase marker

Every item obeys exactly the rules it would obey through its own trait, so a bundle whose last item
is illegal writes nothing at all, and "nothing" covers what the bundle would have *changed*, not
only what it would have created: the journal entry it would have settled is still `Pending`, the card
it would have resolved is still `Resolving`, the replay record it would have overwritten is the one
that was there. In PostgreSQL that is one transaction. The in-memory store applies the items in place
while recording how to undo each of them, and replays those reversals if any item fails; it used to
copy the whole state per commit instead, which is correct but quadratic in the number of commits a
process makes (4 000 single-event commits: 5.4 s before, 6 ms after).

**What is deliberately outside the transaction** is the domain executor's own state commit, which may
live in a different database. Turnframe does not attempt a distributed transaction. Safety across
that seam comes from the journal: the entry is admitted *before* execution, the executor is
idempotent on the idempotency key, and recovery resumes pending entries by that key. If the process
dies between the executor's commit and the bundle, recovery finds the entry `Executing`, re-runs the
command, the executor recognises the key and returns the original outcome, and the bundle is written
then. Adopters whose domain tables live in the same database may enlist the executor in the same
transaction; the contract does not require it.

## Testing against failure

`MemoryStores` can be made to fail at the boundaries the spec's chaos tests care about (§27.7):
after interaction persistence, before the command-journal insert, after that insert but before the
domain commit, after the commit but before the event readback, before and after outbox dispatch, and
before response persistence.

```rust
use turnframe_store::error::StoreError;
use turnframe_store::memory::{FailurePoint, MemoryStores};

# fn main() -> Result<(), StoreError> {
let store = MemoryStores::new();
store.fail_next(FailurePoint::AfterJournalInsertBeforeCommit, StoreError::Timeout)?;
// The next `begin` writes its entry, then reports the timeout. The entry
// survives in `Pending`: exactly what `pending_for_turn` exists to find.
assert_eq!(
    store.armed_failures()?,
    vec![FailurePoint::AfterJournalInsertBeforeCommit]
);
# Ok(())
# }
```

Whether the write preceding the boundary survives is part of each point's meaning and is documented
per variant: a point named `After…` leaves it visible, which is the partial state recovery must
cope with. Inside `commit` the rule is different and has to be: a fired failure discards the whole
bundle. A `Clock` (`SystemClock` or `ManualClock`) supplies the timestamps the store owns, so expiry
and claim-reaping tests assert rather than sleep.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
