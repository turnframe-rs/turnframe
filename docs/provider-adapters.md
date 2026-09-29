# Provider adapters

Turnframe talks to language models through a provider-neutral layer. The runtime and the model
tasks see only normalized requests, normalized responses and declared capabilities; vendor wire
formats stay inside their adapter crates. This guide explains how that layer is shaped, what an
adapter may do, how routing and fallback are bounded, and how to write and certify a new adapter. It follows the master spec (§20, §24, §25.2) and ADR-008.

Keep one sentence in mind: **models propose meaning, deterministic reducers decide effects,
committed events decide claims.** An adapter is the channel through which a proposal arrives; it
never decides anything, and it must never make a weak proposal look like a strong one.

## Where the provider layer sits

| Crate | Responsibility |
| --- | --- |
| `turnframe-provider` | The `ModelProvider` trait, `ModelPurpose`, `ProviderCapabilities`, `CapabilityRequirements`, the `ProviderRouter` trait, retry and fallback policy, redaction hooks, and the conformance suite. |
| `turnframe-provider-*` | One crate per vendor family. Wire conversion, authentication, streaming adaptation, capability declarations and provider-specific error mapping. Nothing else. |
| `turnframe-test` | Fake providers, model-script doubles and the conformance macros an adapter crate runs in its own tests. |

`turnframe-core` never depends on a provider crate, and provider crates depend on
`turnframe-provider` (plus the shared types of `turnframe-core`), never on `turnframe-runtime`
internals. An adapter contains no workflow policy: it does not know what a
`WorkflowDefinition` is, never inspects a `WorkflowView`, and has no opinion about which acts are safe.

## The `ModelProvider` trait

Every adapter implements one trait:

```rust
#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    fn provider_key(&self) -> ProviderKey;
    fn model_key(&self) -> ModelKey;
    fn capabilities(&self) -> ProviderCapabilities;

    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError>;
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError>;
}
```

A single `ModelProvider` instance represents one **provider-model profile**: one endpoint, one
authentication method, one model, one set of declared capabilities; the same vendor with two models
means two instances. `provider_key` and `model_key` label every provider metric, attempt record and
replay entry, so a reviewer can always answer "which model produced the answer that was reduced".

`generate` returns a complete normalized `ModelResponse`; `stream` returns a `ModelStream` that the
runtime reassembles into the same shape, and the conformance suite checks the two are identical.

## `ModelPurpose`: why a request is being made

Each request carries a purpose, and the purpose carries both capability requirements and the
logging and redaction policy that applies to it. There is one purpose per model task:

```rust
pub enum ModelPurpose {
    // Understanding: before any effect.
    Segment, Coverage, Route, Locate, Extract, Verify, QuestionFrame, CrossCheck, Investigate,
    // The reply: after the commit.
    Acknowledge, Answer, Review, Progress,
    // Never on a live turn.
    OfflineEvaluate,
}
```

The understanding tasks are the critical ones: what they answer can, after checks, target
resolution, policy and reduction, become commands with effects. `Investigate` asks for read-only
context before a unit is understood. `Acknowledge`, `Answer` and `Review` run after the
`EventLedger` has recorded what occurred, so they describe committed facts. `Progress` says, as it
happens, what the understanding is doing; it describes no outcome. `OfflineEvaluate` serves
`turnframe-eval` runs and never touches a live turn.

Adapters do not branch on purpose; the router does.

## The capability model

```rust
pub struct ProviderCapabilities {
    pub structured_output: StructuredOutputCapability,
    pub tool_calling: ToolCallingCapability,
    pub parallel_tool_calls: bool,
    pub vision: bool,
    pub audio_input: bool,
    pub audio_output: bool,
    pub streaming: bool,
    pub prompt_caching: bool,
    pub reasoning_controls: bool,
    pub max_context_tokens: Option<u64>,
    pub preserves_call_ids: bool,
}

pub enum StructuredOutputCapability {
    NativeJsonSchema,
    NativeFunctionSchema,
    JsonObject,
    GrammarConstrained,
    PromptOnly,
    None,
}
```

