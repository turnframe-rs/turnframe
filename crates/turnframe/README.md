# turnframe

**Deterministic conversational workflows for Rust, built around the Flow Map architecture.**

This is the crate an application installs. It re-exports the whole `turnframe-*`
family behind feature flags, so a normal application never names a sub-crate and
never keeps a set of version numbers in step.

```toml
[dependencies]
turnframe = { version = "0.1", features = ["openai", "postgres", "telemetry"] }
```

> The model proposes meaning. Deterministic code decides effects. Committed
> events decide claims.

## Scope

Turnframe is not a framework in which a language model calls write tools. A free
tool-calling agent guesses what the user meant, decides what happens and says what
happened, all in one place. Turnframe gives each job its own layer and its own
trust level:

- a **workflow** projects persisted state into a view: one lifecycle phase, zero
  or more parameterized obligations, at most one blocking interaction, an
  outcome only when the work is really finished;
- the **understanding** runs small model tasks, each checked by code, that turn
  the message into an untrusted proposal whose every value points at the user's
  own words;
- a **reduction** resolves the whole turn (corrections, cancellations,
  constraints, ambiguity) into typed commands with an expected revision, an
  idempotency key and a trusted origin;
- **committed events** decide what the reply is allowed to claim, and the reply
  asks for the next thing the work needs.

## Minimal use

The vocabulary is at the crate root, and the prelude pulls in the common set:

```rust
use turnframe::prelude::*;

let case = CaseRef::new("trip", "trip-1", CaseRevision(3));
assert_eq!(case.key(), CaseKey::new("trip", "trip-1"));

// A consequential command is not authorized by a model proposal; there is no
// origin variant that could express one.
assert_eq!(CommandPolicy::low_risk().risk, RiskClass::ReversibleLowRisk);
```

A runnable end-to-end turn (a workflow, the in-memory stores, scripted tasks,
one message, one committed event, one receipt) is in the crate documentation
under the `test-kit` feature. Longer programs are under
[`examples/`](https://github.com/turnframe-rs/turnframe/tree/main/examples): a
travel-disruption desk walking the four guarantees, a traveler onboarding flow and a
mixed question-and-action turn, which run with no API key and no database, and a
console that runs the travel desk against a real model.

## Features

None are on by default, and none change the safety semantics of the runtime.

| Feature | What it turns on |
|---|---|
| `openai` | OpenAI, Azure OpenAI and OpenAI-compatible endpoints |
| `anthropic` | the Anthropic Messages API |
| `gemini` | Google Gemini and Vertex AI |
| `bedrock` | AWS Bedrock Converse |
| `ollama` | a local Ollama daemon |
| `all-providers` | every adapter above |
| `postgres` | the PostgreSQL reference store and its migrations |
| `prompts` | prompt sources: prompts compiled in from your repository, and a cache |
| `langfuse` | `prompts` plus a source backed by a Langfuse project |
| `telemetry` | the `turnframe.*` metrics observer, tracing spans and the dashboard description |
| `otel` | `telemetry` plus the OpenTelemetry bridge |
| `test-kit` | scripted providers and tasks, fake stores, workflow exploration, three sample domains |
| `eval` | the model evaluation harness |
| `full` | `all-providers`, `postgres`, `prompts`, `telemetry`, `test-kit`, `eval` |

## Links

- [Workspace README](https://github.com/turnframe-rs/turnframe/blob/main/README.md)
- [Architecture guide](https://github.com/turnframe-rs/turnframe/blob/main/docs/architecture.md)
- [Reliability model](https://github.com/turnframe-rs/turnframe/blob/main/docs/reliability-model.md)
- [Persistent interactions](https://github.com/turnframe-rs/turnframe/blob/main/docs/interactions.md)
- [Provider adapters](https://github.com/turnframe-rs/turnframe/blob/main/docs/provider-adapters.md)

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
