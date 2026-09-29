# ADR-008: Provider capability routing and no silent downgrade

- Status: Accepted (2026-09-05)
- Amended (2026-09-26): the purposes are the model tasks. What this record requires of `InterpretTurn` applies to the understanding tasks; the reply's tasks accept a JSON object.

## Context

Turnframe talks to language models through a provider-neutral layer. The runtime only depends on
normalized capabilities and normalized responses; the wire formats of OpenAI, Anthropic, Gemini,
Bedrock, Ollama and the OpenAI-compatible gateways stay inside their adapter crates. That neutrality
creates a temptation that this decision exists to resist: because every adapter exposes the same
`ModelProvider` trait, it is easy to treat every provider-model pair as interchangeable and to let
the runtime "just try the next one" when the first fails or does not offer a feature.

The structured-output guarantee is where interchangeability breaks. The `InterpretTurn` stage is the
only place where a model proposes semantic acts that can, after validation and reduction, become
consequential commands. The whole safety chain (schema validation, evidence grounding, target
resolution, policy, reduction) assumes that the model's output arrived through a transport that
constrains its shape: a native JSON schema, a function schema used purely as a structured-output
carrier, or a grammar-constrained decoder that has passed conformance tests. A provider that only
offers prompt-only JSON, or a plain JSON-object mode with no schema enforcement, gives a response that
looks the same in the logs but was produced under a weaker contract. Nothing downstream can tell
the difference after the fact.

Concretely, these are the things that go wrong in a conversational application when capability is
not routed explicitly and downgrades are silent:

- A user in a trip workflow writes "add two bags and rebook it". The primary provider is rate
  limited; a fallback picks a model without schema-constrained output. The model emits a plausible
  plan where one act is malformed, the runtime accepts the parseable subset, and the rebooking is sent
  with one line instead of two. The receipt says "sent" and nobody can explain why the plan differed.
- An operator switches the default model in configuration to a cheaper one. No error appears, the
  application keeps answering fluently, but for weeks command planning has been running on
  prompt-only JSON. The regression is discovered only through an audit of wrong mutations.
- A provider times out after the reducer has already dispatched a command. A naive retry re-runs
  interpretation on another provider, produces a second plan, and executes it again, so a traveler is
  created twice or a submission is repeated.
- Two providers each return half of a multi-act plan and the runtime stitches them together. The
  resulting plan never existed as a single model proposal, so its internal consistency (negations,
  corrections, conditions) is unverified.
- A capability is assumed from the provider brand ("OpenAI supports JSON schema") while the actual
  model behind the configured endpoint (a gateway, a self-hosted runtime, an older model) does not.

The spec (§0 rule 9, §20.3 through §20.7) settles this: routing must be based on declared, per
provider-model capabilities; critical stages must reject providers that cannot meet their
requirements; and fallback is bounded by where in the turn it happens.

## Decision

1. Every provider adapter MUST declare `ProviderCapabilities` for the specific provider-model
   combination it serves. Capabilities MUST be configured or probed per model; they MUST NOT be
   inferred from the provider brand alone.
2. Every `ModelPurpose` MUST carry an explicit `CapabilityRequirements` value. The
   `ProviderRouter::select` call MUST receive those requirements together with the `RoutingPolicy`
   and MUST return only candidates whose declared capabilities satisfy them, in policy order. A
   purpose with no satisfying candidate MUST yield a `RoutingError`, never an empty best-effort list
   that the caller silently proceeds without.
3. `InterpretTurn` for a mutation-capable flow MUST require a `StructuredOutputCapability` of
   `NativeJsonSchema`, `NativeFunctionSchema` used only as a structured-output transport, or
   `GrammarConstrained` backed by passing conformance tests. `JsonObject`, `PromptOnly` and `None`
   MUST be rejected for this stage by default.
4. An application MAY opt into an explicitly named experimental unsafe mode that admits `PromptOnly`
   interpretation. That opt-in MUST be a deliberate configuration act, MUST be visible in the
   replay record and telemetry of every affected turn, and MUST NOT be the default of any
   configuration path or feature flag.
5. When the selected provider cannot serve a critical stage at request time (capability mismatch
   discovered late, provider health, refusal of the schema), the runtime MUST either route to another
   candidate that meets the same requirements or fail the stage with a typed `ProviderError`. It MUST
   NOT lower the requirements, retry with a weaker transport, or reinterpret an unconstrained
   response as if it were constrained.
6. Fallback across providers is permitted only before any command has executed, and for post-commit
   answer or narration generation where committed events already fix the facts. The runtime MUST
   NOT re-run interpretation or a mutation plan after an effect may have committed.
