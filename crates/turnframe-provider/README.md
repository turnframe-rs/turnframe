# turnframe-provider

The provider-neutral model layer of [Turnframe](https://github.com/turnframe-rs/turnframe).

The runtime never sees an OpenAI, Anthropic, Gemini or Bedrock wire type. It speaks the vocabulary
in this crate (a normalized request, a normalized response, a declared set of capabilities), and
vendor quirks stay inside the adapter crates (`turnframe-provider-openai`, `-anthropic`, `-gemini`,
`-bedrock`, `-ollama`). This crate holds that vocabulary, the routing and fallback policy built on
top of it, and the conformance suite every adapter must pass.

One sentence explains the shape of everything here: **models propose meaning, deterministic code
decides effects, committed events decide claims.** An adapter is the channel a proposal arrives
through. It decides nothing, and it must never make a weak proposal look like a strong one.

## Scope

| Module | What it holds |
| --- | --- |
| `request` | `ModelRequest`, `Message`, `ContentPart` (text, image, document, tool call, tool result), `ImageSource`, `DocumentSource`, `OutputSpec`, `ToolSpec`, `RequestMetadata`. |
| `response` | `ModelResponse`, `FinishReason`, `TokenUsage`, and the `text()` / `tool_calls()` / `single_json()` readers. |
| `stream` | `ModelStream`, `StreamEvent`, and `reconstruct`, which rebuilds a full response deterministically. |
| `structured` | All-or-nothing parsing: `CompiledSchema`, `SchemaCache`, `parse_structured`, `StructuredOutputError`. |
| `capabilities` | `ProviderCapabilities`, `StructuredOutputCapability`, `CapabilityRequirements`, `ModelProfile`. |
| `purpose` | `ModelPurpose` and the capability and logging policy each purpose carries. |
| `error` | `ProviderError`, `ProviderErrorKind`, `RetryClass`. |
| `provider` | The `ModelProvider` trait. |
| `router` | `ProviderRouter`, `PolicyRouter`, `RoutingPolicy`, `ProviderPool` and clock-injectable health. |
| `fallback` | `RetryPolicy`, `FallbackStage`, `execute_with_fallback`, `ProviderAttempt`. |
| `secret` | `ApiKey` over `secrecy`, and the `Redactor` hook with a `DefaultRedactor`. |
| `testing` | `StaticProvider`, `ManualClock`, `ImmediateSleeper`, always compiled, for other crates' tests. |
| `conformance` | The reusable adapter suite, behind the `conformance` feature. |

Out of scope: HTTP clients, vendor authentication, workflow policy, command handlers. An adapter
never receives a command handler or an external credential.

## The four rules this crate exists to keep

**Model output is all-or-nothing.** If a response proposes three acts and one is malformed,
`parse_structured` rejects the whole thing. There is no function here that returns a partial value,
because executing the parseable subset of a bad plan is how a correction gets silently dropped
(invariant I18).

**No silent capability downgrade.** A profile declares what it can do; `PolicyRouter` filters on
capability fit *before* anything else and returns a `RoutingError` naming what was missing rather
than an empty list or a weaker substitute. An understanding task is never served by a
prompt-only transport (§0 rule 9, §20.4).

**Provider failure cannot repeat an effect.** `FallbackStage` is a required positional argument of
`execute_with_fallback`, so a caller cannot forget where in the turn they are. Passing a critical
purpose with `PostCommitNarration` is refused before the model is called (invariant I17).

**Secrets never reach a prompt or a log.** `ApiKey` has no `Display` and no `Serialize`; its `Debug`
is `ApiKey(REDACTED)`. `ProviderError` has no field a response body fits into, and its `Display`
renders a kind, configured keys and a sanitized code (§25.2).

## Using it

```rust
use std::sync::Arc;
use turnframe_provider::prelude::*;
use turnframe_provider::testing::StaticProvider;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
// Two profiles: one that enforces a JSON schema, one that only sees it in a prompt.
let strong = StaticProvider::new("openai", "gpt-4o").with_capabilities(
    ProviderCapabilities::minimal()
        .with_structured_output(StructuredOutputCapability::NativeJsonSchema),
);
let weak = StaticProvider::new("local", "llama").with_capabilities(
    ProviderCapabilities::minimal()
        .with_structured_output(StructuredOutputCapability::PromptOnly),
);

let pool = ProviderPool::builder()
    .provider(Arc::new(weak))
    .provider(Arc::new(strong))
    .build()?;
let router = PolicyRouter::new(Arc::new(pool));

// An understanding task admits only the strong profile.
let requirements = ModelPurpose::Extract.requirements();
let candidates = router.select(ModelPurpose::Extract, &requirements, &RoutingPolicy::new())?;
assert_eq!(candidates.len(), 1);
assert_eq!(candidates[0].reference().to_string(), "openai/gpt-4o");
# Ok(())
# }
```

Then run the stage, letting the fallback policy record every attempt:

```rust,ignore
let outcome = execute_with_fallback(
    &candidates,
    &request,
    FallbackStage::PreCommit,
    &FallbackOptions::new(),
)
.await?;

let arguments: Value = parse_structured(&outcome.response, &schema)?;
for attempt in &outcome.attempts {
    replay.provider_attempts.push(attempt.to_core_record());
}
```

## Writing an adapter

1. **Create the crate** as `turnframe-provider-<vendor>`, depending on `turnframe-provider` and, for
   tests, on this crate's `conformance` feature. Do not depend on `turnframe-runtime`.
2. **Take the credential as an `ApiKey`** at configuration-parsing time, so the plain `String` never
   lives long enough to be logged.
3. **Declare capabilities honestly**, per provider-model pair, as configuration rather than as a
   constant derived from the vendor. If the endpoint offers schema enforcement, use it and declare
   `NativeJsonSchema`; if it offers only function calling, use it as a transport and declare
   `NativeFunctionSchema`; if neither holds, declare what is true even though that excludes the
   profile from the understanding tasks.
4. **Implement `generate`**: normalized request in, vendor request out, vendor response back into a
   `ModelResponse`. Honour `ModelRequest::timeout`, send `request_id` as an idempotency hint where
   the vendor supports one, and stay cancel-safe.
5. **Implement `stream`** only if the profile declares `streaming: true`. Reassembly through
   `reconstruct` must equal what `generate` returns for the same exchange. If the model cannot
   stream, leave the default, which refuses honestly rather than chunking a finished answer.
6. **Map errors** onto `ProviderError` variants. Never put a header or a body into an error; the
   `ErrorCode` constructor sanitizes what you pass, but the discipline is to pass a code. Three
   pairs need the body, not the status line, and the conformance suite has a row for each:

   | Do not report | When it is really | Because |
   | --- | --- | --- |
   | `Authentication` | `CredentialExpired` | A wrong key stays wrong; an expired token works again after a refresh. Vertex AI bearer tokens and Bedrock session credentials expire by design, and both arrive on the same 401 as a bad key. Reported as `Authentication`, a caller holding a refresher gives up instead of refreshing. Its class is `Fallback`, never `Retry`: a caller with a refresher may refresh and call the same profile again, a caller without one treats it as fatal for that profile. |
   | `RateLimited` | `QuotaExhausted` | A rate limit clears by waiting; a spent quota or an empty balance clears when a window resets or a human pays. Several vendors report both with 429. Its class is `Fallback`, so the router moves on rather than sleeping through the turn's deadline. |
   | `InvalidRequest` | `ContextOverflow` | Both arrive as 400; only one is fixed by shrinking the prompt. |
7. **Warn instead of lying.** `ResponseWarning::SynthesizedCallIds` and `FeatureDropped` are the
   channel for "I did the job, but not the way you asked". There is no warning for structured
   output: an adapter that cannot enforce a schema lowers its declaration.
8. **Run the conformance suite** (below), and add optional live smoke tests behind an environment
   guard, outside CI.

## Running the conformance suite

The suite owns the corpus (the schema, the requests, the payloads), so two adapters are measured on
the same thing. The adapter supplies a `ProviderFactory` that builds it against a base URL with a
dummy key, and a `WireFixtures` that mounts one vendor-shaped mock per `Scenario`.

```toml
[dev-dependencies]
turnframe-provider = { workspace = true, features = ["conformance"] }
wiremock = { workspace = true }
```

```rust,ignore
#[tokio::test]
async fn adapter_conforms() {
    let report = run_all(&MyFactory, &MyFixtures).await;
    assert!(report.passed(), "{report}");
}
```

The report covers the seventeen rows of spec §20.8: valid structured response, malformed JSON,
unknown fields, missing required fields, multiple acts, tool and read request ids, streaming
reconstruction, empty output, refusal, timeout, rate limit with `Retry-After`, authentication
failure, context overflow, cancellation, retry classification, secret redaction, and no silent
capability downgrade.

Three more rows exist because reassembly alone proves less than it looks like it does:

- `streaming_incremental`: an answer that arrived as several wire events must produce several
  deltas. An adapter that reads the whole body and emits one delta at the end reassembles
  perfectly and gives an adopter a spinner.
- `streaming_usage_agreement`: the streamed and the whole path must report the same
  `TokenUsage` for the same answer. Usage rides on a final frame in most vendors, so it is the
  first thing a streaming implementation drops, and the loss is invisible until someone compares
  two bills.
- `token_usage_contract`: `cached_input` is a subset of `input`, never a figure beside it.
  `input` is the whole prompt; an adapter reporting the net figure makes every cost estimate
  wrong and every cache-hit ratio over one.

It also covers thirteen **per-status rows**, because the classification row alone cannot catch an
adapter that maps every failure onto one kind: `transport` for everything is internally consistent,
classifies correctly and is useless. Each row puts one wire failure in front of the adapter and
demands one kind back, with the retry class that kind must carry: 401 to `authentication`, 403 to
`authorization`, 404 to `model_not_found`, 408 and a connection reset to `timeout` (a reset may also
be `transport`; both are retryable), 429 to `rate_limited` with the advertised delay intact, a 400
the vendor describes as a context-length problem to `context_overflow` and a genuinely bad 400 to
`invalid_request`, 500 and 503 to `server`, a safety filter to `content_filter` or `refusal`, the
vendor's expired-credential signal to `credential_expired`, and its quota or billing signal to
`quota_exhausted`. A failure names the status, the expected kind and the kind observed.

The connection-reset row needs no fixture: the suite binds its own socket and closes it on the
adapter, because a dead connection is a property of the socket and no mock response can express one.

Where an endpoint genuinely cannot produce a row (no separate 403, no 408, no per-request quota
signal), say so **in words** by overriding `WireFixtures::status_support` with
`RowSupport::not_producible("…")`. `WireFixtures::feature_support` says the same about the feature
rows a deployment can genuinely lack: the three streaming rows, authentication, rate limit and
refusal, which `Check::is_declarable()` enumerates. A daemon started on a laptop authenticates
nobody, meters nothing and filters nothing, and it says so rather than being measured on bodies a
proxy would have sent. The default of both hooks is `Mounted`, so silence is never a skip, and a
skip without a reason fails the row. `ConformanceReport::compatibility_table()` renders skipped rows
as *unproven*, so a table copied into a documentation page cannot claim a mapping the run never
exercised.

Declaring `streaming: false` does not make the streaming rows disappear either: they fail, and the
failure points at `feature_support`. A compatibility table must not show a blank where an
unimplemented feature sits. Every other row is a property of the adapter rather than of what
surrounds it, and a declaration on one of those fails the row instead of skipping it.

Two rules for reading it:

- A failing structured-output row means the **declaration** is wrong, not the test. Lower
  `structured_output` to what the profile actually does.
- A **skipped** row is not a pass. It means the deployment said, in words, that it cannot produce
  the row, so the adapter is unproven there.

**Conformance is per provider-model pair.** A passing report for `openai/gpt-4o` says nothing about
`openai/gpt-4o-mini`, which is why the report records both keys.

`tests/conformance_selftest.rs` runs the suite against a toy in-crate adapter in both directions: a
correct adapter must pass every row, and a set of deliberately broken ones (a leaked key, an
omitted schema, renamed call ids, flattened errors, an invented plan, a stream buffered and released
at the end, a dropped final usage frame, a net input count) must each be caught on the row that
describes the defect. A harness that cannot catch those is worse than no harness.

## Testing helpers

`turnframe_provider::testing` is compiled unconditionally, so the runtime, store and evaluation
crates can drive a turn without a network: `StaticProvider` answers from a script and declares
whatever capabilities the test gives it (dishonest ones included, which is what a no-silent-downgrade
test needs), `ManualClock` makes health cooldowns deterministic, and `ImmediateSleeper` records a
backoff instead of waiting it out.

## More

See the workspace [README](../../README.md), the [provider adapter
guide](../../docs/provider-adapters.md) and
[ADR-008](../../docs/adr/ADR-008-provider-capability-routing-and-no-silent-downgrade.md).

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