Capabilities are **configured or probed per provider-model profile, never inferred from the vendor
brand**. The reasons are practical. A gateway that speaks the OpenAI wire format may front a model
that ignores `response_format`. A self-hosted runtime may support grammar-constrained decoding for
one model and not another. A vendor may ship a new model whose structured-output behaviour differs
from its predecessor. The brand tells you the shape of the HTTP request; only the profile tells you
what the model behind it will honour. An optimistic declaration is the most dangerous
misconfiguration in the system, because everything downstream trusts it.

`preserves_call_ids` matters because a tool call and its result are paired by id; a model that
renumbers or drops them must declare `false`.

## Critical-stage requirements

Each purpose accepts only some structured-output values:

| `StructuredOutputCapability` | Understanding tasks | `Investigate` and the reply | Condition |
| --- | --- | --- | --- |
| `NativeJsonSchema` | Yes | Yes | None beyond the conformance suite. |
| `NativeFunctionSchema` | Yes | Yes | Used strictly as a structured-output transport. The function is never executed; its arguments are the answer. |
| `GrammarConstrained` | Yes | Yes | Only when the profile has passing conformance tests for the grammar. |
| `JsonObject` | No | Yes | Well-formed JSON without schema enforcement; the answer is still validated, and it can only become text or a read. |
| `PromptOnly` | No by default | No by default | Admitted only under an explicitly named experimental unsafe mode (see below). |
| `None` | No | No | Not applicable. |

The experimental unsafe mode is a deliberate configuration act: never a default, never reachable
through a feature flag alone, and visible in the replay record and telemetry of every turn it
affects. Enabling it means the safety gates claimed for constrained understanding no longer hold.

When a selected provider turns out at request time to be unable to serve a critical stage (the
schema is refused, health degrades, a late capability mismatch surfaces), the runtime either routes
to another candidate that satisfies the **same** requirements or fails the stage with a typed
`ProviderError`. It never lowers the requirements, never retries with a weaker transport, and never
reinterprets an unconstrained response as if it were constrained. This is the "no silent downgrade"
rule, and every occurrence emits `turnframe.provider.capability_mismatch` with provider and model
keys as labels.

## Routing

```rust
pub trait ProviderRouter: Send + Sync {
    fn select(
        &self,
        purpose: ModelPurpose,
        requirements: &CapabilityRequirements,
        policy: &RoutingPolicy,
    ) -> Result<Vec<ProviderCandidate>, RoutingError>;
}
```

The router returns only candidates whose declared capabilities satisfy the requirements, ordered by
policy. A purpose with no satisfying candidate yields a `RoutingError`, never an empty list the
caller could proceed without. Beyond capability fit, a `RoutingPolicy` may weigh data residency, a
cost ceiling, a latency objective, a tenant allowlist, provider health, a model evaluation score and
a sensitivity policy (personal data may only leave through an allowlisted profile). The adapter takes
no part in this; it only publishes the facts the router reads.

## Fallback relative to commit

Fallback across providers is bounded by *where in the turn* it happens; the irreversible boundary is
the moment a command executes and its events are committed:

- Before any command executes, fallback is allowed. The understanding tasks can move to the next
  candidate freely.
- After commit, fallback is allowed for the reply's tasks (`Acknowledge`, `Answer`, `Review`),
  because the `EventLedger` already fixes the facts and a different model can only phrase them
  differently. A critical purpose passed with the post-commit stage is refused before any call.
- A message is never understood again after an effect may have committed. A timeout that arrives
  after dispatch is not a signal to run the understanding on another provider.
- Every attempt uses a stable request id and is recorded with provider key, model key, the
  requirements evaluated and the outcome. Fallbacks emit `turnframe.provider.fallback`.
- Partial outputs from different providers are never merged. A task's answer comes from one model
  call; a vote compares whole answers and never splices them.

Errors carry a retry classification (§24): retryability, whether an effect may have happened, a
user-safe message key and an operational severity. Adapters do the first mapping, from vendor status
codes and payloads to `ProviderError` variants; the policy in `turnframe-provider` acts on it.

## Secrets and redaction

