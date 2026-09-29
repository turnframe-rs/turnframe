# turnframe-provider-gemini

The Google Gemini adapter of [Turnframe](https://github.com/turnframe-rs/turnframe).

One crate serves two surfaces that reach the same models and agree on almost nothing else.

| | Gemini developer API | Vertex AI |
| --- | --- | --- |
| Host | `generativelanguage.googleapis.com` | `{location}-aiplatform.googleapis.com` |
| Path | `/v1beta/models/{model}:generateContent` | `/v1/projects/{p}/locations/{l}/publishers/google/models/{m}:generateContent` |
| Credential | a long-lived API key in `x-goog-api-key` | a short-lived OAuth token in `Authorization: Bearer` |
| Where it comes from | your configuration | a `TokenSource`, consulted once per request |
| `labels` | rejected | accepted |
| Provider key | `gemini` | `vertex-ai` |

They share a request body, and that is where the sharing stops. Both are `EndpointProfile`s of the
same adapter because the *translation* is identical; the plumbing is not.

## Building each profile

```rust
use std::sync::Arc;
use turnframe_provider::prelude::*;
use turnframe_provider_gemini::GeminiProvider;
use turnframe_provider_gemini::credential::{TokenError, TokenFn};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
// The developer API: an API key, and nothing else to arrange.
let gemini = GeminiProvider::gemini()
    .api_key(ApiKey::new("AIza-not-a-real-key"))
    .model("gemini-2.5-flash")
    .build()?;
assert!(gemini.endpoint().ends_with("/models/gemini-2.5-flash:generateContent"));

// Vertex AI: one project, one region, and a token you already know how to get.
let vertex = GeminiProvider::vertex_ai("aurora-prod", "europe-west4")
    .token_source(Arc::new(TokenFn::new(|| async {
        std::env::var("VERTEX_ACCESS_TOKEN")
            .map(ApiKey::new)
            .map_err(|_| TokenError::failed("env_var_missing"))
    })))
    .model("gemini-2.5-flash")
    .build()?;
assert!(vertex.endpoint().contains("/projects/aurora-prod/locations/europe-west4/"));
// The location is the data residency, so region-aware routing gets it for free.
assert_eq!(vertex.profile().region.as_deref(), Some("europe-west4"));
# Ok(())
# }
```

The builder also takes a quota project (sent as `x-goog-user-project`), extra headers for a proxy,
safety settings, a thinking budget, a transport timeout, per-million-token costs and routing tags.
The effective deadline of a call is the smaller of the builder's timeout and the request's own, so a
caller can always ask for less time and never for more.

## The Vertex credential is yours, and that is deliberate

This crate embeds **no Google authentication library**. It takes an access token you already hold,
through a `TokenSource`: a small async trait, with `TokenFn` wrapping a closure and `StaticToken`
wrapping a fixed value. Four reasons:

- **Acquiring a Google credential is an ambient concern of the deployment, not of the model call.**
  On GKE and Cloud Run it is the metadata server, in CI it is workload identity federation, on a
  laptop it is application default credentials, and in a regulated fleet it is a broker the security
  team owns. An adapter that picked one would impose a deployment policy from inside a translation
  layer.
- **Spec §25.2 puts external credentials outside the model layer.** This adapter never reads a
  service-account key, never signs a JWT, and never touches the filesystem or the metadata endpoint.
  Keeping acquisition outside is what makes that structural rather than intended.
- **Refresh policy belongs to you.** How early to renew, whether one token is shared across a fleet
  of provider instances, and what happens when the broker is down are fleet decisions, invisible
  from here.
- **Nobody pays for what they do not use.** A build that only speaks the developer API would still
  carry a cloud authentication tree, and usually a second TLS stack with it.

The source is consulted **once per request**, immediately before dispatch, so a source that caches
and refreshes keeps a long-running process authenticated with no further ceremony. When a token
lapses anyway, the failure is `CredentialExpired` and not `Authentication`; see *Errors* below.

## Two ways to enforce a schema, and both are offered

This endpoint has two, and a profile picks one by what it declares:

| Declaration | What goes on the wire | When to reach for it |
| --- | --- | --- |
| `native_json_schema` | `generationConfig.responseSchema` with `responseMimeType: "application/json"` | the default; the answer arrives as text |
| `native_function_schema` | one `functionDeclarations` entry whose `parameters` is the schema, with `toolConfig.functionCallingConfig` pinned to `ANY` and that single name | a surface that refuses a response schema and `tools` in the same request, or a router that also routes to an adapter whose wire format has only the function form |

The forced call is never executed, never reaches a command handler and never authorizes anything: it
is a shipping container for JSON, and the runtime reads its arguments as the document. The parsed
answer is identical either way, which is the whole point: a stage can move between providers without
its contract changing. Two consequences the builder enforces: `grammar_constrained` is still refused,
and `native_function_schema` cannot be declared alongside `tool_calling: none`, because the transport
*is* a function call. While the choice is pinned the model cannot call a read tool in the same turn;
a stage that wants reads asks for `OutputSpec::ToolCalls` instead.

Whichever transport carries it, the schema is translated into Gemini's dialect first, so the
refusals below apply to both.

## The schema subset, and what a rejected schema looks like

A `responseSchema` with `responseMimeType: "application/json"` is a real, enforced schema transport,
so the profile declares `NativeJsonSchema` and means it. But the wire type is a restricted OpenAPI
3.0 `Schema`, not JSON Schema, and Google's REST layer rejects a body carrying a field its proto
does not declare.

`turnframe_provider_gemini::schema::translate_schema` carries across everything with an equivalent
and **fails on everything else**, naming the keyword and the JSON pointer. It never sends a weaker
schema. That refusal is the safety property of this crate: a silently narrowed schema under a
`NativeJsonSchema` declaration leaves the router still admitting the profile for the understanding
tasks ([ADR-008](../../docs/adr/ADR-008-provider-capability-routing-and-no-silent-downgrade.md))
while the guarantee everything downstream rests on has quietly stopped being true.

| Source | Becomes | Why |
| --- | --- | --- |
| `"type": "string"` | `"type": "STRING"` | Gemini spells types in upper case. |
| `"type": ["string", "null"]` | `"type": "STRING", "nullable": true` | The dialect has a flag, not a union. |
| `"const": "x"` | `"enum": ["x"]` | Exactly equivalent, and `enum` exists. |
| `"properties"` | same, plus `propertyOrdering` | Deterministic decoding; the set of keys is unchanged. |
| `"additionalProperties": false` | *dropped* | Gemini's decoder emits only declared properties, so the constraint already holds. |
| `$schema`, `$id`, `examples`, `readOnly`, … | *dropped* | Annotations that constrain nothing. |
| an unrecognized `format` | *dropped* | Gemini would reject the request and would not have enforced it anyway. |
| `$ref` into `$defs` | the definition, inlined | The dialect has no references, and deleting one loses whatever it referenced. |
| a provably disjoint `oneOf` | `anyOf` | Proven equivalent before the rewrite, never assumed. |
| a `oneOf` of literals | one `enum`, each variant's sentence folded into the description | An enum constrains harder, and the documentation survives. |
| an unresolvable or recursive `$ref`, an unprovable `oneOf`, `allOf`, `not`, `if`, `patternProperties`, `multipleOf`, `uniqueItems`, `exclusiveMinimum`, tuple `items`, `additionalProperties` as a schema | **refused** | Each one narrows or widens the accepted set in a way the dialect cannot carry. |

Two of those rows deserve their reasoning spelled out.

**References are inlined rather than deleted.** Deleting them is the move that suggests itself, and
it produces a schema that still looks like a schema while no longer constraining anything it
referenced. A field whose type was an enum of four operations becomes free text, the model invents a
fifth, and the turn dead-ends on a name nothing can compile. Inlining is exact for an acyclic
schema; a recursive one is refused, because substituting it does not terminate.

**`oneOf` is proven, not renamed.** `oneOf` means *exactly one* branch matches and `anyOf` means *at
least one*, so a value matching two branches is accepted by the second and rejected by the first.
Renaming one to the other unconditionally is precisely the silent weakening this module exists to
prevent. But the two accept the same set when no document can match two branches, and that is the
normal case for a schema derived from a Rust enum, where every variant pins a distinct tag. So the
exclusivity is **proved** (from the branches' constants, their types, or a shared discriminant),
and the rewrite happens only where the proof goes through. A union that cannot be proved is refused,
naming the pointer.

**How a rejected schema surfaces.** Two ways, and you want the first:

```rust
use serde_json::json;
use turnframe_provider_gemini::translate_schema;

// At start-up, so a deployment fails instead of a turn.
let refused = translate_schema(&json!({
    "type": "object",
    "properties": {"act": {"oneOf": [{"type": "object"}, {"type": "object"}]}}
}))
.expect_err("two open objects can both match, so the union cannot be proved");
assert_eq!(refused.keyword(), "oneOf");
assert_eq!(refused.pointer(), "/properties/act");
```

At call time it is a `ProviderError` of kind `Unsupported`, rendering as
`unsupported(response_schema:oneOf) [gemini/…] code=unsupported_value:_properties_act`, with retry
class **`Fallback`**, not `Fatal`. The declaration `NativeJsonSchema` is still true; this
provider-model pair simply cannot enforce *this* schema, so the router may offer another candidate
that satisfies the same requirements, which is not a downgrade. Nothing reaches the wire.

## Wire conversion

**Framing instructions are a field.** `systemInstruction` sits beside `contents`; there is no
`system` role. A `system`-role message inside the conversation is hoisted into that field and its
*position* is reported as dropped, because putting framing instructions into a `user` turn is the
prompt-injection shape [spec §25.3](../../docs/threat-model.md) warns about.

**The role vocabulary is different, and short.** Gemini knows `user` and `model`.

| Normalized | Gemini | Note |
| --- | --- | --- |
| `User` | `user` | |
| `Assistant` | `model` | |
| `Tool` | `user` | A function *response* is something the caller tells the model. |
| `System` | none | Hoisted into `systemInstruction`. |

Consecutive contents of the same role are merged, because Gemini expects the two to alternate and
merging preserves both order and content.

**Tool calls are parts, not messages.** A `functionCall` is a part of a `model` content and a
`functionResponse` is a part of a `user` content. Text arrives as `text` parts, inline bytes as
`inlineData`, and a URI as `fileData`; note that Gemini does not fetch arbitrary `http(s)` URLs, so
a URL source is expected to be a Cloud Storage object or a Files API resource.

**A call has no id on this wire.** Gemini's `functionCall` carries a name and arguments; the REST
surface usually omits the optional `id`. So the default profile declares `preserves_call_ids: false`,
the adapter synthesizes stable positional ids (`call_0`, `call_1`, …) identically on the streamed and
non-streamed paths, and every response carrying one says so with a `SynthesizedCallIds` warning. A
`functionResponse` is therefore addressed by **name**, recovered from the call it answers; a result
whose call is nowhere in the conversation is a loud `invalid_request` rather than a guess, because
answering a question the model did not ask is worse than refusing the turn.

## Usage

`promptTokenCount` already **includes** `cachedContentTokenCount`, which is exactly what
`TokenUsage` documents (`cached_input` is "already counted in `input`"), so the two map across with
no arithmetic. `thoughtsTokenCount` is the one that needs some: it sits *outside*
`candidatesTokenCount` in the response and *inside* the output tokens on the bill, so the
normalized `output` is the sum and `reasoning` is the thinking half of it. A cost estimate built on
`candidatesTokenCount` alone would under-report every reasoning call.

## Errors

Google carries the meaning in `error.status`, its gRPC canonical name, which is more specific than
the HTTP status it rides on.

| On the wire | Normalized | Retry class |
| --- | --- | --- |
| `RESOURCE_EXHAUSTED`, per-minute quota | `RateLimited { retry_after }` | retry after the delay |
| `RESOURCE_EXHAUSTED`, per-day quota or a billing message; HTTP 402 | `QuotaExhausted { scope }` | fall back |
| `UNAUTHENTICATED` whose message or `WWW-Authenticate` says expired | `CredentialExpired` | fall back: refresh and retry |
| `UNAUTHENTICATED` otherwise | `Authentication` | fall back |
| `PERMISSION_DENIED`, `FAILED_PRECONDITION` | `Authorization` | fall back |
| `INVALID_ARGUMENT` describing a too-long prompt; HTTP 413 | `ContextOverflow { needed, limit }` | fatal: shrink the prompt |
| `INVALID_ARGUMENT` otherwise | `InvalidRequest` | fatal |
| `NOT_FOUND` | `ModelNotFound` | fall back |
| `UNAVAILABLE`, `INTERNAL`, `ABORTED` | `Server { status }` | retry |
| `DEADLINE_EXCEEDED`; a deadline that passes | `Timeout` | retry |
| `promptFeedback.blockReason`; a candidate finishing as `SAFETY`, `RECITATION`, `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII` | `ContentFilter` | fatal |

Two of those rows are splits a status code cannot make, and both matter more here than for most
providers.

**An expired token is not a bad token.** Vertex's credential expires by design, so a 401 in a
long-running process is routine and recoverable. Folding it into a generic authentication failure
would tell a caller its key is bad when a refresh would have fixed the call. The two are told apart
by the message and the `WWW-Authenticate` challenge; where neither says, the conservative reading
wins and the failure stays `Authentication`, because claiming expiry over a wrong key invites a
refresh loop. The documented remedy is the `TokenSource`: it is consulted per request, so refreshing
and retrying the same logical call is enough.

**A rate limit is not an exhausted quota.** `RESOURCE_EXHAUSTED` covers both and they want opposite
handling. They are told apart by the `QuotaFailure` detail, whose `quotaId` names the window
(`…PerMinute…` against `…PerDay…`), and by a message naming billing or a credit balance. Where the
response carries neither (which happens, since the detail is optional), the classification stays
`RateLimited`: its `RetryAfter` class still permits moving to another candidate, so a misread hard
quota costs one delay, whereas a misread rate limit would abandon a healthy provider outright.

Safety is the third: a blocked answer is a `ContentFilter` and never a generic failure, because
`ContentFilter` is `Fatal` and a generic failure would be retried, and retrying elsewhere until a
model complies is a safety bypass, not a recovery. A blocked candidate carrying no content is a
failure; one carrying a prefix is a response finishing as `ContentFilter`, which `is_complete()`
already refuses to let a structured stage parse.

**What never leaves.** No response body, no header and no prompt reaches a `ProviderError`. What
survives a failure is a typed kind, a short sanitized code built from Google's canonical status
(passed through a redactor seeded with the configured credential first), and, for a context overflow,
the two token counts the message named. The credential appears in no `Debug` output: what `Debug`
shows is an eight-character fingerprint identifying *which* key is configured, the token source's
own redacted rendering, and the header names. The developer API's `?key=` URL form is deliberately
not implemented, and a base URL carrying one is refused at build time, because a URL is the one part
of a request that reliably reaches an access log.

## Streaming

`stream()` reads `streamGenerateContent?alt=sse` and emits the provider crate's normalized events.
Gemini delivers a function call **whole** inside one chunk rather than slicing its arguments, so a
call is announced, filled and closed at once, in arrival order; text deltas are concatenated into the
single part the non-streamed path builds; every open call is closed before the finish event, and
usage follows it.

Gemini sends **no `[DONE]` sentinel**, so the `finishReason` is the only evidence an answer is
complete. A stream that ends without one emits no finish at all and the accumulator reports a
truncation: a silent `Stop` there would turn a dropped connection into a short answer.

Reassembling through `turnframe_provider::stream::reconstruct` produces the same response
`generate()` returns for the same exchange, which the conformance suite asserts on both profiles.

## Running the conformance suite

`tests/conformance.rs` runs the full suite of spec §20.8 (the twenty feature rows plus the
thirteen per-status rows) against a wiremock server, three times:

| Run | Result |
| --- | --- |
| `gemini/gemini-2.5-flash` | 30 passed, 0 failed, **0 skipped** |
| `vertex-ai/gemini-2.5-flash` | 30 passed, 0 failed, **0 skipped** |
| a modest `json_object`, tool-less profile | 28 passed, 0 failed, 2 skipped |

The two full profiles prove **every** row with nothing skipped. The modest one passes while
*skipping* `tool_and_read_request_ids` and `no_silent_capability_downgrade`, the two rows a
tool-less `json_object` declaration cannot honestly claim, because a skipped row is not a pass, it
is a row the adapter is unproven on, and the compatibility table renders it as such.

```bash
cargo test -p turnframe-provider-gemini
# The runs print their whole table, so the evidence lands in the CI log.
cargo test -p turnframe-provider-gemini --test conformance -- --nocapture
```

To certify your own model and region, point the factory in that file at your profile and run it. If
a structured-output row fails, lower the declaration; do not weaken the test. **Conformance is per
provider-model pair**: a passing run against `gemini-2.5-flash` on the developer API says nothing
about the same id on Vertex in another region, and nothing at all about `gemini-2.5-pro`.

## Optional live smoke tests

`tests/live_smoke.rs` makes two real calls against a real endpoint: one structured task, and one
streamed reply. Mocks cannot tell you whether the request this adapter builds is one Google
still accepts; these can.

```bash
TURNFRAME_GEMINI_LIVE_KEY=AIza… cargo test -p turnframe-provider-gemini --test live_smoke -- --nocapture
```

They **never run in CI**: without the key they print a note and pass, and they skip outright when
`CI` is set. `TURNFRAME_GEMINI_LIVE_MODEL` and `TURNFRAME_GEMINI_LIVE_BASE_URL` point them at another
model or at a gateway, and `TURNFRAME_GEMINI_LIVE_TRANSPORT=function` runs the same two calls through
the forced function call instead of `responseSchema`: the two transports must produce the same parsed
answer, and this is how you check that against the service rather than against a fixture.

Vertex AI is deliberately not covered: its credential is a short-lived OAuth token this crate never
mints, so a smoke test for it would be a test of whatever mints the token.

## Known gaps

- **No idempotency key.** Neither surface offers one, so `ModelRequest::request_id` labels attempt
  records on this side but cannot help Google deduplicate. A model call commits no effect, so that
  costs a duplicate generation at worst.
- **A cache hint is dropped, with a warning.** Gemini's explicit caching addresses a `cachedContent`
  resource that must be created and paid for out of band. Implicit caching still happens and is
  reported as `TokenUsage::cached_input`.
- **Only the first candidate is read.** A response with more reports `FeatureDropped`; merging
  candidates would be a choice the adapter has no standing to make.

## More

See the workspace [README](../../README.md), the [provider adapter guide](../../docs/provider-adapters.md),
[ADR-008](../../docs/adr/ADR-008-provider-capability-routing-and-no-silent-downgrade.md) and the
provider-neutral layer in [`turnframe-provider`](../turnframe-provider/README.md).

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
