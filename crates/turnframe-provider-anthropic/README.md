# turnframe-provider-anthropic

The Anthropic Messages API adapter of [Turnframe](https://github.com/turnframe-rs/turnframe).

One wire format: `POST {base}/v1/messages`, authenticated with the `x-api-key` header and pinned to
a dated `anthropic-version`. Anthropic's own API is the profile that ships configured; anything else
that reimplements the same surface (a gateway, an internal proxy, a vendor that publishes an
Anthropic-compatible endpoint) is a `compatible` profile with a declaration you measured yourself.

## Structured output is a forced tool, and that is the honest answer

This is the sentence the crate is built around. The Messages API has **no** `response_format`, no
JSON mode and no grammar. The only way to make the endpoint enforce a schema is to declare one tool
whose `input_schema` *is* the required schema and pin `tool_choice` to it. The model's `tool_use`
block is then validated against that schema before it is emitted, and the runtime reads the block's
input as the document.

That is exactly what [spec §20.4](../../docs/provider-adapters.md) admits as
`NativeFunctionSchema`: *a native function schema used only as a structured-output transport, not
direct execution*. The forced call is never executed, never reaches a command handler and never
authorizes anything: it is a shipping container for JSON that the provider type-checks on the way
out.

```json
{
  "tools": [{ "name": "user_turn_plan", "description": "…", "input_schema": { "…your schema…" } }],
  "tool_choice": { "type": "tool", "name": "user_turn_plan", "disable_parallel_tool_use": true }
}
```

**Its honest limits**, all of which the builder or the conversion enforce rather than merely
document:

- **`NativeJsonSchema`, `JsonObject` and `GrammarConstrained` are refused at build time.** They name
  transports this wire format does not have. There is no other constructor, so no provider instance
  can exist claiming one.
- **`NativeFunctionSchema` cannot be declared with `tool_calling: none`.** The transport *is* a tool
  call, so the pair is a contradiction the builder will not assemble.
- **While the choice is pinned, the model cannot call a read tool in the same turn.** A stage that
  wants reads asks for `OutputSpec::ToolCalls` and gets the ordinary tool loop; a stage that asks for
  a document gets a document. When a request does both, the transport wins and the response carries
  a `FeatureDropped { feature: "tool_choice" }` warning saying so.
- **The schema must be an object schema.** `input_schema` is a tool's parameter schema; a top-level
  `array` or `string` schema is rejected by the endpoint, not silently wrapped.
- **A synthetic tool name is sanitized and de-collided.** The API takes `^[a-zA-Z0-9_-]{1,64}$`, so
  anything else becomes `_`, and a name a declared read tool already owns gets an `_output` suffix.
- The weaker `PromptOnly` transport is available and describes the schema in the system prompt. It is
  **not** admitted for the understanding tasks, and the router refuses to downgrade to it.

## Building it

```rust
use turnframe_provider::prelude::*;
use turnframe_provider_anthropic::{AnthropicProvider, AuthScheme, EndpointProfile};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
// Anthropic: forced-tool schema enforcement, parallel tools, vision, documents, streaming,
// prompt caching, ids that round-trip.
let anthropic = AnthropicProvider::anthropic()
    .api_key(ApiKey::new("sk-ant-not-a-real-key"))
    .model("claude-sonnet-4-5-20250929")
    .build()?;
assert_eq!(anthropic.endpoint(), "https://api.anthropic.com/v1/messages");

// A proxy: your base URL, your credential shape, the declaration you measured.
let proxy = AnthropicProvider::builder(
    EndpointProfile::compatible("claude-proxy").with_auth(AuthScheme::Bearer),
)
    .api_key(ApiKey::new("not-a-real-token"))
    .base_url("https://claude.internal")
    .model("claude-sonnet-4-5")
    .capabilities(
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
            .with_tool_calling(ToolCallingCapability::Parallel)
            .with_streaming(true),
    )
    .build()?;
assert!(proxy.capabilities().structured_output.enforces_schema());
# Ok(())
# }
```

The builder also takes an `anthropic-version` override, `anthropic-beta` flags, extra headers for
gateways that want one, a transport timeout, per-million-token costs, a region label and routing
tags. The effective deadline of a call is the smaller of the builder's timeout and the request's own,
so a caller can always ask for less time and never for more.

`EndpointProfile::anthropic()` is a **default, not a certificate**. It describes the Claude models
this crate was written against; a model with a smaller window, without tool support or behind a
proxy that strips a feature is configured with `.capabilities(…)`, and the only thing that licenses
what you write there is a passing conformance run against that endpoint and that model. Each profile
also carries a ceiling that only ever lowers, so a profile written for an unmeasured endpoint cannot
be talked into claiming schema enforcement.

## What this wire format does differently

| | Messages API | How the adapter handles it |
| --- | --- | --- |
| The system prompt | a top-level field, not a message | a `Role::System` message is hoisted into it, joined after `request.system` |
| A tool result | a block in the **user** turn | `Role::Tool` becomes `user`, and results are ordered first in the turn |
| A failed tool result | a real `is_error` flag | no marker prefix is invented |
| Consecutive same-role turns | merged by the API anyway | merged here, so the body is deterministic |
| `max_tokens` | **required** | the caller's, or the profile's `default_max_output_tokens` |
| Attachments | separate `image` and `document` blocks | the normalized part decides: `ContentPart::Image` is an image block, `ContentPart::Document` is a document block, and both forms of a document state their media type so nothing is read off a URL's suffix |
| Prompt caching | `cache_control` breakpoints | `CacheHint::System` marks the system block, `CacheHint::Prefix` marks the end of the turn its last message landed in |
| Thinking blocks | `thinking`, `thinking_delta` | dropped on both paths: reasoning is not prose a user may be shown |

## Usage, and what "input" means

Anthropic reports four counters and its `input_tokens` **excludes** the cached ones. `TokenUsage`
documents the opposite: `cached_input` is already counted in `input`. So the normalization adds both
cache counters back into `input` and reports only the *read* half as `cached_input`: creation tokens
were not served from a cache, they filled one, and they cost more rather than less.

```text
input        = input_tokens + cache_creation_input_tokens + cache_read_input_tokens
cached_input = cache_read_input_tokens
output       = output_tokens
```

## What never leaves

- The credential is an `ApiKey` from `turnframe-provider`, reaches the wire as a header value marked
  sensitive, and appears in no `Debug` output. What `Debug` shows instead is an eight-character
  fingerprint that identifies *which* key is configured without revealing it, plus the header names.
- The base URL may not carry `user:password@`, and an extra header may not claim `authorization`,
  `x-api-key`, `api-key`, `anthropic-version` or `anthropic-beta`. A credential travels in the
  credential slot.
- No response body, no header and no prompt reaches a `ProviderError`. What survives a failure is a
  typed kind, a short sanitized code the endpoint's own `error.type` suggested (passed through a
  redactor seeded with the configured key first), and, for a context overflow, the two token counts
  the message named.

