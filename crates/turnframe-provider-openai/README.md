# turnframe-provider-openai

The chat-completions adapter of [Turnframe](https://github.com/turnframe-rs/turnframe).

One crate serves what looks like three vendors and is one wire format:

| Surface | How it differs |
| --- | --- |
| **OpenAI** | `https://api.openai.com/v1/chat/completions`, `Authorization: Bearer`. |
| **Azure OpenAI** | `{resource}/openai/deployments/{deployment}/chat/completions?api-version=…`, `api-key` header. |
| **Any OpenAI-compatible endpoint** | Same path as OpenAI, a base URL and a capability declaration you supply. Named presets ship for OpenRouter, Together, Fireworks, DeepInfra, vLLM, `llama.cpp` server, Groq, Mistral and xAI. |

It speaks the **chat completions** surface rather than the newer responses API for one reason: it is
the surface every compatible endpoint implements. Tools use the standard function shape.

## The five structured-output transports

Which transport is used is decided by the **declared capability** and by nothing else: never by
what the caller asked for, and never by what the endpoint might happen to accept. That is the
no-silent-downgrade rule made mechanical.

| Declaration | On the wire | Backed by |
| --- | --- | --- |
| `native_json_schema` | `response_format: {"type": "json_schema", …}` carrying the schema, with `strict` | OpenAI, Azure OpenAI, gateways that copy them |
| `native_function_schema` | one declared function whose `parameters` **are** the schema, with `tool_choice` pinned to it | any endpoint with tool calling |
| `grammar_constrained` | `guided_json` (vLLM) or `grammar` (`llama.cpp`), per the profile's `GrammarDialect` | the two self-hosted runtimes |
| `json_object` | `response_format: {"type": "json_object"}` plus the schema described in the system prompt | nearly everything |
| `prompt_only` | the schema described in the system prompt, and nothing else | endpoints that enforce nothing |

The forced function is a **transport, never an execution** (spec §20.4): the model is made to fill
in the one slot the format leaves open, and the adapter reads the document back out. Nothing is
dispatched. A request that also declares real tools is refused rather than silently stripped of
them, because serving both would mean dropping one side without saying so.

The `llama.cpp` half of the grammar transport compiles the JSON Schema into GBNF. A grammar
constrains *shape*, not values, so `pattern`, `minimum`, `oneOf`, `$ref` and their relatives are
**refused by name** (with the keyword and the JSON pointer in the error code) instead of being
dropped into a grammar weaker than the schema the profile promised to enforce. Objects are closed
and their properties keep schema order, required ones first; both are narrowings, so every document
the grammar admits satisfies the schema.

## A shared wire format is not shared behaviour

This is the sentence the whole crate is built around. Two endpoints that accept the same JSON can
differ on whether a schema is enforced, whether tool-call ids survive, whether usage is reported,
how a stream is framed and what an error body looks like, and **the same endpoint can differ
between its own models**.

So capabilities are declared per profile *and* model, never inferred from a brand
([spec §20.3](../../docs/provider-adapters.md)); the builder refuses a declaration its profile does
not back; and the only thing that licenses a declaration is a passing run of the conformance suite
against that endpoint and that model.

## Building each profile

```rust
use turnframe_provider::prelude::*;
use turnframe_provider_openai::{OpenAiProvider, profile::Preset};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
// OpenAI: schema enforcement, parallel tools, vision, documents, streaming.
let openai = OpenAiProvider::openai()
    .api_key(ApiKey::new("sk-not-a-real-key"))
    .model("gpt-4o-2024-08-06")
    .build()?;
assert_eq!(openai.endpoint(), "https://api.openai.com/v1/chat/completions");

// Azure OpenAI: the deployment is in the path, the version in the query.
let azure = OpenAiProvider::azure_openai("2024-10-21")
    .api_key(ApiKey::new("not-a-real-key"))
    .base_url("https://contoso.openai.azure.com")
    .model("gpt-4o")
    .deployment("prod-4o")
    .build()?;
assert!(azure.endpoint().contains("/openai/deployments/prod-4o/"));

// A gateway: the preset gets you connected, you declare what you measured.
let groq = OpenAiProvider::preset(Preset::Groq)
    .api_key(ApiKey::new("gsk-not-a-real-key"))
    .model("llama-3.3-70b-versatile")
    .capabilities(
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::JsonObject)
            .with_streaming(true),
    )
    .build()?;
assert!(!groq.capabilities().structured_output.enforces_schema());

// A self-hosted runtime: no default URL, and no credential required.
// Its server constrains decoding, so `grammar_constrained` is declarable
// once a conformance run against this model has proved it.
let local = OpenAiProvider::preset(Preset::Vllm)
    .base_url("http://gpu-01.internal:8000/v1")
    .model("Qwen/Qwen3-32B")
    .capabilities(
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::GrammarConstrained)
            .with_streaming(true),
    )
    .build()?;
// Which makes it eligible for the understanding tasks (spec §20.4).
assert!(local.capabilities().structured_output.enforces_schema());
# Ok(())
# }
```

The builder also takes an organization and a project (sent as `OpenAI-Organization` and
`OpenAI-Project`), extra headers for gateways that want one, a transport timeout, per-million-token
costs, a region label and routing tags. The effective deadline of a call is the smaller of the
builder's timeout and the request's own, so a caller can always ask for less time and never for more.

## The honest scope of the presets

A preset is a **starting point, not a certificate**. It fills in a provider key, a base URL, the
wire quirks that endpoint is known to need, and a deliberately conservative capability declaration:
`json_object`, and no schema enforcement claimed on anybody's behalf. OpenRouter alone fronts
hundreds of models; two of them behind the same preset can disagree about every row of the
conformance table.

| Preset | Default base URL | Credential |
| --- | --- | --- |
| `OpenRouter` | `https://openrouter.ai/api/v1` | required |
| `Together` | `https://api.together.xyz/v1` | required |
| `Fireworks` | `https://api.fireworks.ai/inference/v1` | required |
| `DeepInfra` | `https://api.deepinfra.com/v1/openai` | required |
| `Groq` | `https://api.groq.com/openai/v1` | required |
| `Mistral` | `https://api.mistral.ai/v1` | required |
| `Xai` | `https://api.x.ai/v1` | required |
| `Vllm` | none (your own host) | optional |
| `LlamaCpp` | `http://127.0.0.1:8080/v1` | optional |

Each preset declares `json_object` out of the box and claims schema enforcement for nobody. What
differs per preset is the **ceiling**: the strongest transport that profile admits at all.

| Preset | Ceiling | Why |
| --- | --- | --- |
| `OpenRouter`, `Together`, `Fireworks`, `DeepInfra`, `Groq`, `Mistral`, `Xai` | `native_json_schema` | they front models that enforce a schema, and models that do not |
| `Vllm` | `native_json_schema` | `guided_json` is its grammar transport, and recent releases also read a `json_schema` response format |
| `LlamaCpp` | `grammar_constrained` | it enforces a schema only through a grammar, so a `json_schema` response format would be a claim its server never reads |

Both self-hosted presets name a grammar dialect, so `grammar_constrained` can be declared for them
after a passing run. A profile that names no dialect refuses that declaration at build time rather
than sending a field the endpoint ignores.

**What you are expected to do:** run the conformance suite against your endpoint and your model,
then pass what it proved to `.capabilities(…)`. Raising a declaration without a passing run is the
most dangerous misconfiguration in the system, because routing, the refusal to downgrade a critical
stage and the decision to trust a parsed plan all rest on it.

A declaration stronger than the ceiling is refused at build time (the `LlamaCpp` preset cannot be
talked into claiming `NativeJsonSchema`), and the ceiling only ever lowers, since a ceiling a caller
can lift is not one. Two further declarations are refused because the profile has no way to carry
them: `GrammarConstrained` without a grammar dialect, and `NativeFunctionSchema` without tool
calling, which is the function slot the transport forces.

## What never leaves

- The credential is an `ApiKey` from `turnframe-provider`, reaches the wire as a header value marked
  sensitive, and appears in no `Debug` output. What `Debug` shows instead is an eight-character
  fingerprint that identifies *which* key is configured without revealing it, plus the header names.
- The base URL may not carry `user:password@`, and an extra header may not claim `authorization`,
  `api-key` or `x-api-key`. A credential travels in the credential slot.
- No response body, no header and no prompt reaches a `ProviderError`. What survives a failure is a
  typed kind, a short sanitized code the endpoint's own `code` or `type` suggested (passed through
  a redactor seeded with the configured key first), and, for a context overflow, the two token
  counts the message named.

