# ADR-012: Snapshot storage is allowed; the event journal remains mandatory for claims

- Status: Accepted (2026-09-05)

## Context

Turnframe needs two things from persistence that are easy to conflate: a place to keep the current state of a case (a trip, a traveler record, an onboarding file) and a place to keep the evidence that a consequential effect actually happened. Full event sourcing satisfies both with one mechanism, but it is a heavy price for an adopter who already owns relational tables, migrations, reporting queries and an ORM. The master spec resolves the tension in §0 rule 12: events are mandatory as the claim ledger, but applications may use snapshot storage plus an append-only event journal rather than full event-sourced aggregates. This ADR records that choice and the boundary it draws.

The forces on each side are real. A framework that demands event-sourced aggregates would exclude most existing applications and would push adopters to rebuild working domain models before they can put a single conversation in front of a user. A framework that lets the application persist only snapshots would break the technical promise that "committed events decide claims": without an append-only record of what committed, the runtime has nothing to point a receipt at, and the narrator's prose becomes the only record of what the system did.

The failure modes this decision prevents are concrete things that go wrong in a conversational application:

- A user asks the assistant to rebook a flight. The database write fails after the model has already drafted a reply. Without a journal that the receipt must reference, the reply "Your new flight is booked" reaches the chat with nothing behind it, and the user only discovers the truth when the traveler never receives anything.
- A user reloads the conversation the next day. If cards and receipts were reconstructed from the assistant's free text instead of from committed events and persisted blocks, the reload shows a "sent" status that the row in the domain table contradicts, and nobody can tell which one is right.
- An external submission times out after transmission. The snapshot still says "awaiting confirmation"; only an explicit committed event distinguishes "we do not know yet" from "it failed", and only that event can authorize a truthful "verification in progress" message instead of a blind retry.
- An operator audits why a traveler was told a deletion was completed. A snapshot shows the row is gone; it cannot say which command, under which revision, authorized by which interaction, produced the deletion. The audit questions listed in the spec (§26.4) become unanswerable.
- Two turns race on the same case. The second one overwrites the snapshot and the chat reports both outcomes as successful. The journal, written in the same transaction as the revision bump, is what makes the stale write visible as a conflict instead of a silent loss.

A pure snapshot store has no answer to any of these. A pure event-sourced store answers them but at the cost described above. The middle position is what the spec asks for and what this ADR adopts.

## Decision

1. Applications MAY persist current case state as snapshots in tables they own. Turnframe MUST NOT require event-sourced aggregates, event replay to rebuild state, or any particular domain table layout.
2. The `EventJournal` store (spec §22.1) is mandatory for every adopter. Every command that commits a change MUST append one or more `CommittedEvent` records in the same transaction as the snapshot write and the case revision bump (spec §16.3). A snapshot change without a journal entry is a defect.
3. The journal MUST be append-only. Events MUST NOT be updated or deleted by application code;
   corrections are expressed as new events.
4. One operation is exempt, and only one: an erasure MAY empty an event's payload in place, through
   the store contract's redaction operation, while the event's identity, type, sequence and
   timestamps survive unchanged. This keeps the property the rule exists to protect, since no event
   appears, disappears or changes position, and it exists because the append-only rule and the rule
   that a case's identity outlives its content otherwise compose into a system personal data enters
   and never leaves, which no adopter subject to an erasure obligation can lawfully run. A redacted
   event stays visibly redacted, so a receipt rendered over it says so rather than reading as though
   the data were still there, and the redaction records when it happened and under whose authority
   but never what was removed. A store that implements erasure as a delete breaks this decision, and
   the store conformance suite catches it.
5. Every committed event MUST carry the tenant, workflow key, case identifier, case revision at commit, the originating command identifier, an event type, a payload and an occurrence timestamp, as in the reference `tf_domain_events` table (spec §22.2). The event MUST be traceable to its `tf_command_journal` row.
6. Operational claims MUST be derived from committed events or authoritative external receipts, never from the snapshot alone and never from model output (invariant I16). An `OperationalReceipt` MUST reference at least one committed event identifier in `event_ids`; a receipt with an empty `event_ids` list MUST NOT be rendered for a consequential outcome.
7. The runtime MUST read events back from the journal after commit and compose receipts from that readback, not from the in-memory result of the write. A commit whose events cannot be read back MUST be treated as unknown, not as success.
8. The snapshot remains authoritative for current workflow state (invariant I1). `FlowProjector` and `WorkflowDefinition` implementations project from the snapshot, not from the event stream. The journal is authoritative for what happened; the snapshot is authoritative for what is.
9. Chat persistence MUST store the exact typed response blocks that were returned live (spec §22.3). Reload MUST NOT reconstruct receipts or cards from free text or from the snapshot.
10. Applications MAY additionally adopt full event sourcing for their own reasons. Doing so MUST NOT change how the runtime derives claims: the journal contract in points 2 to 7 still applies.

## Consequences

Positive:

