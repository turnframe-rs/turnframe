<h1 align="center">
  <a href="https://turnframe.rs">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/turnframe-rs/turnframe/main/website/public/brand/turnframe-wordmark-on-dark.svg">
      <img alt="Turnframe" src="https://raw.githubusercontent.com/turnframe-rs/turnframe/main/website/public/brand/turnframe-wordmark-on-light.svg" width="247" height="48">
    </picture>
  </a>
</h1>

<p align="center"><b>Deterministic conversational workflows for Rust.</b></p>

<p align="center">
  <a href="https://crates.io/crates/turnframe"><img alt="crates.io" src="https://img.shields.io/crates/v/turnframe?style=flat-square&color=4b58ff"></a>
  <a href="https://docs.rs/turnframe"><img alt="docs.rs" src="https://img.shields.io/docsrs/turnframe?style=flat-square"></a>
  <a href="https://github.com/turnframe-rs/turnframe/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/turnframe-rs/turnframe/ci.yml?branch=main&style=flat-square&label=CI"></a>
  <img alt="MSRV 1.88" src="https://img.shields.io/badge/MSRV-1.88-4b58ff?style=flat-square">
  <img alt="License: MIT or Apache-2.0" src="https://img.shields.io/badge/license-MIT%20or%20Apache--2.0-4b58ff?style=flat-square">
</p>

<p align="center">
  <a href="https://turnframe.rs">Website</a> ·
  <a href="https://turnframe.rs/docs">Documentation</a> ·
  <a href="docs/flow-map.md">The Flow Map</a> ·
  <a href="https://docs.rs/turnframe">API reference</a> ·
  <a href="docs/benchmarks.md">Benchmarks</a> ·
  <a href="CHANGELOG.md">Changelog</a>
</p>

Turnframe combines probabilistic language understanding with typed workflow projection,
persistent human interactions, deterministic command execution, and event-backed responses.

It is built around the [**Flow Map**](docs/flow-map.md) architecture:

1. persisted state determines the workflow view;
2. small model tasks propose what a message means;
3. reducers decide effects;
4. interactions authorize consequential actions;
5. committed events determine what may be claimed.

> Let the model understand language. Let your code control reality.
>
> **Models propose meaning. Deterministic reducers decide effects. Committed events decide claims.**

Turnframe is not an "AI agent framework" in which a model freely calls write tools. It is a
framework for natural conversational applications with deterministic workflows, durable human
interactions and verifiable side effects, provider-neutral and domain-neutral.

## What you get

A user can write one long, colloquial message that confirms a card, changes a field, asks a
question and adds a constraint, and the runtime handles it safely:

```text
"Rebook the outbound on the flight you offered, but don't touch the return, change my email
 to marta@aurora.example, tell me how much hand luggage is included, and don't confirm
 anything yet."
```

- The rebooking card is drawn for the exact case revision, and a click on it after the airline
  re-quotes is refused as stale.
- The return is kept as it is: the turn holds any act that would change it, and a lock refuses
  one later.
- The email change is read by small model tasks, each checked by code, resolved to an exact
  target, and compiled into a typed command whose policy decides whether it applies now or waits
  for review.
- The "don't confirm anything yet" constraint blocks every submission act in the turn before
  anything runs.
- The question gets its own answer, on an explicit state basis.
- Every operational receipt in the reply is backed by a committed domain event.
- The next card the user needs is persisted before the reply refers to it.
- Repeating the turn, double-clicking or crashing mid-way cannot repeat an effect.
- Anything the turn declined to do is said out loud, in words a deployment writes, and the reply
  ends by asking for the next thing the work needs.

## How a turn is read

A message is not handed to one large prompt. It goes through a fixed chain of small tasks, each
with a narrow question, a strict schema and a check in code:

| Task | Question it answers |
|------|---------------------|
| segment | which units the message holds: requests, questions, values, corrections, chitchat |
| coverage | whether a request or question was missed |
| route | which offered operations a request asks for, one act each |
| locate | which record it is about, when more than one could be |
| extract | each argument's value, pointed at in the user's own words |
| verify | whether the act matches what the user said |