## Errors

| On the wire | Normalized | Retry class |
| --- | --- | --- |
| 429 with `Retry-After` | `RateLimited { retry_after }` | retry after the delay |
| **429 with `insufficient_quota`** or an empty balance | `QuotaExhausted { scope }` | **fall back**: waiting does not refill a quota, whatever the header says |
| 401 with a wrong or revoked key | `Authentication` | fall back |
| **401 with an expired token** | `CredentialExpired` | fall back: a caller holding a refresher may refresh and retry the same profile |
| 402, or any billing signal in the body | `QuotaExhausted` | fall back |
| 403, 404, `model_not_found` | `Authorization`, `ModelNotFound` | fall back |
| 400 with `context_length_exceeded`, 413 | `ContextOverflow { needed, limit }` | fatal: shrink the prompt |
| 400, 409, 422 otherwise | `InvalidRequest` | fatal |
| `content_filter`, Azure's policy message | `ContentFilter` | fatal |
| A `refusal` field on the message | finish reason `Refusal` | not a failure at all |
| 5xx, a dropped connection | `Server`, `Transport` | retry |
| 408, a deadline that passes | `Timeout` | retry |

The two rows in bold are the ones **no status code can decide**. An expired token and a wrong key
are both 401s; an exhausted quota and a rate limit are both 429s, and the quota one usually carries
a `Retry-After` too. Reading the status alone gets both wrong in the expensive direction: a caller
that could have refreshed gives up, and a runtime sleeps through its own deadline waiting for a
balance only a human can top up. The signal is in the body, so that is where this adapter looks.