Providers receive API keys and tokens through secret wrappers (`secrecy` or an equivalent) and
never through prompts or request bodies visible to the model. Command handlers own external
credentials for the actions they perform; the model layer has no path to them. In logs, traces and
error `Display` output, authorization headers and raw provider response bodies are redacted; the
conformance suite includes a fixture whose body contains a planted secret precisely to check that
no log line or error string reproduces it. Metric tag values carry provider and model keys only,
never user text, case text or identifiers that could be personal data.

## Conformance suite

Every adapter must pass the whole table below against wiremock fixtures in CI; live smoke tests are
optional and run outside CI. Failing a structured-output row means the capability declaration must
be lowered, not the test skipped.

| Case | What is asserted |
| --- | --- |
| Valid structured response | A schema-conformant body maps to a normalized `ModelResponse` with every field intact. |
| Malformed JSON | Yields a typed `ProviderError`; no partial parse is returned. |
| Unknown fields | Rejected or reported according to the stage's schema policy; never silently dropped for an understanding task. |
| Missing required fields | Yields a typed error; the answer is not accepted with defaults filled in. |
| Multiple items | A document with several items round-trips in order, as one answer. |
| Tool/read request ids | Ids sent to the model come back unchanged and correlate; `preserves_call_ids` matches observed behaviour. |
| Streaming reconstruction | The reassembled stream equals the non-streamed response for the same fixture. |
| Empty output | An empty body or empty content array is a typed error, not an empty answer. |
| Refusal | A vendor refusal maps to the refusal variant, distinguishable from malformed output. |
| Timeout | Classified as retryable-before-commit with "effect may have happened: no" for the model call itself. |
| Rate limit | Mapped to the rate-limit variant with any retry-after hint preserved. |
| Authentication failure | Mapped to a non-retryable variant; the secret never appears in the error text. |
| Context overflow | Mapped to its own variant so the runtime can shrink context instead of retrying blindly. |
| Cancellation | Dropping the future or cancelling the token aborts cleanly with no dangling state. |
| Retry classification | Each error variant carries the classification the policy layer expects. |
| Redaction of secrets | Planted secrets in headers and bodies are absent from logs, traces and `Display` strings. |
| No silent capability downgrade | A request whose requirements exceed the declared capabilities is refused, never served with a weaker transport. |

## Initial adapters

The workspace ships these adapter crates. Each hosts several profiles, and **conformance is per
provider-model combination**: passing with one model says nothing about another behind the same crate.

| Crate | Covers |
| --- | --- |
| `turnframe-provider-openai` | OpenAI, Azure OpenAI, and any OpenAI-compatible endpoint configured as a profile: OpenRouter, Together, Fireworks, DeepInfra, vLLM, llama.cpp server, Groq, Mistral, xAI, and Text Generation Inference where compatible. |
| `turnframe-provider-anthropic` | Anthropic Messages API. |
| `turnframe-provider-gemini` | Google Gemini, including Vertex AI. |
| `turnframe-provider-bedrock` | AWS Bedrock Converse. |
| `turnframe-provider-ollama` | Ollama local and remote servers. |

The OpenAI-compatible profiles need the most care: a shared wire format does not make providers
behaviourally identical, since structured-output support, id preservation, streaming framing and
error bodies all vary by gateway and by model.

## Writing a custom adapter

1. **Create the crate.** Name it `turnframe-provider-<vendor>`, depend on `turnframe-provider` (and
   on `turnframe-core` only for the shared types it re-exports) and, for tests, on `turnframe-test`.
   Do not depend on `turnframe-runtime`.
2. **Define the profile configuration.** Endpoint, model identifier, authentication method and the
   `ProviderCapabilities` declaration. Secrets are typed with `secrecy` from the moment they are read.
   Make the capability declaration explicit configuration, not a constant derived from the vendor.
3. **Implement wire conversion.** Map a normalized `ModelRequest` to the vendor request and the vendor
   response back to `ModelResponse`. Preserve tool and read request ids exactly. When the vendor
   offers a schema-enforced mode, use it and declare `NativeJsonSchema`; when it offers only function
   calling, use it as a transport and declare `NativeFunctionSchema`; if neither holds, declare what
   is true, even if that excludes the profile from the understanding tasks.
