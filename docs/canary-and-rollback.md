# Canary and rollback, per workflow

A change to a conversational assistant is not one change. Editing a projector, a policy, a prompt or
a provider profile each alters what the assistant does, and they fail differently: a projector defect
shows up as a phase nobody expected, a policy defect as a command that ran without the confirmation
it needed, a prompt change as a slow drift nobody notices for a week. Releasing them together and
watching one number tells you something broke without telling you what.

So the unit of both canary and rollback here is **one workflow at one version**, never the
deployment. This document says how to do that with what the library provides, and what it
deliberately does not provide.

## What the library gives you, and what stays yours

The library gives you three things. A workflow declares a version, and every turn records which
version projected it, so a population can be split by version after the fact rather than only in
advance. The planning path runs a turn to completion without executing anything, so a new version
can answer alongside the old one while the old one stays authoritative. And the divergence
vocabulary describes what two paths disagreed about in terms both sides can compare.

The library gives you no traffic router, no cohort assignment and no feature flags, and this is a
decision rather than an omission. Deciding *which* conversations see a new version depends on how an
application identifies accounts, tenants and users, and a framework that guessed at that would be
wrong for most adopters and dangerous for some. Route in your own code; the library's job is to make
the comparison meaningful and the rollback safe.

## Canary in three stages

### Stage one: shadow, with nothing at stake

Run the new version through `Orchestrator::plan_turn` on the same turns the authoritative path
handles. The planning path stops before the first side effect of any kind: no interaction persisted,
no command journaled, no event, no outbox row, no conversation block. That is a property of its
types rather than a promise, because it holds a `ReadOnlyStores` and workflow executors reduced to
their loading half, so the write side is not in scope to be called by mistake.

Compare with `divergence::compare`, and read the result with one asymmetry in mind, which the
vocabulary encodes as its own variant rather than leaving to interpretation: a mutation the new path
refused because it could not tell which record was meant, that the old path performed anyway, is a
finding **against the old path**. A comparison that counts every difference as a regression will
tell you the safer version is the broken one.

Use `Orchestrator::plan_turn_from` to replay a recorded corpus rather than only watching live
traffic. A recorded turn is a user message plus the state that preceded it, so a corpus you already
have becomes a shadow corpus with a known composition, replayable against every subsequent change.
Comparing two versions against *live* state compares two states as well as two projectors, which is
not the experiment you meant to run.

Leave stage one when the divergences are understood, not when they reach zero. A new version that
diverges nowhere is usually a version that changed nothing.

### Stage two: authoritative for a slice

Route a small population to the new version and let it execute. Two things make this recoverable.

Each turn's replay record names the version that projected it, so after the fact you can separate
what the new version did from what the old one did without having tagged anything in advance. And
the metrics that matter here are the ones that separate a safety failure from a quality one:
`turnframe.command.revision_conflict`, `turnframe.interaction.stale`, `turnframe.claim.violation`,
`turnframe.target.ambiguous` and `turnframe.external.outcome_unknown` describe integrity, while
`turnframe.question.unanswered` and the clarification rate describe experience. Watch them
separately, because averaging them is how an integrity failure hides behind good conversations.

The specification's own gate is worth stating plainly as the exit condition: zero side-effect and
operational-claim integrity failures in the observation window, with the semantic metrics meeting
whatever threshold you agreed before you started, rather than one chosen afterwards to fit.

### Stage three: authoritative everywhere

Widen the slice. Keep the previous version registered.

## Rollback

Rolling back a workflow means routing turns to the previous version again. What makes it safe is
that nothing about a committed case is version-specific: the state is the application's, the events
are the ledger's, and both are readable by any version of the projector that understands the state
type.

Three rules keep it that way, and the first two are the ones that get broken.

**A version change must not require a state migration.** If the new version needs a state shape the
old one cannot read, rolling back means migrating data backwards under load, which is not a
rollback. Add fields, do not repurpose them, and let the old projector ignore what it does not know.

**A version change must not invalidate committed events.** Receipts already rendered were rendered
from events, and an event's meaning cannot be edited retroactively. A new version that reinterprets
an old event type is changing history, not behaviour.

**Cards outlive a rollback and must be answerable after it.** A card written by the new version
carries stored options whose meaning the server owns, and it stays valid across the switch. If the
new version writes a card whose option the old version cannot compile, that card is unanswerable
after a rollback and the user is stuck. Either keep the operation registered in both versions, or
invalidate the new version's open cards as part of rolling back, which the interaction store's
invalidation path does per case.

Rolling back is per workflow. A rollback of the trip workflow leaves the traveler workflow where
it is, because their versions are independent and their cases never share a revision.

## What to write down before you start

- Which workflow and which version, and what the previous version is.
- The routing rule, in your own code, and how to change it without a deploy.
- The exit condition for each stage, agreed before the first turn rather than after the first
  surprise.
- Which metrics you are watching, split into integrity and experience.
- Who decides to roll back, and on what evidence.

## What is proven, and what is procedure

The library's half is tested: the planning path cannot write, the seeded path holds no persistence
at all, the divergence vocabulary carries the asymmetry, and two versions of a workflow can coexist
in one registry with each turn recording which one projected it. `crates/turnframe-runtime/tests/`
carries those, and `crates/turnframe-store-postgres/tests/rollback.rs` exercises the state and
ledger rules above against a live database, showing that a case written under one version reads
back under the previous one with its events intact.

The rest of this document is procedure. It is written down because an untested procedure invented
during an incident is not a procedure, but it is not something a test can hold you to.
