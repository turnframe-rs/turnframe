# The persistence contract

`turnframe-store` declares what must be durable and with which rules, as seven
object-safe traits. This page holds the rules; the crate documentation holds the
map.

## Five rules that hold for every method of every trait

An implementation that breaks one of them is wrong even when its own tests pass.

1. **Account scoping is total.** Every lookup takes an `AccountId` and answers
   only about that tenant. A record that exists for another account must be
   reported exactly like one that does not exist at all: `StoreError::NotFound`,
   with no difference in timing, message or error code (§25.4). The outbox is the
   one deliberate exception: it is a system-owned dispatch queue addressed by
   `OutboxId`, never by user input. So are the two cross-tenant sweeps,
   `expire_due` and `list_unfinished_turns`, which are operator jobs and still
   stamp every row they return with its account.
2. **The error surface is closed.** A store speaks `StoreError` and nothing else,
   so the runtime can classify a failure without knowing the backend. `Conflict`
   means a uniqueness or compare-and-swap precondition failed and nothing was
   written; `Timeout` means the write *may* have landed and the caller must
   re-read rather than retry.
3. **Compare-and-swap, never blind overwrite.** Every status change names the
   state it expects. Repeating a settled transition with the same data is
   accepted, because recovery replays it; settling it differently is `Conflict`.
   That is what makes crash recovery safe to run more than once.
4. **Nothing is rewritten.** Interaction payloads, journal commands and committed
   events are immutable once written; only lifecycle columns move. An assistant
   turn is stored as the exact `AssistantTurn` that was returned and must reload
   with identical, identically ordered blocks: a reload never rebuilds a card or
   a receipt from free text (§22.3).
5. **Ordering is specified, not incidental.** Wherever a method returns a list,
   its documentation names the sort key. Two implementations given the same
   writes must return the same order.

## The atomicity model

A turn produces several writes that are meaningless apart: the journal outcome of
each command, the committed events, the resolution of the card that authorized
them, the cards the new state requires, the cards the new revision invalidates,
outbox rows, the replay record and the phase marker. A journal entry reading
`Committed` with no events would let a receipt be rendered from nothing; events
with no journal outcome would let recovery run the command twice.

So they do not travel one by one. `CommitBundle` carries all of them and
`CommitStore::commit` applies it all or nothing, in the normative order the
`commit` module documents. Each item obeys exactly the rules it would obey
through its own trait, so a bundle whose last item is illegal writes nothing at
all, and nothing includes the records the bundle would have *changed*, which
read back exactly as they were.

What is deliberately not in that transaction is the domain executor's own state
commit, which may live in a different database. Turnframe does not attempt a
distributed transaction. Safety across that seam comes from the journal: the
entry is admitted *before* execution, the executor is idempotent on the
idempotency key, and recovery resumes pending entries by that key (§16.2,
§23.1). Adopters whose domain tables sit in the same database may enlist the
executor in the same transaction; the contract does not require it.

## Bringing your own store

Implement the seven traits over your backend, put them in a `Stores`, and run
`conformance::run_all` against a factory that builds an empty instance. The suite
exercises the rules above (tenant isolation, the blocking-interaction slot,
compare-and-swap resolution, journal idempotency, event ordering, exactly-once
paging of the event stream, outbox claim exclusivity, turn round-tripping and
bundle atomicity) and returns a `ConformanceReport` instead of panicking, so it
can run inside a test, a health check or a migration gate. Every check is also
callable on its own while the adapter is being built up.

`MemoryStores` is the reference implementation: one shared state behind a
`std::sync::Mutex` that is never held across an await, deterministic iteration
order, an injectable `Clock` so expiry is testable without sleeping, a
`FailurePoint` hook that reproduces the chaos boundaries of §27.7, and a commit
that is atomic without copying the state.

## Two reads of the event journal, and choosing wrong is a bug

The journal answers two different questions and they need two different cursors.

`list_since` pages by **case revision**, and a revision is not a position: one
command commits several events at the same revision. Ask for three events of a
revision that produced five and you get three, with no way to say "continue after
the third": the next call can only name a revision, so it either re-reads the
whole revision or skips the two events it never saw. That is fine for what the
method is for (show me what happened to this case since the revision I hold, with
a sanity cap on the answer) and wrong for anything that must see every event once.

`read_from` pages by the store-assigned sequence, which *is* a position: every
event has its own, they never collide, and they never move. A consumer keeps the
cursor from the last page and the next call resumes exactly after the last event
it was handed, including in the middle of a revision, and including when the
journal grew between the two calls. That is the read for a projector, a
dispatcher, an exporter or any at-least-once consumer with an offset of its own.

Both are account-scoped, so a cursor never carries a consumer across tenants.

## Erasing personal data from an append-only ledger

Two rules compose into a consequence nobody chose. The ledger is append-only
because committed events are the only thing authorizing an operational claim
(I16), and a case may not be deleted because its identity outlives its content. A
domain that manages people therefore puts names, document numbers and addresses
into event payloads that have no exit, while an adopter in the European Union is
obliged to erase them on request.

`redact_payload` is the exit, and it is deliberately not a delete. The claim guard
needs an event's identity, its type, its ordering and its status code; it does not
need the payload. So the payload is emptied **in place** and everything else
stays: a receipt rendered before the erasure is still backed by the events it
cited, a receipt rendered after it renders from a redacted event and the domain
decides what that reads like, and no consumer's cursor shifts. That is
append-only in the sense that carries the guarantee.

A store meeting the same obligation by deleting the row would break it silently:
every claim resting on that event becomes unverifiable, every at-least-once
consumer skips a position it will never be handed again, and nothing fails until
someone asks whether an assistant's statement was true. The conformance check
`check_event_redaction_preserves_identity_and_order` exists to make that a test
failure instead.

**Design payloads that need this less.** An event payload carrying a *reference*
rather than a value (a traveler id instead of a name, an attachment id instead
of the text read out of it) is easier to erase, because erasing the record it
points at empties the payload without touching the ledger at all. Prefer it where
the domain allows. The honest caveat is that it does not generalise: a receipt
says *what changed*, and "the registered name is now Aurora" cannot be rendered from
an identifier. Where the receipt needs the value, the value is in the payload, and
this path is why that is survivable.