4. **Implement streaming.** Adapt the vendor's event framing to `ModelStream` so that reassembly is
   byte-for-byte equal to the non-streamed response. If the model cannot stream, declare
   `streaming: false` rather than emulating it.
5. **Map errors.** Translate status codes, error bodies and transport failures into `ProviderError`
   variants with the retry classification of §24. Never put a header or raw body into an error string.
   Three distinctions cannot be made from the status line alone, and the conformance suite has a row
   for each: an **expired credential** is `CredentialExpired`, not `Authentication`, even though it
   usually arrives on the same 401: a wrong key stays wrong while an expired token works again after
   a refresh, which is routine for Vertex AI bearer tokens and Bedrock session credentials; an
   **exhausted quota or empty credit balance** is `QuotaExhausted`, not `RateLimited`, even when the
   vendor reports it with 429, since waiting refills nothing, so the router must move on rather than sleep
   through the turn's deadline; and a **context-length 400** is `ContextOverflow`, not
   `InvalidRequest`, because only one of the two is fixed by shrinking the prompt. Where an endpoint
   genuinely cannot produce one of these signals, declare it with `StatusSupport::not_producible` and
   a reason, so the report marks the row unproven rather than passing it silently.
6. **Wire the redaction hooks.** Use the hooks in `turnframe-provider` for request and response
   logging, and confirm that a planted secret does not survive into any log line.
7. **Run the conformance suite.** Add wiremock fixtures for every row of the table above and invoke
   the conformance macros from `turnframe-test`. If a structured-output case fails, lower the
   declaration; do not weaken the test.
8. **Add optional live smoke tests** behind an environment guard, excluded from CI, one per profile
   you intend to deploy.
9. **Register profiles** in the application's routing configuration with their declared
   capabilities, and confirm that at least one profile satisfies the understanding tasks, plus a
   second one if fallback at that stage is wanted.

An adapter that follows these steps needs no change in the core: adding a model is a matter of
declaring honest capabilities and proving them. See ADR-001, ADR-008 and ADR-014 for the decisions
this guide rests on.

## Bedrock: a marketplace, not a model

The AWS Bedrock Converse adapter goes through the AWS SDK for Rust rather than
hand-rolled HTTP, and that is not a convenience. Every Bedrock request is signed
with SigV4 over a canonical form of its own headers and body, and credential
resolution, session refresh, region and endpoint resolution and retry
classification all hang off that signing. Reimplementing it would mean
reimplementing all of it, badly, and holding the credential to do so. The SDK
holds the credential; the adapter never does (§25.2).

**Structured output is a forced tool.** Converse has no `response_format`, no
JSON mode and no grammar. The only way to make the endpoint enforce a schema is
to declare one tool whose `inputSchema` *is* the required schema and pin
`toolChoice` to it; the model's `toolUse` block is then produced against that
schema, and the runtime reads the block's input as the document. That is exactly
what §20.4 admits as `NativeFunctionSchema`: a native function schema used only
as a structured-output transport, not direct execution. The forced call is never
executed, never reaches a command handler and never authorizes anything (§21.4).

Two consequences, both enforced at build time: `NativeJsonSchema`, `JsonObject`
and `GrammarConstrained` are refused, because they describe transports this wire
format does not have and a profile claiming one would be lying about the only
thing the whole system trusts; and `NativeFunctionSchema` cannot be declared
beside `ToolCallingCapability::None`, because the transport *is* a tool call.
What the transport does not give is a second answer alongside the document:
while `toolChoice` is pinned the model cannot call a read tool in the same turn,
so a stage that wants reads asks for `OutputSpec::ToolCalls` and gets the
ordinary tool loop.

**Capabilities are declared per model, never inferred.** The same Converse call
reaches Anthropic, Meta, Mistral, Amazon, Cohere and AI21 models, and they
disagree about images, documents, tools, parallel tool calls, prompt caching and
context windows. Bedrock exposes no capability endpoint that would answer for the
model you configured, and a declaration inferred from the brand would be a
declaration about nothing (§20.3). So the builder's `capabilities` is where you
record what you measured for one model id, and the conformance suite is how you
measure it: a passing report for one Claude model says nothing about a Llama one.
The single exception is `converse_defaults`, which claims the two things the
protocol decides rather than the model: streaming is available, and tool-call
ids round-trip.