## Errors

| On the wire | Normalized | Retry class |
| --- | --- | --- |
| `invalid_request_error`, 400 | `InvalidRequest` | fatal |
| `invalid_request_error` naming a prompt that is too long, `request_too_large` (413) | `ContextOverflow { needed, limit }` | fatal: shrink the prompt |
| `authentication_error`, 401 | `Authentication` | fall back |
| …whose message says the key expired, was revoked or was disabled | `CredentialExpired` | fall back: refresh, do not retry |
| `permission_error`, 403 | `Authorization` | fall back |
| `not_found_error`, 404 | `ModelNotFound` | fall back |
| `rate_limit_error`, 429 with `Retry-After` | `RateLimited { retry_after }` | retry after the delay |
| A spent balance or spend limit, **on a 400 or a 429** | `QuotaExhausted { scope }` | fall back: waiting will not refill it |
| `timeout_error`, 408 | `Timeout` | retry |
| `api_error`, 500, 503 | `Server` | retry |
| `overloaded_error`, 529 | `Server` | retry |
| A message blocked by the content policy | `ContentFilter` | fatal |
| `stop_reason: "refusal"` | finish reason `Refusal` | not a failure at all |
| A dropped connection | `Transport` | retry |

Two rows are the ones a status code cannot decide, and both are recognized from the message
**before** the status is consulted: the API returns an empty balance as a `429` as readily as a
`400`, and an expired key as the same `401` as a wrong one. Reading either from the status alone
makes the runtime wait out a delay that changes nothing, or tells a caller holding a refresher that
its key is simply bad.