Dates and amounts are computed by code from what the model points at. A structural mistake goes
back to the model once with the exact error; a failed call is sent again in place; a task can vote
or escalate to a stronger model. Every task has a budget and a configurable prompt. This is what
lets a small, cheap model run a real workflow: no task needs more than a few hundred tokens of
context. The steps are streamed as the turn runs, so a chat surface can show what the message was
read to say while the rest happens.

The reply is written the same way. The acknowledgement is written from the turn's outcome (what
was done, what was not and why, and the one thing to ask next, which code chooses), then reviewed
against a checklist before it is shown. Each question gets its own answer task. When no model
produces a reply that passes, the server's own question stands in.

## Crate family

Install only the facade and pick features:

```toml
[dependencies]
turnframe = { version = "0.1", features = ["openai", "postgres", "telemetry"] }
```

| Crate | Role |
|-------|------|
| `turnframe` | Facade re-exporting the family behind feature flags |
| `turnframe-core` | Pure types and the deterministic Flow Map projector; no async runtime, HTTP or database |
| `turnframe-tasks` | Small verified model tasks: repairs, in-place retries, votes, escalation, budgets, records |
| `turnframe-understand` | The understanding pipeline over those tasks |
| `turnframe-runtime` | Turn orchestration: reduction, interactions, commands, events, the reply, tracing, replay |
| `turnframe-store` | Object-safe persistence traits and the deterministic in-memory store |
| `turnframe-store-postgres` | PostgreSQL reference store with migrations and expected-revision transactions |
| `turnframe-provider` | Provider-neutral model interfaces, capability routing, fallback policy, conformance suite |
| `turnframe-provider-openai` | OpenAI, Azure OpenAI and OpenAI-compatible endpoints (profiles) |
| `turnframe-provider-anthropic` | Anthropic Messages API |
| `turnframe-provider-gemini` | Google Gemini and Vertex AI |
| `turnframe-provider-bedrock` | AWS Bedrock Converse |
| `turnframe-provider-ollama` | Ollama |
| `turnframe-prompt` | Prompt sources: prompts compiled in from your own repository, a bounded cache, an optional Langfuse v4 adapter |
| `turnframe-test` | Test kit: scripted providers and tasks, fake stores, sample workflows, workflow exploration |
| `turnframe-eval` | Evaluation harness, scored per turn and per understanding task |
| `turnframe-telemetry` | Tracing spans, `turnframe.*` metrics, optional OpenTelemetry bridge |
| `turnframe-macros` | Reserved; no macros ship in 0.1 by policy |

Feature flags on `turnframe`: `openai`, `anthropic`, `gemini`, `bedrock`, `ollama`,
`all-providers`, `postgres`, `prompts`, `langfuse`, `telemetry`, `otel`, `test-kit`,
`eval`, `full`.

## Vocabulary

| Term | Meaning |
|------|---------|
| Turnframe | the project and the facade crate |
| Flow Map | the architectural pattern |
| `WorkflowDefinition` | the pure projector a domain implements: state in, `WorkflowView` out |
| `WorkflowView` | exactly one lifecycle phase, zero or more parameterized obligations, at most one blocking interaction requirement, an optional outcome |
| `Understanding` | what the tasks made of a message: its units, the acts it asks for, its questions. A proposal, never an effect |
| `TurnReducer` | the deterministic layer that resolves targets, corrections and constraints into typed commands |
| `Interaction` | a persistent, server-owned card or decision the user can respond to |
| `OperationSpec` | one operation as a turn offers it: its arguments, which targets it takes, whether a model may propose it |
| Event ledger | the committed domain events that alone authorize operational claims |

## Safety model

Model output is untrusted input. Every task answer is schema-validated all or nothing and checked
by code; a value must point at the user's own words; targets are resolved without guessing; policy
and reduction run before any command exists. Consequential commands need a server-issued origin
such as a confirmed interaction, and a raw model proposal is not a valid origin by construction.
Every command carries an expected case revision and an idempotency key. Every visible operational
claim is generated from committed events or authoritative external receipts, never from prose.
Ambiguity creates a selection card; the most recent record is never picked for you. Provider
fallback happens before effects, or after the commit for the reply only.