7. Every provider attempt MUST be recorded with a stable request identifier, the provider and model
   keys, the requirements evaluated, and the outcome. The replay record of a turn MUST allow a
   reviewer to see which provider produced the plan that was reduced.
8. Partial outputs from different providers MUST NOT be merged into one response unless the stage
   explicitly declares that it supports merging. `InterpretTurn` does not.
9. Every capability mismatch and every fallback MUST emit `turnframe.provider.capability_mismatch`
   or `turnframe.provider.fallback` respectively, with provider and model keys as labels and never
   user or case text.

## Consequences

Positive. The trust placed in a structured model response becomes an explicit property of the route
that produced it, not an assumption. Misconfiguration surfaces at routing time as a typed error
instead of weeks later as wrong mutations. Provider outages degrade the conversation (a clear
message that the request could not be planned right now) rather than degrading safety. Adding a
provider is a matter of declaring honest capabilities and passing the conformance suite; the core
does not change.

Negative. Some cheaper or self-hosted models are simply unavailable for mutation-capable
interpretation, and adopters will feel this as cost or latency they cannot trade away without the
explicit unsafe opt-in. Capability declarations are a maintenance burden: when a vendor ships a new
model or a gateway changes behaviour, the declaration must be updated and re-verified, and a stale
declaration can wrongly exclude a capable model. Routing adds one more decision point that must be
traced and tested, and adopters lose the convenience of a single "try anything" fallback chain.

What adopters must do. Configure at least one provider-model pair whose declared structured-output
capability satisfies `InterpretTurn` for every mutation-capable workflow they register, and a second
one if they want fallback at that stage. Keep capability declarations per model, not per vendor.
Treat `RoutingError` and capability-related `ProviderError` as operational alerts, and watch the two
provider metrics on the reliability dashboard. If they choose the experimental unsafe mode, they must
accept that the safety gates of §33 are no longer claimed for the affected flows.

## Alternatives considered

Brand-level capability tables with automatic fallback to prompt-only JSON. The router would assume
capabilities from the vendor name and, when structured output was unavailable, add a "respond only
with JSON" instruction and parse the result. Rejected because it makes the weakest transport the
implicit floor for every stage, hides the downgrade from operators and auditors, and contradicts §0
rule 9 directly. Gateways and self-hosted runtimes also make brand a poor predictor of behaviour.

Runtime validation as the only guard. Accept any provider for `InterpretTurn` and rely on schema
validation plus all-or-nothing parsing to reject bad output. Rejected because validation is
necessary but not sufficient: an unconstrained model can produce well-formed output that is
semantically wrong more often, the failure rate becomes model-dependent in ways the runtime cannot
observe, and the guarantee "this plan came through a constrained transport" would no longer be part
of the record that authorizes commands.

A single hard-coded provider for critical stages. Pin `InterpretTurn` to one vendor and allow the
rest to vary. Rejected because it contradicts the provider-neutral goal of the library, blocks data
residency and tenant policies, and still leaves the fallback question unanswered for that vendor's
outages.

## Enforcement

Invariants implemented. I9 (model output is a proposal) depends on this decision because the
proposal is only trustworthy under a constrained transport. I17 (provider failure cannot repeat
effects) is decision 6 above. I18 (model arrays are all-or-nothing) is upheld by decision 8, which
forbids stitching providers. I19 (critical state reads fail closed) extends to the routing decision:
if the runtime cannot establish that a candidate satisfies the requirements, it must not proceed.
I20 (replay is possible) is served by decision 7.

Tests and gates. The provider conformance suite of §20.8 and §27.5 includes an explicit "no silent
capability downgrade" case for every adapter, run against wiremock fixtures in CI, plus the
malformed-JSON, multiple-acts, refusal, timeout and rate-limit cases that exercise the fallback path.
Chaos tests of §27.7 inject failures "after remote request but before response" and "during
provider streaming" and verify that no interpretation is re-run after a possibly committed command.
The release gates of §33 that this ADR must satisfy are the provider gates: capability routing is
explicit, critical stages reject unsupported structured-output modes, fallback never repeats a
possibly committed command, adapter conformance suites pass, and provider raw data and secrets are
redacted. The safety gate "no malformed multi-act response executes a subset" is also affected.

Responsible code. `turnframe-provider` owns `ProviderCapabilities`, `StructuredOutputCapability`,
`CapabilityRequirements`, `ProviderRouter`, the fallback policy and the definition of the conformance suite; `turnframe-test` hosts the conformance macros and fixtures that each adapter crate runs in its own tests.
Each `turnframe-provider-*` crate owns its honest capability declaration and error mapping and
nothing else. `turnframe-runtime` owns the placement of fallback relative to command execution in
the turn pipeline (steps H through M of §23) and the recording of provider attempts in the replay
record. `turnframe-telemetry` owns the two provider metrics.
