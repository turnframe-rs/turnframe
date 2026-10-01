# Introduction

Turnframe is a Rust library for conversational applications that change real records: a travel
desk that rebooks flights, an onboarding flow that registers people, an assistant that files an
expense claim. People write to it the way people write, in one long message that asks for three
things and corrects one of them. Small language-model tasks read what the message means. Your code
decides what happens.

The design follows from one sentence.

> **Models propose meaning. Deterministic reducers decide effects. Committed events decide claims.**

It is built around the [**Flow Map**](/docs/flow-map): five steps every turn runs in order, each
handing the next only what it may decide.

1. **Persisted state determines the workflow view.** A pure projector turns the stored record into
   one lifecycle phase, its open obligations and at most one blocking card. The transcript is
   history, never state; [ADR-002](/docs/adr/002) records why the projector is pure.
2. **Small model tasks propose what a message means.** A fixed chain of narrow tasks reads each
   message: which units it holds, which offered operation each request asks for, which record it is
   about, and which of the user's words carry each value. Every answer has a strict schema, and code
   checks it before the next task sees it.
3. **Reducers decide effects.** The whole turn is reduced before anything runs. Corrections replace
   what they correct and keep what they do not change, constraints hold what they forbid, and every
   act compiles to a typed command with an expected revision and an idempotency key, under a policy
   you declare.
4. **Interactions authorize consequential actions.** A command whose policy asks for a click waits
   on a card bound to the record's revision, and a click on a card drawn before the record changed
   is refused.
5. **Committed events decide claims.** The reply may say that something happened only when a
   committed event backs it. Receipts, notices and cards are rendered by the server from persisted
   records.

## One message, many acts

A user of the sample travel desk writes:

```text
Rebook the outbound on the flight you offered, but don't touch the return, change my email
to marta@aurora.example, tell me how much hand luggage is included, and don't confirm
anything yet.
```

The runtime handles all of it in one turn:

- the rebooking becomes a card drawn for the exact revision of the trip, and a click on it after
  the airline re-quotes is refused as stale;
- the return is kept: the turn holds any act that would change it, and the domain's lock refuses
  one later;
- the email change is read by small checked tasks, resolved to one record and compiled into a
  typed command;
- «don't confirm anything yet» blocks every submission in the turn before anything runs;
- the question gets its own answer, on an explicit state basis;
- the reply claims only what committed events back, and asks for the one thing the work needs
  next.

Repeating the turn, clicking twice or crashing half way through cannot repeat an effect.

## Small models are the point

No understanding task needs more than a few hundred tokens of context, so the reading runs on
small models. Accuracy is bought with more checked calls on the same small model: a turn runs at
`low`, `medium` or `high` effort, and the level changes how a message is read,
never what policy, cards or the claim guard allow. What this release measured, and what it did not,
is in [benchmarks](/docs/benchmarks).

## When it fits

Turnframe is for assistants that write to records you have to be able to trust afterwards:
bookings, profiles, orders, claims, cases. It fits when a wrong write costs more than an extra
question, when the conversation has to survive reloads, double clicks and crashes, and when you
need to explain a turn later from its replay record.

It is not an agent framework in which a model freely calls write tools. Each state of each workflow
offers a closed catalog of operations, and the model can only choose among them. A product whose
assistant should browse, plan and act on its own is better served by a different design.

## Status

Turnframe 0.1 is on crates.io, and its API follows semantic versioning: a change that breaks the
public API waits for the next minor version, and the [changelog](/docs/changelog) names it. The
minimum supported Rust version is {{msrv}}. It is licensed under MIT or Apache-2.0, at your option.

## Where to go next

- [Installation](/docs/installation): the dependency line, feature flags and the crate family.
- [Quickstart](/docs/quickstart): one turn end to end, with no key, database or network.
- [Examples](/docs/examples): the travel desk, traveler onboarding, a mixed turn and the console.
- [The Flow Map](/docs/flow-map): the five steps every turn runs, and who decides at each.
- [Architecture](/docs/architecture): the pipeline, the twenty invariants and what you write.