**A shape the runtime will refuse is not on the schema in the first place.** Each task's schema is
built for the turn in hand: an operation is one of the keys on offer, a record is one of the handles
in view, a card answer is one of that card's options. A model choosing an impossible combination
gets a repair round, and nothing is silently dropped. Where prevention is impossible the answer is
a refusal: nothing this library shows a person is edited model output.

**A turn that does less than it was asked says so.** A refused write, a declined card, a superseded
act, an unresolvable target: each reaches the user as a deterministic notice with its own stable
code, reaches an operator as its own metric, and reaches the reply's writer as a fact, so the prose
beside the notice cannot contradict it. The notice does not depend on a model running.

**Nothing is capped for you.** Answer length, transcript window, budgets and attachment counts are
all optional. A limit an adopter cannot raise is not a configurable limit, and a number chosen here
would be a decision about a product this library has not seen. Where a configured limit does bite,
the block is refused whole and reported, never shortened: a sentence cut in half reads like an
assistant that lost its thread.

## Tracing a turn

`TURNFRAME_TRACE=1` makes the examples write every turn to the gitignored `traces/` as JSON Lines:
the message, each understanding step, every model request with its full prompt and the answer, what
was understood and decided, and the reply. `JsonlTrace` and `TracedProvider` do the same for any
application, and the file is easy to load into Langfuse or any other tool that reads JSON.

## Documentation

- [Architecture guide](docs/architecture.md)
- [Composition: what the reply may say](docs/composition.md)
- [Reliability model](docs/reliability-model.md)
- [Persistent interactions](docs/interactions.md)
- [Provider adapters](docs/provider-adapters.md)
- [Evaluation](docs/evaluation.md)
- [Telemetry](docs/telemetry.md)
- [Threat model](docs/threat-model.md)
- [Consent and acceptance](docs/consent-and-acceptance.md)
- [Release checklist and production-readiness gates](docs/release-checklist.md)
- [Canary and rollback, per workflow](docs/canary-and-rollback.md)
- [Roadmap: what waits until after 0.1, and why](docs/roadmap.md)
- [Benchmarks: what is measured, what is not claimed](docs/benchmarks.md)
- [Architecture decision records](docs/adr/README.md)

Examples live under [`examples/`](examples/): a travel-disruption desk that walks the four
guarantees in order (dependent acts, a protected leg, a stale card, an airline that does not
answer), a shop's refund desk under nine attacks (the demonstration on
[turnframe.rs](https://turnframe.rs)), a traveler onboarding flow, and a mixed question-and-action
turn. Each is a runnable binary
built on the facade, the in-memory stores and a scripted provider, so `cargo run -p travel-desk`
needs no API key and no database, and each prints what the runtime actually did.

`examples/console` is the one that talks to a real model. It runs the travel desk interactively
against whichever endpoint your environment has a key for, from one seeded trip (Marta Bianchi's,
the outbound cancelled and a new flight quoted), so you can see what a model does with the contract
from the first message:

```sh
cp .env.dev .env     # put a key in .env; the console loads it
cargo run -p console # OpenAI, Anthropic or Gemini, whichever key is set; else a local Ollama
```

Each turn prints the understanding steps as they are decided, then what was understood and what the
runtime did with it, which are not the same list. Try rebooking the outbound while keeping the
return; register a traveler and put them on a trip in one message; give two fields in one sentence
and watch one revision commit both; ask what you can do, or where things stand.
`TURNFRAME_LOCALE=it-IT` has the replies written in Italian.

What a user of a real surface would read is printed in bright white, and what the console adds to
explain the turn is dimmed. With `TURNFRAME_TRACE=1` the first lines name the one file the whole
session is traced to. `/effort high` makes the turns that follow spend more calls to read you, and
each turn's cost line names its level.

## Status

Turnframe 0.1 is on crates.io. It follows semantic versioning: a change that breaks the public API
waits for the next minor version, and the [CHANGELOG](CHANGELOG.md) names it. The live evaluation
corpus measures how it reads real messages; [benchmarks](docs/benchmarks.md) says what is measured
and what is deliberately not claimed.

Minimum supported Rust version: **1.88**.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in the work by you shall be dual licensed as above, without
any additional terms or conditions.
