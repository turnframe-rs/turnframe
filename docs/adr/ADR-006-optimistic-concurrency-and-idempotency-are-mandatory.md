# ADR-006: Optimistic concurrency and idempotency are mandatory

- Status: Accepted (2026-09-05)

## Context

Turnframe runs conversational workflows in which a model proposes meaning and a deterministic reducer decides which commands run against a persisted case. Between the moment the runtime loads a case and the moment it commits a change, the world can move: the same user may send a second message from another tab, a button on an older card may be clicked twice, a background poller may update the same case, or a network retry may redeliver a request the server already handled. Two invariants from the master specification exist to make this window safe: I13 (optimistic concurrency is mandatory) and I14 (idempotency is mandatory). This ADR records why they are non-negotiable and how they are enforced.

The failure modes are concrete and they all look the same to the person in the chat: the assistant says one thing and the record says another.

- **Lost update from a stale read.** A user asks to change the travel date of a trip while, in another tab, the same user (or a colleague on the same account) adds an extra. Without a revision check the second commit writes a full snapshot built from a state that no longer exists, and the line silently disappears. The chat contains a confident receipt for both changes; the database contains only one.
- **Stale confirmation executing on a changed case.** The runtime shows a "Rebook" card bound to a preview of the case. The user edits the case in a following message, then scrolls up and clicks the old card. If the card's confirmation is not tied to the case revision it was rendered against, the runtime sends a document the user never saw. The specification treats this as a stale interaction that must be rejected (§15.5) and lists it as a required integration scenario (§27.4, scenario 7).
- **Double click, double effect.** A user clicks a confirmation button twice, or the client retries a request after a timeout. Each delivery carries the same intent. Without an idempotency key the runtime executes the command twice: two trips are emitted, two numbers consumed, two external submissions dispatched. The specification requires that a double click executes at most once (§27.4, scenario 8) and that a second click on a resolved interaction returns the original resolution result (§15.5).
- **Provider retry after a partial commit.** A provider fails while narrating the response, after commands have committed. If the runtime re-enters interpretation and command execution to recover, the same commands are proposed again. Idempotency is the last line of defence that turns that re-execution into a replay of the original outcome instead of a repeated effect (I17).
- **Crash between journal insert and domain commit.** A process dies after the command journal has recorded a command but before the domain change is durable. On restart the runtime must be able to resume by idempotency key and reach exactly one outcome, not zero and not two (§23 recovery rules, §27.7 chaos boundaries).

The forces pulling against this decision are real. Revision checks add a `RevisionConflict` path that every adopter must handle in the conversation, idempotency keys require a durable journal that is written in the same transaction as the domain change, and both add a small amount of latency and schema surface. The alternative, however, is a system whose visible claims cannot be trusted, which contradicts the project's technical promise: committed events decide claims, and an event that was committed twice or committed against a state nobody saw is not a claim anyone should read.

## Decision

The following statements are normative for every crate in the workspace and for every adopter that implements the store traits.

1. Every mutable case MUST carry a monotonically increasing revision, represented in the core crate as `CaseRevision`. A case without a revision MUST NOT be exposed to the reducer as a mutation target.
2. Every command that mutates a case MUST target an expected revision. The `CommandEnvelope` carries a `case_ref` whose expected revision is the revision the runtime loaded and reduced against; the runtime MUST NOT substitute a fresher revision at commit time.
3. The store MUST check the expected revision in the same database statement or transaction that writes the new state (§16.1). Zero affected rows MUST be surfaced as `RevisionConflict`, never as success and never as a blind overwrite.
4. A `RevisionConflict` MUST NOT be silently retried by re-applying the same command to the new revision. The runtime MUST reload the case, re-project the workflow, and either re-reduce the turn or return a conversational explanation; the choice is a policy decision, the silent retry is forbidden.
5. Every `CommandEnvelope` MUST carry a stable `idempotency_key`. The key MUST be derived deterministically from the trusted command origin (for a confirmed interaction: the interaction identifier and the payload hash; for a direct safe user act: the turn identifier and the normalized act), so that a redelivery produces the same key without any client cooperation.
6. Command identifiers and idempotency keys MUST be persisted before or atomically with execution (§16.2). The reference PostgreSQL schema enforces this with a unique constraint on `(account_id, idempotency_key)` in the `tf_command_journal` table.
7. A repeated command MUST return the previously persisted outcome, flagged as a replay (`idempotency_replay` in the `Commit` result), and MUST NOT repeat the effect, emit new domain events, or dispatch a new outbox row.
8. State, domain events, interaction resolution, and outbox rows that belong to one internal commit MUST be written in one transaction (§16.3). A partial commit across these tables is an invariant violation, not a degraded success.
9. Interaction responses MUST carry the expected case revision (`expected_case_revision`). An interaction bound to a prior revision MUST be treated as stale and MUST NOT execute, unless the interaction explicitly declares revision independence (§15.5).
10. External side effects dispatched through the outbox MUST carry an idempotency key toward the destination, enforced by `UNIQUE (destination, idempotency_key)` on `tf_outbox`. Where the remote system cannot honour idempotency, the runtime MUST NOT blindly resend; ADR-007 governs that path.

## Consequences

Positive:

- The chat can never claim a change that a concurrent writer erased; a conflict becomes a visible, explainable event instead of a silent data loss.
- Double clicks, client retries, provider fallbacks, and crash recovery all collapse into one code path: look up the idempotency key, return what already happened.
- Recovery after a crash is mechanical. The command journal tells the runtime exactly which commands are pending, committed, or failed, and resuming by key is safe at every boundary listed in §27.7.
- Interactions gain a precise notion of staleness. A card is valid for one revision of one case, and the runtime can say so in plain language when the user clicks an outdated one.
- Audit and replay (I20) become possible, because every command records the revision it expected and the outcome it produced.