## Streaming

`stream()` reads server-sent events and emits the provider crate's normalized events: text deltas in
arrival order; a tool call announced from `content_block_start`, which already carries both its id
and its name, so nothing has to be buffered; `input_json_delta` fragments forwarded verbatim and
concatenated per call by the accumulator; every open call closed before the finish; and usage
**merged** across `message_start` and `message_delta` and emitted once, because the two halves
separately would not equal what the non-streamed call reports.

Reassembling that stream through `turnframe_provider::stream::reconstruct` produces the same response
`generate()` returns for the same exchange, which the conformance suite asserts, and which the
crate's own tests assert field for field, usage included.

A stream that ends without `message_delta` and without `message_stop` emits no finish at all, so the
accumulator reports a truncation. A silent `Stop` there would turn a dropped connection into a short
answer. An `error` frame mid-stream is classified by its own type and ends the stream as a typed
failure.

## Running the conformance suite

`tests/conformance.rs` runs the full suite of spec §20.8 (the twenty feature rows and the
thirteen per-status rows) against a wiremock server, three times:

| Run | Expectation |
| --- | --- |
| The Anthropic profile, fully declared | **30 passed, 0 failed, 0 skipped**: nothing may be skipped, because a skipped row is not a pass |
| A measured proxy: bearer auth, its own base URL | the same rows, through a different surface |
| A modest `prompt_only`, tool-less profile answered with JSON in a text block | 27 passed, 0 failed, **3 skipped**: tool ids, streaming reconstruction and no-silent-downgrade are rows it cannot honestly claim |

```bash
cargo test -p turnframe-provider-anthropic
```

To certify your own endpoint and model, point the factory in that file at your profile and run it. If
a structured-output row fails, lower the declaration; do not weaken the test.

## Optional live smoke tests

`tests/live_smoke.rs` makes two real calls against a real endpoint: one structured task through the
forced tool, and one streamed reply. Mocks cannot tell you whether the request this
adapter builds is one the vendor still accepts; these can.

```bash
TURNFRAME_ANTHROPIC_LIVE_KEY=sk-ant-… cargo test -p turnframe-provider-anthropic --test live_smoke -- --nocapture
```

They **never run in CI**: without the key they print a note and pass, and they skip outright when
`CI` is set. `TURNFRAME_ANTHROPIC_LIVE_MODEL` and `TURNFRAME_ANTHROPIC_LIVE_BASE_URL` point them at
another model or at any endpoint reimplementing the Messages API.

## Not covered

Anthropic models served through **Vertex AI** and **AWS Bedrock** speak the same message body behind
a different route and a different credential: Bedrock has its own crate
(`turnframe-provider-bedrock`), and Vertex is not implemented here. Server-side tools (web search,
code execution), the batch API, the files API and the extended-thinking budget controls are not sent;
a `thinking` block that arrives anyway is dropped with a `FeatureDropped` warning rather than folded
into the answer.

## More

See the workspace [README](../../README.md), the [provider adapter
guide](../../docs/provider-adapters.md),
[ADR-008](../../docs/adr/ADR-008-provider-capability-routing-and-no-silent-downgrade.md) and the
provider-neutral layer in [`turnframe-provider`](../turnframe-provider/README.md).

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
