# turnframe-provider-ollama

The Ollama adapter of [Turnframe](https://github.com/turnframe-rs/turnframe), speaking the daemon's
**native** chat endpoint.

## Two routes into Ollama, and why this one

Ollama answers on two surfaces, and both work:

| Route | Endpoint | Adapter |
| --- | --- | --- |
| **Native** | `POST /api/chat` | **this crate** |
| Compatibility | `POST /v1/chat/completions` | [`turnframe-provider-openai`](../turnframe-provider-openai/README.md), configured as a compatible profile |

If you have already standardized on the OpenAI-compatible path, you lose nothing by staying there.
This crate exists because the native route is better for three reasons that are not a matter of
taste.

- **The knobs are on it.** `options.num_ctx` is how you stop the daemon from silently truncating a
  prompt to its small default window, and `keep_alive` is how you stop it from unloading several
  gigabytes of weights between turns. Neither has a home in the compatibility layer, and both are
  the difference between a local model that works and one that mystifies.
- **The structured-output transport is stronger.** `format` takes a JSON Schema and constrains
  decoding against it. Going through the compatibility layer turns a schema transport into a weaker
  one for no reason.
- **The failures say what happened.** The native error body carries the daemon's own words, which is
  what lets a model that has not been pulled surface as `ModelNotFound` with a code that names the
  fix, rather than as an anonymous 404.

## A local runtime is not a small cloud

Three differences shape every decision in this crate.

**There is usually no credential.** `ollama serve` listens on `127.0.0.1:11434` and authenticates
nothing, so `bearer_token` is optional and the ordinary local profile carries none. A token is
accepted all the same, for the daemon behind an authenticating proxy or a hosted Ollama-compatible
runtime, which is the deployment where 401, an expired token and a spent balance are reachable
at all.

**It reports no cached tokens.** The daemon reuses its own KV cache between calls and says not a word
about it, so `usage.cached_input` is always zero and `prompt_caching` is not declared. Zero is read
by the contract as "not reported", which is exactly what is true here.

**It has no tool-call ids.** A tool call on this wire is a name and an arguments object. The ids come
from this adapter, every response carrying one is marked `SynthesizedCallIds`, `preserves_call_ids`
is false, and the builder refuses to let a profile claim otherwise.

## Capabilities are declared per **model**, never per Ollama

This is the sentence the crate is built around. The daemon is the same program whatever it is
serving: it will accept a JSON Schema in `format` for a 0.5B model exactly as it will for a 70B one,
and it will stream either. But a small instruction-tuned model routinely ignores a schema it was
told to satisfy, produces prose where a tool call was asked for, and has never seen an image in its
life.

So `declarations::baseline()` claims only what the **daemon** backs whatever it is running:

| Field | Baseline | Why |
| --- | --- | --- |
| `streaming` | **true** | `/api/chat` streams by default, for every model. |
| `structured_output` | `JsonObject` | `format: "json"` constrains any model to valid JSON *syntax*. It is deliberately not `NativeJsonSchema`: the daemon takes a schema happily, and whether the model honours it is what a conformance run measures. |
| `preserves_call_ids` | **false** | Structural. There is no id on the wire to preserve. |
| everything else | off | Tools, vision, the context window: a model earns each one. |

Everything above the baseline is raised with the `with_*` methods of `ProviderCapabilities`, and the
only thing that licenses raising one is a passing conformance run **against that model**. A report
for `qwen3:8b` says nothing about `smollm2:135m`, and this crate will not claim it on their behalf.

```rust
use turnframe_provider::capabilities::{StructuredOutputCapability, ToolCallingCapability};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::secret::ApiKey;
use turnframe_provider_ollama::{OllamaProvider, declarations::baseline};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
// The ordinary case: a daemon on this machine, no credential, nothing claimed
// on an unmeasured model's behalf.
let small = OllamaProvider::local().model("smollm2:135m").build()?;
assert_eq!(small.endpoint(), "http://127.0.0.1:11434/api/chat");
assert!(small.key_fingerprint().is_none());
// `json_object` is not admitted for the understanding tasks, and the
// profile says so rather than being talked into it.
assert!(small.supports(&ModelPurpose::Extract.requirements()).is_err());

// A model that earned its declaration by passing the suite against it.
let measured = OllamaProvider::local()
    .model("qwen3:8b")
    .capabilities(
        baseline()
            .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
            .with_tool_calling(ToolCallingCapability::Parallel)
            .with_max_context_tokens(32_768),
    )
    .keep_alive("30m")
    .build()?;
assert!(measured.supports(&ModelPurpose::Extract.requirements()).is_ok());

// The same daemon behind an authenticating proxy.
let remote = OllamaProvider::at("https://ollama.internal")
    .bearer_token(ApiKey::new("not-a-real-token"))
    .model("qwen3:8b")
    .region("on-premise")
    .build()?;
assert!(remote.key_fingerprint().is_some());
# Ok(())
# }
```

### `num_ctx` is part of the declaration

Ollama defaults a model's context window to a small value and **silently truncates** anything
longer. A profile that declared `max_context_tokens` without telling the daemon to allocate that
window would be declaring something untrue, so the declared window travels as `options.num_ctx` on
every request. `context_tokens(…)` overrides it when the two must differ.

## Running against a local daemon

```bash
ollama serve                 # if it is not already running as a service
ollama pull qwen3:8b         # the tag must exist on this machine
curl -s http://127.0.0.1:11434/api/tags | head   # confirms it is up
```

Then point the builder at it. `OllamaProvider::local()` is the default host and port;
`OllamaProvider::at("http://gpu-01.internal:11434")` is another machine.

Two things bite on the first afternoon:

- **`connection refused`** means the daemon is not running. The adapter says so in the error code
  itself (`transport … code=ollama_unreachable_is_the_daemon_running`) rather than leaving you
  reading your network configuration.
- **The first call is slow.** A cold model is several gigabytes read off disk before a token
  appears. Give the builder's `timeout` room for it, and use `keep_alive` so the next turn does not
  pay it again.

## What travels on the wire

| Normalized | Ollama |
| --- | --- |
| `system` prompt | a leading `system` message |
| messages, roles | `messages[].role`, `messages[].content` |
| images | `messages[].images`, bare base64; a **URL** image is refused, because the daemon fetches nothing |
| documents | nothing: `/api/chat` has one attachment channel and it is `images`, so a document part is refused rather than sent as a picture of a PDF |
| tool declarations | `tools[]`, the function shape |
| tool calls | `messages[].tool_calls[].function`, arguments as an **object** |
| tool results | a `tool` message with `tool_name`, correlated by name because there is no id |
| a failed tool result | prefixed `[tool_error] `, since the format has no error flag |
| `OutputSpec::Json`, `NativeJsonSchema` | `format: {…the schema…}` |
| `OutputSpec::Json`, `JsonObject` | `format: "json"`, schema described in the system prompt |
| `temperature`, `stop`, `max_output_tokens` | `options.temperature`, `options.stop`, `options.num_predict` |
| declared context window | `options.num_ctx` |
| usage | `prompt_eval_count`, `eval_count`; `cached_input` is always 0 |

Dropped and reported as a `FeatureDropped` warning: a `tool_choice` of `required` or a named tool
(the endpoint has no field for it), a cache hint, and request metadata. `ToolChoice::None` is
expressed exactly, by declaring no tools at all.

## Streaming

`stream()` reads Ollama's **newline-delimited JSON**, not server-sent events: one complete chat
object per line, the last carrying `"done": true` and the token counts. Text arrives as
`message.content` on each frame and is emitted as it lands, so an adopter sees prose appear rather
than a lump at the end. A tool call arrives whole, because the runner parses it before sending, and
is announced, filled and closed within one frame.

Reassembling that stream through `turnframe_provider::stream::reconstruct` produces the same
response `generate()` returns for the same exchange, **counts included**: the counts ride on the
final frame, so an adapter that dropped it would report no usage at all.

A body that ends without a `"done": true` frame emits no finish event, so the accumulator reports a
truncation; a body cut in the middle of a frame fails as `stream_ended_mid_frame`. A silent `Stop`
in either case would turn a dropped connection into a short answer.

## Errors

| On the wire | Normalized | Retry class |
| --- | --- | --- |
| 404 `model "x" not found, try pulling it first` | `ModelNotFound`, code `model_not_pulled` | fall back |
| connection refused | `Transport`, code `ollama_unreachable_is_the_daemon_running` | retry |
| a message naming the context window or size | `ContextOverflow { needed, limit }` | fatal: shrink the prompt or raise `num_ctx` |
| 400 the daemon rejected | `InvalidRequest` | fatal |
| 500 `llama runner process has terminated` | `Server` | retry |
| a deadline that passes | `Timeout` | retry |
| 401 (a proxy) | `Authentication` | fall back |
| 401 saying the token **expired** (a proxy) | `CredentialExpired` | fall back, and refresh if you hold a refresher |
| 429 with `Retry-After` (a proxy) | `RateLimited { retry_after }` | retry after the delay |
| 429 or 402 naming a **balance or quota** (a proxy) | `QuotaExhausted { scope }` | fall back: waiting refills nothing |
| a policy gateway's block | `ContentFilter` | fatal |

No response body, no header and no prompt reaches a `ProviderError`. What survives a failure is a
typed kind, a short sanitized code (passed through a redactor seeded with the configured token
first), and, for a context overflow, the two token counts the message named.

## Running the conformance suite

`tests/conformance.rs` runs the full thirty-three-row suite of spec §20.8 against a wiremock server
speaking `/api/chat`, three times:

| Run | Result |
| --- | --- |
| A **bare local daemon**, no credential | 25 proven, 0 failed, **8 unproven** |
| The same daemon **behind an authenticating proxy** | 33 proven, 0 failed, 0 unproven |
| A **small unmeasured model** on the bare daemon | 23 proven, 0 failed, 10 unproven |

The eight unproven rows of the first run are everything a thing standing *in front of* the runner
would have produced: the feature rows `authentication_failure`, `rate_limit` and `refusal`, and the
per-status rows `status_401_authentication`, `expired_credential_kind`, `quota_exhausted_kind`,
`status_429_rate_limited` and `content_filter_kind`. Each is declared through
`RowSupport::not_producible` **with a reason** (`feature_support` for the first three,
`status_support` for the rest): a daemon that authenticates nothing cannot answer 401, holds no
credential that can expire, meters no quota, queues instead of rate-limiting, and inspects no
prompt. They are not skipped silently and they are not claimed: the adapter maps all eight, and the
proxied run is what proves it.

The small-model run adds `tool_and_read_request_ids` and `no_silent_capability_downgrade`, which a
`json_object`, tool-less declaration cannot honestly claim. Skipping them is why that run passes: a
skipped row is not a pass, it is a row the adapter is unproven on, and honesty is not a failure. The
three streaming rows stay proven in every run, because streaming belongs to the daemon rather than
to the model.

```bash
cargo test -p turnframe-provider-ollama
cargo test -p turnframe-provider-ollama -- --nocapture   # prints the compatibility table
```

To certify **your** model, point the factory in that file at it and run it. If a structured-output
row fails, lower the declaration; do not weaken the test.

## Optional live smoke tests

`tests/live_smoke.rs` makes two real calls against a real daemon: one structured task, and one
streamed reply. Mocks cannot tell you whether the request this adapter builds is one the
version you are running accepts; these can.

```bash
TURNFRAME_OLLAMA_LIVE_MODEL=qwen3:8b cargo test -p turnframe-provider-ollama --test live_smoke -- --nocapture
```

They **never run in CI**: without a model they print a note and pass, and they skip outright when
`CI` is set. The switch is the model rather than a credential because an ordinary `ollama serve` has
none, and because the daemon only runs what someone pulled. `TURNFRAME_OLLAMA_LIVE_BASE_URL` points
them at another daemon and `TURNFRAME_OLLAMA_LIVE_TOKEN` at one behind an authenticating proxy.

A green run says the daemon accepted the request. It says nothing about the model: raising a
declaration is what the conformance suite is for, per model.

## More

See the workspace [README](../../README.md), the [provider adapter
guide](../../docs/provider-adapters.md),
[ADR-008](../../docs/adr/ADR-008-provider-capability-routing-and-no-silent-downgrade.md) and the
provider-neutral layer in [`turnframe-provider`](../turnframe-provider/README.md).

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
