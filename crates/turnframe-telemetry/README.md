# turnframe-telemetry

Metrics, structured tracing and a reliability dashboard for a Turnframe runtime.
Implementations of `turnframe_core::observe::Observer` you can hand to the
runtime at start-up, plus the data that turns them into a board.

Part of the [Turnframe](https://github.com/turnframe-rs/turnframe) workspace. See the
workspace [README](../../README.md) and [architecture guide](../../docs/architecture.md).

## Scope

| Module | What it holds |
| --- | --- |
| `metrics` | `MetricsObserver`: every signal becomes a counter or a latency histogram on the [`metrics`](https://docs.rs/metrics) facade. `describe_all()` registers descriptions and units; `catalogue()` and `catalogue_markdown()` publish the table below |
| `tracing` | `TracingObserver` plus the stable identifiers of spec §26.1 as structured fields, a turn span that hashes the account id, a span per pipeline stage, a `gen_ai.*` provider-call span, `TraceGrouping` and the opt-in `ContentRecorder` |
| `attrs` | Every attribute key in one place: the OpenTelemetry GenAI semantic conventions and the vendor-neutral trace-grouping keys |
| `composite` | `CompositeObserver` to fan one signal out to several observers, and `RecordingObserver` for other crates to assert against in their tests |
| `dashboard` | The reliability dashboard of spec §26.3 as serializable data, renderable as markdown |
| `otel` (feature `otel`) | The same names, units and labels on OpenTelemetry instruments; the same GenAI attributes as key/values; the trace grouping carried in baggage and stamped onto every span by `GroupingSpanProcessor` |

## LLM observability backends

Provider calls are described with the OpenTelemetry **GenAI semantic
conventions** (`gen_ai.system`, `gen_ai.operation.name`, request and response
model, temperature, response id, finish reasons, token usage), which is what
Langfuse, Datadog LLM Observability, Phoenix and Braintrust already consume. No
vendor is hardcoded: a backend that wants its own spelling is a five-line
renaming function over `TraceGrouping::fields()` or `ProviderCall::attributes()`
in the adopter's code.

Token accounting is explicit, because it is the part that is easy to get wrong:
`gen_ai.usage.input_tokens` is recorded **net of** `gen_ai.usage.input_cached_tokens`.
Read the first alone for the uncached cost, add both for the whole prompt;
neither double counts. `ProviderCall::with_cached_usage(total, cached, output)`
does the subtraction for a provider that reports a total.

`TraceGrouping` carries the session (the conversation), a hashed end-user
reference, tags, environment and release. These backends filter at the level of
the individual span, not only at the trace root, so the grouping has to reach
every span. Without the `otel` feature, `TraceGrouping::stamp` fills it in on
any span this crate opened. With the `otel` feature it is automatic: register
`otel::GroupingSpanProcessor` on your tracer provider and take an
`otel::attach_grouping` guard at the top of the turn.

```rust,ignore
let provider = SdkTracerProvider::builder()
    .with_span_processor(GroupingSpanProcessor::new())
    .with_span_processor(your_exporting_processor)
    .build();

let _guard = attach_grouping(&grouping);
// every span opened from here on carries session.id, user.id, tags,
// deployment.environment.name and service.version, children included.
```

**Ordering matters.** `on_start` reads the context at the moment a span begins,
so the grouping must be attached *before* the spans that should carry it are
created; a span already underway cannot be back-filled. Only the documented
grouping keys are copied, so an application's own baggage never leaks into
telemetry.

## Conversation content is off by default

Prompts and completions are user data, and this crate does not record them
unless you say so. `ContentRecorder::disabled()` is the default; switching it on
takes `ContentRecorder::enabled(redactor)` with a `ContentRedactor` you wrote,
and every string passes through that hook. Plainly: **turning content recording
on sends what your users typed, and what the model answered, to your tracing
backend.** Treat it as a data-residency and consent decision, not a debugging
convenience.

## The label rule

**No user text and no case text ever becomes a metric label.** Three things
enforce it:

1. A label value can only come from a typed field of `SignalLabels`: workflow
   key, provider key, model key, request purpose, risk class, interaction kind
   and a stable error code. There is no free-string field to abuse.
2. Every signal declares the dimensions it documents; anything else a caller
   attached is dropped, so one call site cannot inflate a metric's cardinality.
3. Every surviving value must pass `is_safe_label_value`: not empty, at most 64
   bytes, no whitespace, and not a UUID. A record identifier therefore cannot
   reach a metric dimension even when an application puts one in an error code
   by mistake.

Identifiers belong in traces instead, and even there the account id is hashed
rather than logged (`tracing::account_hash`).

## Minimal example

```rust
use std::sync::Arc;
use std::time::Duration;

use turnframe_core::ids::WorkflowKey;
use turnframe_core::observe::{Observer, Signal, SignalLabels};
use turnframe_telemetry::{CompositeObserver, MetricsObserver, TracingObserver, metrics};

// Once, after the metrics exporter is installed.
metrics::describe_all();

let observer: Arc<dyn Observer> = Arc::new(
    CompositeObserver::new()
        .with(MetricsObserver::new())
        .with(TracingObserver::new()),
);

let labels = SignalLabels::workflow(WorkflowKey::from("trip"));
observer.observe_labeled(&Signal::TurnReceived, &labels);
observer.observe_duration(&Signal::TurnDuration, Duration::from_millis(412), &labels);
```

## Metric catalogue

Generated from the code by `metrics::catalogue_markdown()`; a test regenerates
it and fails when this table drifts.

<!-- BEGIN GENERATED METRIC CATALOGUE -->

| Metric | Type | Unit | Labels | Meaning |
| --- | --- | --- | --- | --- |
| `turnframe.turn.received` | counter | count | `workflow`, `effort` | User turns accepted for processing. |
| `turnframe.turn.completed` | counter | count | `workflow`, `effort` | Turns that produced a persisted response. |
| `turnframe.turn.failed` | counter | count | `workflow`, `error_code`, `effort` | Turns that ended in an orchestrator error. |
| `turnframe.target.ambiguous` | counter | count | `workflow` | Acts whose target matched more than one case. |
| `turnframe.target.unresolved` | counter | count | `workflow`, `error_code` | Acts whose target produced no resolution at all, leaving no target record, no policy decision and no command. |
| `turnframe.target.missing` | counter | count | `workflow` | Acts whose target matched no case. |
| `turnframe.case.not_authorized` | counter | count | `workflow` | Candidates the case directory refused to authorize for the actor. |
| `turnframe.command.confirmation_required` | counter | count | `workflow`, `risk` | Commands held back until a confirmation interaction is answered. |
| `turnframe.command.executed` | counter | count | `workflow`, `risk` | Commands that committed. |
| `turnframe.command.rejected` | counter | count | `workflow`, `risk`, `error_code` | Commands the domain refused. |
| `turnframe.command.idempotency_replay` | counter | count | `workflow`, `risk` | Commands whose idempotency key was already in the journal. |
| `turnframe.command.revision_conflict` | counter | count | `workflow`, `risk` | Commands planned against a stale case revision. |
| `turnframe.interaction.created` | counter | count | `workflow`, `interaction` | Interaction cards persisted for the user. |
| `turnframe.interaction.resolved` | counter | count | `workflow`, `interaction` | Interaction cards answered and resolved. |
| `turnframe.interaction.stale` | counter | count | `workflow`, `interaction` | Answers that arrived after the case had moved on. |
| `turnframe.interaction.failed` | counter | count | `workflow`, `interaction`, `error_code` | Interaction responses that could not be accepted. |
| `turnframe.claim.receipt_emitted` | counter | count | `workflow` | Operational receipts derived from committed events. |
| `turnframe.external.outcome_unknown` | counter | count | `workflow`, `error_code` | External side effects whose outcome is unknown and needs reconciliation. |
| `turnframe.external.reconciled` | counter | count | `workflow` | External side effects whose outcome was later established. |
| `turnframe.provider.fallback` | counter | count | `provider`, `model`, `purpose`, `error_code` | Provider calls that fell back to another candidate. |
| `turnframe.provider.capability_mismatch` | counter | count | `provider`, `model`, `purpose`, `error_code` | Provider calls refused because the model lacked a required capability. |
| `turnframe.workflow.invariant_violation` | counter | count | `workflow`, `error_code` | Projected views that broke a Flow Map invariant. |
| `turnframe.question.answered` | counter | count | `workflow` | Questions answered from the facts of their records or a knowledge source. |
| `turnframe.question.unanswered` | counter | count | `workflow` | Questions the turn could not answer. |
| `turnframe.act.superseded` | counter | count | `workflow`, `operation` | Acts a correction or a cancel in the same message replaced. |
| `turnframe.act.refused` | counter | count | `workflow` | Acts the domain refused during reduction, which never became commands. |
| `turnframe.turn.duration_ms` | histogram | ms | `workflow`, `effort` | Wall-clock time from accepted input to persisted response. |
| `turnframe.projection.duration_us` | histogram | µs | `workflow` | Pure projection time of one case. |
| `turnframe.reduction.duration_us` | histogram | µs | `workflow` | Whole-turn reduction time. |
| `turnframe.persistence.duration_ms` | histogram | ms | `workflow` | Time spent in persistence for one turn. |
| `turnframe.provider.latency_ms` | histogram | ms | `provider`, `model`, `purpose` | Latency of one provider call. |
| `turnframe.external.latency_ms` | histogram | ms | `workflow` | Latency of one external command dispatch. |
| `turnframe.narration.latency_ms` | histogram | ms | `provider`, `model`, `purpose` | Latency of one call that writes or reviews the reply. |
| `turnframe.task.completed` | counter | count | `provider`, `model`, `purpose`, `error_code`, `effort` | Model tasks finished, by purpose and verdict. |
| `turnframe.task.repaired` | counter | count | `provider`, `model`, `purpose`, `error_code`, `effort` | Model task answers sent back for a repair. |
| `turnframe.task.escalated` | counter | count | `provider`, `model`, `purpose`, `error_code`, `effort` | Model tasks re-run on a stronger model. |
| `turnframe.task.vote_disagreement` | counter | count | `provider`, `model`, `purpose`, `effort` | Model task votes that found no majority. |
| `turnframe.budget.exhausted` | counter | count | `workflow`, `error_code`, `effort` | Turns whose model calls reached a bound. |
| `turnframe.task.latency_ms` | histogram | ms | `provider`, `model`, `purpose`, `effort` | Latency of one model task call. |

<!-- END GENERATED METRIC CATALOGUE -->

## Reliability dashboard

`Dashboard::reliability()` describes the seven panels of spec §26.3 (side-effect
integrity failures, claim integrity failures, semantic understanding failures,
clarification rate, abandonment rate, provider failures and user experience
scores) and names the metrics that feed each one. It is plain serializable
data, so the same description can drive a Grafana or Datadog board, an alert
inventory, or a docs page through `Dashboard::to_markdown()`.

The split is the point: a single "agent accuracy" percentage would average a
misworded sentence together with a wrongly rebooked flight, and nobody can act on
that number.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