**Streaming is real and is the default.** `stream` runs over `ConverseStream` and
forwards every fragment as it arrives; nothing is buffered to be flushed at the
end, and there is no fallback dressing a finished `Converse` answer up as a
stream. The token counts the metadata event carries land in the reassembled
response, so both paths report the same usage for the same answer (§20.8).

## Gemini: two surfaces, one translation

One crate serves the Gemini developer API, with an API key in `x-goog-api-key`, and
Vertex AI, where the model lives at a project- and region-shaped path behind a
short-lived OAuth bearer token. They share the request body and nothing else:
different host, different path, different credential with a different lifetime,
and one extra field (`labels`) only Vertex accepts. Both are endpoint profiles of
the same adapter, because the translation is the same and the plumbing is not.

**The schema dialect is a subset, and the adapter refuses rather than narrows.**
A `responseSchema` with `responseMimeType: "application/json"` is a real,
enforced `NativeJsonSchema` transport, so the profile declares it as one. But the
wire type is a restricted OpenAPI 3.0 `Schema`: no `$ref`, no `$defs`, no
`oneOf`, no `additionalProperties` as a schema. The translation carries across
everything with an equivalent and fails, naming the keyword and the JSON pointer,
on everything without one. It never sends a weaker schema, because a weaker
schema under a `NativeJsonSchema` declaration is the silent downgrade §0 rule 9
exists to forbid.

**Two ways to enforce a schema, both offered.** Besides `responseSchema`, Gemini
can carry a document through a forced function call: one declaration whose
`parameters` is the schema, with `toolConfig.functionCallingConfig` pinned to
`ANY` and that single name. The model has no other move than to fill the schema
in, the runtime reads the `functionCall` arguments as the document, and the call
is never executed and never authorizes anything (§21.4). Declared as
`NativeFunctionSchema`, a profile serves it instead; the parsed answer is
identical either way, which is what lets a router move a stage between this
adapter and one whose wire format has only the function form. It is also the
transport to reach for on a surface that refuses a response schema and `tools` in
the same request: the forced function *is* a tool, so nothing is given up. The
builder still refuses `GrammarConstrained`, and refuses `NativeFunctionSchema`
beside `ToolCallingCapability::None`. As on Bedrock, while the choice is pinned
the model cannot call a read tool in the same turn.

**A tool call has no id on this wire.** `functionCall` carries a name and
arguments, and the REST surface usually omits the optional `id`. So the default
profile declares `preserves_call_ids: false`, the adapter synthesizes stable
positional ids, and every response carrying one warns `SynthesizedCallIds`. A
function *response* is therefore addressed by name, recovered from the call it
answers.

**The Vertex token comes from the caller.** The crate embeds no Google
authentication library; it takes an already-obtained token through a
`TokenSource`, consulted once per request.

## Why `oneOf` is not simply renamed to `anyOf`

It is the rewrite everyone reaches for, and on its own it is a widening: `oneOf`
means *exactly one* branch matches, `anyOf` means *at least one*, so a document
matching two branches is rejected by the first and accepted by the second.

But the two are identical when no document can match two branches, and that is
the normal case for a schema generated from a Rust enum: every variant pins a
distinct discriminant. So the dialect module does not assume disjointness and
does not ignore it either: it **proves** it, and rewrites only what it proved. Three
sufficient conditions are checked, each of them sound:

- **by constant**: every branch pins the whole document to a finite set of
  literals, and no literal appears in two branches;
- **by type**: no two branches admit a common JSON type;
- **by discriminant**: every branch is an object requiring a shared property, and
  each pins that property to literals no other branch accepts, which is the
  shape `#[serde(tag = "kind")]` produces.

A union satisfying none of them is refused rather than approximated. Adapters
then compose the rewrites: OpenAI's strict mode wants disjointness-guarded
unions, sibling lifting and closed objects; Gemini's dialect wants definitions
inlined first, because it has no `$ref` to lift anything onto.