Negative:

- Adopters must design their domain tables with a revision column and route every write through expected-revision transactions; read-modify-write full-row updates are not compatible with the store adapter.
- Every conversational flow needs a story for `RevisionConflict`. The runtime provides the detection, but the wording and the choice between re-reduce and ask-again belong to the adopter.
- The command journal and the unique constraints add write amplification and a small latency cost per command. The specification defers any quantitative statement about this cost to benchmarks; this ADR makes no claim about it.
- Idempotency keys must be derived deterministically. A key that includes a timestamp, a random value, or a client-supplied token defeats the guarantee and is a defect.

What adopters must do:

- Implement the store traits (`CommandJournal`, `EventJournal`, `OutboxStore`, and the interaction store) over a database that supports the required transactional semantics, or use the PostgreSQL reference implementation.
- Include the revision in every domain case snapshot and never mutate a case outside a Turnframe command.
- Handle `RevisionConflict` in the conversational layer and never map it to a generic error message that hides the cause.
- Bind every consequential interaction to a case revision and a payload hash; declare revision independence only for interactions whose meaning genuinely does not depend on the case content.

## Alternatives considered

1. **Pessimistic locking (row locks or advisory locks held for the duration of a turn).** Rejected. A turn includes model interpretation, a bounded read loop, and possibly a wait for a user to click a card; holding a database lock across a human decision is not viable, and lock ownership does not survive a process crash. Optimistic revisions cost nothing while nobody conflicts and degrade to a clear, recoverable error when someone does.
2. **Last-writer-wins with full-snapshot updates.** Rejected. This is the behaviour the specification names as the root of lost updates and stale confirmations (§31 explicitly calls for replacing read-modify-full-row updates with expected revisions). It makes the transcript lie about what was saved, which violates the principle that committed events decide claims.
3. **Best-effort deduplication at the transport layer (client-generated request IDs, short-lived caches).** Rejected as the sole mechanism. Client identifiers cannot be trusted to define command semantics (I7), a cache is not durable across a crash, and a provider-level retry never passes through the client at all. A durable journal keyed on a server-derived idempotency key is the only place where every redelivery path converges.
4. **Making revision checks and idempotency opt-in per command kind.** Rejected. The specification's dependency rules forbid feature flags that create materially different safety semantics (§6.2), and the release gates require that no critical command lacks idempotency and no mutable case lacks revision checking (§33). An opt-in would move the decision to every adopter and make the gate unverifiable.

## Enforcement

Invariants implemented:

- I13 (optimistic concurrency is mandatory) and I14 (idempotency is mandatory) directly.
- I17 (provider failure cannot repeat effects) and I19 (critical state reads fail closed) depend on this decision: fallback after commit is safe only because a replay returns the original outcome, and a revision that cannot be verified is treated as a conflict, not as permission to write.
- §15.5 lifecycle rules for stale interactions and repeated clicks are the interaction-level face of the same guarantee.

Tests and gates that prove it:

- Property tests in §27.2 for revision conflicts, repeated idempotency keys, duplicate interaction clicks, and arbitrary command sequences.
- State exploration tests in §27.3 asserting that stale interactions cannot execute and that rejected commands do not mutate.
- Runtime integration scenarios in §27.4: scenario 7 (stale confirmation after a case edit is rejected), scenario 8 (double click executes at most once), scenario 13 (provider failure after commit regenerates narration without repeating commands), scenario 14 (database timeout during an atomic batch produces no hidden partial state), and scenario 18 (reload returns the same ordered blocks and interaction state).
- Chaos tests in §27.7 at every commit boundary, verifying idempotent recovery.
- Release gates in §33: "No critical command lacks idempotency", "No mutable case lacks revision checking", "No stale interaction can execute", and "Crash recovery is tested at every commit boundary".
- Observability: the metrics `turnframe.command.revision_conflict` and `turnframe.command.idempotency_replay` (§26) make both paths visible in production, so a rise in either is an operational signal rather than a silent condition.

Related decisions:

- ADR-004 (persistent interactions own CTA semantics) relies on the revision binding described here to define when a card is stale.
- ADR-005 (domain events authorize operational claims) relies on the single-transaction commit so that an event exists only when the state change it describes is durable.
- ADR-007 (external actions use outbox/saga and explicit unknown outcomes) extends the idempotency key beyond the database boundary to the remote destination.

Responsible crates and modules:

- `turnframe-core` owns the types: `CaseRevision`, `CommandEnvelope` with its `idempotency_key` and `case_ref`, the `RevisionConflict` error variant, and the `Commit` result with its `idempotency_replay` flag. It contains no I/O and cannot enforce the checks; it makes them impossible to omit from the type signatures.
- `turnframe-runtime` derives idempotency keys from trusted origins, threads expected revisions from context loading through reduction to dispatch, refuses to execute a command whose revision or interaction ownership cannot be verified, and implements crash recovery by idempotency key.
- `turnframe-store` defines the object-safe persistence traits and the transactional boundary that every implementation must honour.
- `turnframe-store-postgres` is the reference implementation: the expected-revision `UPDATE`, the `tf_command_journal` unique constraint on `(account_id, idempotency_key)`, the `tf_outbox` unique constraint on `(destination, idempotency_key)`, and the single-transaction commit of state, events, interaction resolution, and outbox rows.
- `turnframe-store` ships the deterministic in-memory store used by tests and examples; `turnframe-test` provides the fixtures, fake stores and chaos injection points used by the property, exploration, and chaos suites above.