`Retry-After` is read in all three forms endpoints use: `retry-after-ms` (the precise one, and the
one that wins), seconds (whole or fractional), and an HTTP date. A date becomes a delay relative to
the response's own `Date` header when it has one, so a peer with a skewed clock still yields the
delay it meant, and a date already in the past clamps to zero rather than becoming a negative wait.

## Streaming

`stream()` reads server-sent events and emits the provider crate's normalized events **as they
arrive**: a test drives a hand-written chunked endpoint and asserts the first delta is delivered
before the last fragment has even been written, so a buffered answer released at the end cannot
pass for streaming. What comes out: text deltas in arrival order, tool calls reassembled **by index** with their id and name (buffered until the
name arrives, so the announcement always precedes its fragments), every open call closed before the
finish event, and usage last where the endpoint reports it. Reassembling that stream through
`turnframe_provider::stream::reconstruct` produces the same response `generate()` returns for the
same exchange, which the conformance suite asserts.

A stream that ends without a `finish_reason` and without `[DONE]` emits no finish at all, so the
accumulator reports a truncation. A silent `Stop` there would turn a dropped connection into a short
answer.

## Running the conformance suite

`tests/conformance.rs` runs the full suite of spec §20.8 against a wiremock server: the twenty
feature rows, plus thirteen per-status rows that prove each wire failure maps onto the one kind it
means and the retry class that kind must carry.

It runs six times over:

| Profile | Expectation |
| --- | --- |
| OpenAI | every row proven, nothing skipped |
| Azure OpenAI | the same rows through a deployment-shaped path and the `api-key` header |
| A modest `json_object`, tool-less gateway | passes while *skipping* the rows it cannot honestly claim |
| `native_function_schema` | every row proven through the forced-function transport |
| vLLM and `llama.cpp` | every row proven through each runtime's grammar |

A seventh run demonstrates the other half of the contract: a fixture that declares a row its
deployment cannot produce (an edge that answers 504 before an upstream 408 ever arrives), and the
report marks that row **unproven, with the reason**, rather than passing it over. A skipped row is
never a pass.

```bash
cargo test -p turnframe-provider-openai
```

To certify your own endpoint and model, point the factory in that file at your profile and run it.
If a structured-output row fails, lower the declaration; do not weaken the test.

## Optional live smoke tests

`tests/live_smoke.rs` makes two real calls against a real endpoint: one structured task, and one
streamed reply. Mocks cannot tell you whether the request this adapter builds is one the
vendor still accepts; these can.

```bash
TURNFRAME_OPENAI_LIVE_KEY=sk-… cargo test -p turnframe-provider-openai --test live_smoke -- --nocapture
```

They **never run in CI**: without the key they print a note and pass, and they skip outright when
`CI` is set. `TURNFRAME_OPENAI_LIVE_MODEL` and `TURNFRAME_OPENAI_LIVE_BASE_URL` point them at
another model or at any OpenAI-compatible gateway.

## More

See the workspace [README](../../README.md), the [provider adapter
guide](../../docs/provider-adapters.md),
[ADR-008](../../docs/adr/ADR-008-provider-capability-routing-and-no-silent-downgrade.md) and the
provider-neutral layer in [`turnframe-provider`](../turnframe-provider/README.md).

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
