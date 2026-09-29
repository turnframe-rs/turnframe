# Telemetry

`turnframe-telemetry` implements `turnframe_core::observe`: an `Observer`
receives a `Signal` with optional `SignalLabels`. This page holds the two rules
that decide what it refuses to do.

## What is recorded

- Every metric of §26.2 under the `turnframe.` prefix, as a counter: turns
  received, completed and failed; model tasks completed, repaired, escalated and
  split by a vote, and budgets exhausted; ambiguous, missing and unresolved
  targets; acts refused and superseded; command confirmations, executions,
  rejections, idempotency replays and revision conflicts; interactions created,
  resolved, stale and failed; receipts emitted; unknown and reconciled external
  outcomes; provider fallbacks and capability mismatches; workflow invariant
  violations; answered and unanswered questions.
- The separately measured latencies of §28, as histograms: whole-turn duration,
  projection, reduction, persistence, provider, model task, external command and
  narration.
- The stable identifiers of §26.1 as structured tracing fields: turn,
  conversation, workflow key and version, case id and revision, interaction id,
  provider, model, attempt number, plan hash, command id, event ids, response
  block ids and outbox id.
- One provider call per span, described with the OpenTelemetry GenAI semantic
  conventions: `gen_ai.system`, `gen_ai.operation.name`, the request and response
  model, temperature, response id, finish reasons and token usage. Those are the
  attributes the LLM observability backends already read (Langfuse, Datadog LLM
  Observability, Phoenix, Braintrust), so nothing here is vendor-specific. Input
  tokens are recorded **net of cached tokens**, with the cached figure beside
  them, so a consumer reading both never double counts.
- A vendor-neutral `TraceGrouping`: session (the conversation), a hashed end-user
  reference, tags, environment and release. Those backends filter at the level of
  the individual span, not only the trace root, so the grouping is made to reach
  every span: stamped on the spans this crate opens, or, with the `otel`
  feature, carried in OpenTelemetry baggage and copied onto every span as it
  starts by `GroupingSpanProcessor`.

## Where each signal fires

The runtime reports through the `Observer` given to `OrchestratorBuilder::observer`,
which it pushes into understanding, composition and interactions when it is built.

| Signal | Site | Cardinality |
| --- | --- | --- |
| `turn.received`, `turn.completed`, `turn.failed`, `turn.duration_ms` | `orchestrator` | once per turn, the duration whether it succeeded or not |
| `task.completed`, `task.repaired`, `task.escalated`, `budget.exhausted` | `turnframe-tasks`, for understanding and narration | once per model task, repair, escalation and exhausted bound |
| `provider.latency_ms`, `provider.fallback`, `provider.capability_mismatch` | `turnframe-tasks` | once per provider attempt, and once per candidate routing refused |
| `narration.latency_ms` | `turnframe-tasks` | once per acknowledge, answer or review call |
| `case.not_authorized` | `planning` | once per candidate the case directory refused |
| `projection.duration_us` | `planning` | once per case projected, so several times in a turn |
| `reduction.duration_us` | `reduce` via `orchestrator` | once per turn |
| `persistence.duration_ms` | `orchestrator` | once per turn, measuring the single atomic write |
| `interaction.resolved`, `interaction.failed` | `orchestrator` and `interactions` | once per card settled, on whichever path settled it |
| `external.latency_ms`, `external.reconciled` | `dispatch` | once per outbox row sent, and once per unknown outcome settled |

## Prompt and completion text stays out by default

It is user data, and putting it in a trace is a decision about data residency,
retention and consent, not a debugging convenience. `ContentRecorder` is the only
way in, it is disabled unless an application constructs it with
`ContentRecorder::enabled`, and even then every string passes through a
`ContentRedactor` the application wrote.

Said plainly: switching content recording on sends what your users typed, and
what the model answered, to your tracing backend.

## No user text ever becomes a metric label

§25.5 and §26.2 both state it, and the crate enforces it three ways rather than
asking for it:

1. Label values can only come from the typed fields of `SignalLabels`: workflow
   key, provider key, model key, request purpose, risk class, interaction kind,
   operation, a stable error code, and the turn's effort level (`low`, `medium`,
   `high`), which labels turn and task metrics so a level's cost can be read off
   them. There is no way to pass a free string.
2. Each signal declares the labels it documents; anything else is dropped rather
   than emitted, so one caller cannot inflate a metric's cardinality.
3. Every value is checked before it is emitted: empty values, values longer than
   the maximum, values containing whitespace and values that parse as a UUID are
   dropped: record identifiers are exactly what a label must never carry.