- Adopters keep their existing domain tables, migrations and reporting queries. Integration starts by adding one journal table and wrapping existing writes in an expected-revision transaction, not by rebuilding aggregates.
- Every visible success statement has a row behind it. The claim boundary of §17.4 becomes checkable in tests and in audit rather than a matter of prompt discipline.
- Crash recovery (spec §23.1) has a clean rule: if events are committed, regenerate the response from them without re-executing; if none are, restart safely. The journal is what makes "committed" a fact the runtime can check.
- The audit trail can answer "what committed" and "which event authorized each receipt" without a replayable aggregate.

Negative:

- Two writes per effect instead of one. Adopters pay a modest storage and write-amplification cost for the journal, and the transaction spans two tables in the common case.
- Snapshot and journal can drift if an adopter bypasses the runtime and writes to a domain table directly. The framework cannot detect writes it did not perform; only discipline and the tests below protect this seam.
- Rebuilding state from the journal is not guaranteed to be possible, because the journal was never designed to be a complete state log. Adopters who want time travel must opt into event sourcing themselves.
- The journal grows without bound by design. Retention and archival are an adopter concern, with the constraint that events referenced by live receipts or open reconciliations must remain readable.

What adopters must do:

- Implement the `EventJournal` trait, or use the PostgreSQL reference implementation with the `tf_domain_events` and `tf_command_journal` tables.
- Route every mutation through a Turnframe command so that snapshot write, revision bump, event append and interaction resolution share one transaction.
- Assign `ClaimMode::ServerReceiptOnly` to high-risk outcomes and provide deterministic receipt renderers keyed on event type, so that claims about those outcomes never depend on the narrator.
- Store external protocol identifiers and intermediate statuses (prepared, submitted, received, accepted, rejected, and so on) as distinct events rather than collapsing them into a single "done" flag on the snapshot.

## Alternatives considered

Full event sourcing as the only supported model. Rejected because it forces adopters to abandon working relational schemas, makes the first integration a domain rewrite, and contradicts §0 rule 12 directly. It also moves complexity into the wrong place: the framework's promise is about claims and effects, not about how an application chooses to represent its aggregates.

Snapshot storage only, with claims derived from a successful write. Rejected because a successful write proves that a row changed, not which command, revision and interaction authorized the change, and it leaves nothing for a receipt to reference. Reload would have to reconstruct status from the snapshot or the transcript, which violates §22.3 and invariant I16, and every failure mode listed in the context section returns.

Optional journal enabled by feature flag. Rejected because §6.2 forbids feature flags that create materially different safety semantics. A build in which claims are not backed by events is a different product with a weaker promise, not a configuration of the same one.

Journal written asynchronously after the snapshot commit. Rejected because a crash between the two writes produces a snapshot that changed with no evidence that it did, which is exactly the hidden partial state that §27.4 scenario 14 forbids. The same-transaction rule of §16.3 is the only arrangement in which "committed" means one thing.

## Enforcement

Invariants from spec §4 implemented by this decision:

- I1 (persisted state is authoritative): the snapshot, not the transcript, is what projection reads.
- I13 (optimistic concurrency): the journal entry records the revision at commit and is written in the transaction that bumps it.
- I14 (idempotency): the command journal row that every event references is what makes a repeated delivery return the original outcome.
- I15 (external uncertainty is explicit): unknown remote outcomes are events, not snapshot flags.
- I16 (events authorize claims): the direct subject of this ADR.
- I20 (replay is possible): events and command outcomes are part of the replay record.

Tests and release gates that prove it:

- §27.4 scenario 10 (a failed command cannot produce a resolved-looking receipt), scenario 13 (a provider failure after commit regenerates narration without repeating commands), scenario 14 (a database timeout during an atomic batch produces no hidden partial state), scenario 18 (reload returns the same ordered blocks) and scenario 20 (no critical success phrase without a matching receipt or event).
- §27.7 chaos injection after domain commit but before event readback, and before response persistence: recovery must regenerate from committed events and must never claim what the journal does not hold.
- §27.2 property tests on arbitrary command sequences and revision conflicts: every accepted command yields a journal entry at the new revision; every rejected command yields none.
- §33 safety gates "no critical success receipt lacks committed event IDs" and "no mutable case lacks revision checking"; operational gates "crash recovery is tested at every commit boundary", "live response and reload use the same persisted blocks" and "audit records reconstruct command authorization and claims".

Responsible crates and modules:

- `turnframe-core` owns the `CommittedEvent`, `Commit`, `ClaimMode` and `OperationalReceipt` types and the rule that a receipt carries event identifiers.
- `turnframe-store` owns the `EventJournal`, `CommandJournal` and `ReplayStore` traits and the expected-revision transaction boundary that ties the snapshot write to the event append.
- `turnframe-store-postgres` owns the `tf_domain_events` and `tf_command_journal` tables, their migrations and the single-transaction commit path.
- `turnframe-runtime` owns the post-commit event readback and the composition of receipts from committed events, and the crash-recovery decision that regenerates rather than re-executes once events exist.
- `turnframe-store` ships the deterministic in-memory store; `turnframe-test` provides the fake stores and the chaos injection points at each commit boundary used by the tests above.
