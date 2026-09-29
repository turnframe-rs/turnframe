//! Metric emission through the [`metrics`](https://docs.rs/metrics) facade
//! (spec §26.2, §28).
//!
//! Every [`Signal`] becomes one metric named exactly by
//! [`Signal::name`]: a counter for occurrence signals, a histogram for the
//! latency signals of spec §28, recorded in the unit its name ends with.
//!
//! The dimensions of those metrics are deliberately narrow. A signal declares
//! the labels it documents in [`documented_labels`]; anything else the caller
//! passed is dropped, and every surviving value must satisfy
//! [`is_safe_label_value`]. Neither the user's words nor a case identifier can
//! reach a label: [`SignalLabels`] has no free-text field, and the safety check
//! rejects UUIDs, whitespace and anything longer than
//! [`MAX_LABEL_VALUE_LEN`].

use std::time::Duration;

use metrics::{Key, Label, Level, Metadata, Unit};
use serde::{Deserialize, Serialize};
use turnframe_core::observe::{Observer, Signal, SignalKind, SignalLabels};

use crate::{enum_label, opt_str};

/// Target every Turnframe metric is registered under, for exporter filtering.
pub const METRIC_TARGET: &str = "turnframe";

/// Longest label value that is emitted. Longer values are dropped rather than
/// truncated: a truncated identifier is still an identifier.
pub const MAX_LABEL_VALUE_LEN: usize = 64;

static METADATA: Metadata<'static> =
    Metadata::new(METRIC_TARGET, Level::INFO, Some(module_path!()));

/// Name of a metric dimension.
///
/// The set is closed on purpose: it is exactly the typed, bounded fields of
/// [`SignalLabels`]. There is no variant for anything an end user typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LabelKey {
    /// Workflow key, e.g. `trip`.
    Workflow,
    /// Configured provider key, e.g. `openai`.
    Provider,
    /// Configured model key, e.g. `gpt-x`.
    Model,
    /// Normalized request purpose, e.g. `extract` (spec §20.2).
    Purpose,
    /// Risk class of a command (spec §14.3).
    Risk,
    /// Kind of an interaction (spec §15).
    Interaction,
    /// Stable machine-readable failure or rejection code. Never a message.
    ErrorCode,
    /// Operation an act names, e.g. `trip.set_name`. Bounded by the
    /// catalog, like [`Self::Workflow`] is bounded by the registry.
    Operation,
    /// The effort of the turn, `low`, `medium` or `high`.
    Effort,
}

impl LabelKey {
    /// Every label name, in the order they are emitted.
    pub const ALL: [Self; 9] = [
        Self::Workflow,
        Self::Provider,
        Self::Model,
        Self::Purpose,
        Self::Risk,
        Self::Interaction,
        Self::ErrorCode,
        Self::Operation,
        Self::Effort,
    ];

    /// The label name as it appears on the metric.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workflow => "workflow",
            Self::Provider => "provider",
            Self::Model => "model",
            Self::Purpose => "purpose",
            Self::Risk => "risk",
            Self::Interaction => "interaction",
            Self::ErrorCode => "error_code",
            Self::Operation => "operation",
            Self::Effort => "effort",
        }
    }

    /// Reads this dimension out of a label set, if the caller supplied it.
    #[must_use]
    pub fn value_of(self, labels: &SignalLabels) -> Option<String> {
        match self {
            Self::Workflow => opt_str(&labels.workflow).map(ToOwned::to_owned),
            Self::Provider => opt_str(&labels.provider).map(ToOwned::to_owned),
            Self::Model => opt_str(&labels.model).map(ToOwned::to_owned),
            Self::Purpose => opt_str(&labels.purpose).map(ToOwned::to_owned),
            Self::Risk => labels.risk.as_ref().and_then(enum_label),
            Self::Interaction => labels.interaction.as_ref().and_then(enum_label),
            Self::ErrorCode => opt_str(&labels.error_code).map(ToOwned::to_owned),
            Self::Operation => labels.operation.as_ref().map(|key| key.as_str().to_owned()),
            Self::Effort => labels.effort.map(|effort| effort.as_str().to_owned()),
        }
    }
}

/// Returns `true` when `value` is safe to use as a metric dimension.
///
/// A value is rejected when it is empty, longer than [`MAX_LABEL_VALUE_LEN`]
/// bytes, contains whitespace (a hallmark of prose rather than a code) or
/// parses as a UUID (the shape of every server-generated record identifier).
/// Configured keys, enumeration names and stable error codes all pass.
#[must_use]
pub fn is_safe_label_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LABEL_VALUE_LEN
        && !value.chars().any(char::is_whitespace)
        && uuid::Uuid::parse_str(value).is_err()
}

/// The dimensions a signal documents.
///
/// Labels outside this set are dropped, which is what keeps one metric's
/// cardinality bounded no matter what a caller attaches. Signals added to
/// [`Signal`] after this crate was compiled fall back to the workflow
/// dimension alone.
#[must_use]
pub fn documented_labels(signal: Signal) -> &'static [LabelKey] {
    use LabelKey::{
        Effort, ErrorCode, Interaction, Model, Operation, Provider, Purpose, Risk, Workflow,
    };

    const WORKFLOW: &[LabelKey] = &[Workflow];
    const WORKFLOW_ERROR: &[LabelKey] = &[Workflow, ErrorCode];
    const COMMAND: &[LabelKey] = &[Workflow, Risk];
    const COMMAND_ERROR: &[LabelKey] = &[Workflow, Risk, ErrorCode];
    const INTERACTION: &[LabelKey] = &[Workflow, Interaction];
    const INTERACTION_ERROR: &[LabelKey] = &[Workflow, Interaction, ErrorCode];
    const PROVIDER_ERROR: &[LabelKey] = &[Provider, Model, Purpose, ErrorCode];
    const PROVIDER: &[LabelKey] = &[Provider, Model, Purpose];
    const OPERATION: &[LabelKey] = &[Workflow, Operation];
    const TURN: &[LabelKey] = &[Workflow, Effort];
    const TURN_ERROR: &[LabelKey] = &[Workflow, ErrorCode, Effort];
    const TASK: &[LabelKey] = &[Provider, Model, Purpose, Effort];
    const TASK_ERROR: &[LabelKey] = &[Provider, Model, Purpose, ErrorCode, Effort];

    match signal {
        Signal::TurnReceived | Signal::TurnCompleted | Signal::TurnDuration => TURN,
        Signal::TurnFailed => TURN_ERROR,
        Signal::TargetAmbiguous | Signal::TargetMissing | Signal::CaseNotAuthorized => WORKFLOW,
        // The reason is the whole value here: "an act went nowhere" is a number,
        // "an act named a card that was not open" is a diagnosis.
        Signal::TargetUnresolved => WORKFLOW_ERROR,
        Signal::ActSuperseded => OPERATION,
        Signal::ActRefused => WORKFLOW,
        Signal::CommandConfirmationRequired | Signal::CommandExecuted => COMMAND,
        Signal::CommandRejected => COMMAND_ERROR,
        Signal::CommandIdempotencyReplay | Signal::CommandRevisionConflict => COMMAND,
        Signal::InteractionCreated | Signal::InteractionResolved | Signal::InteractionStale => {
            INTERACTION
        }
        Signal::InteractionFailed => INTERACTION_ERROR,
        Signal::ClaimReceiptEmitted => WORKFLOW,
        Signal::ExternalOutcomeUnknown => WORKFLOW_ERROR,
        Signal::ExternalReconciled => WORKFLOW,
        Signal::ProviderFallback | Signal::ProviderCapabilityMismatch => PROVIDER_ERROR,
        Signal::WorkflowInvariantViolation => WORKFLOW_ERROR,
        Signal::QuestionAnswered | Signal::QuestionUnanswered => WORKFLOW,
        Signal::ProjectionDuration
        | Signal::ReductionDuration
        | Signal::PersistenceDuration
        | Signal::ExternalLatency => WORKFLOW,
        Signal::ProviderLatency | Signal::NarrationLatency => PROVIDER,
        // A task's verdict and a repair's cause travel as the error code.
        Signal::TaskCompleted | Signal::TaskRepaired | Signal::TaskEscalated => TASK_ERROR,
        Signal::TaskVoteDisagreement | Signal::TaskLatency => TASK,
        // The bound that stopped the turn is the error code.
        Signal::BudgetExhausted => TURN_ERROR,
        // `Signal` is `#[non_exhaustive]`; an unknown signal keeps the safest
        // dimension rather than none, so it is still attributable.
        _ => WORKFLOW,
    }
}

/// The metric type a signal is exported as: `counter` or `histogram`.
#[must_use]
pub fn metric_type(signal: Signal) -> &'static str {
    match signal.kind() {
        SignalKind::DurationMillis | SignalKind::DurationMicros => "histogram",
        _ => "counter",
    }
}

/// The unit a signal is recorded in.
#[must_use]
pub fn unit(signal: Signal) -> Unit {
    match signal.kind() {
        SignalKind::DurationMillis => Unit::Milliseconds,
        SignalKind::DurationMicros => Unit::Microseconds,
        _ => Unit::Count,
    }
}

/// One-line meaning of a signal, registered as the metric description and
/// published in the crate's metric catalogue.
#[must_use]
pub fn description(signal: Signal) -> &'static str {
    match signal {
        Signal::TurnReceived => "User turns accepted for processing.",
        Signal::TurnCompleted => "Turns that produced a persisted response.",
        Signal::TurnFailed => "Turns that ended in an orchestrator error.",
        Signal::TargetAmbiguous => "Acts whose target matched more than one case.",
        Signal::TargetMissing => "Acts whose target matched no case.",
        Signal::TargetUnresolved => {
            "Acts whose target produced no resolution at all, leaving no target record, no policy \
             decision and no command."
        }
        Signal::CaseNotAuthorized => {
            "Candidates the case directory refused to authorize for the actor."
        }
        Signal::CommandConfirmationRequired => {
            "Commands held back until a confirmation interaction is answered."
        }
        Signal::CommandExecuted => "Commands that committed.",
        Signal::CommandRejected => "Commands the domain refused.",
        Signal::CommandIdempotencyReplay => {
            "Commands whose idempotency key was already in the journal."
        }
        Signal::CommandRevisionConflict => "Commands planned against a stale case revision.",
        Signal::InteractionCreated => "Interaction cards persisted for the user.",
        Signal::InteractionResolved => "Interaction cards answered and resolved.",
        Signal::InteractionStale => "Answers that arrived after the case had moved on.",
        Signal::InteractionFailed => "Interaction responses that could not be accepted.",
        Signal::ClaimReceiptEmitted => "Operational receipts derived from committed events.",
        Signal::ExternalOutcomeUnknown => {
            "External side effects whose outcome is unknown and needs reconciliation."
        }
        Signal::ExternalReconciled => "External side effects whose outcome was later established.",
        Signal::ProviderFallback => "Provider calls that fell back to another candidate.",
        Signal::ProviderCapabilityMismatch => {
            "Provider calls refused because the model lacked a required capability."
        }
        Signal::WorkflowInvariantViolation => "Projected views that broke a Flow Map invariant.",
        Signal::QuestionAnswered => {
            "Questions answered from the facts of their records or a knowledge source."
        }
        Signal::QuestionUnanswered => "Questions the turn could not answer.",
        Signal::ActRefused => {
            "Acts the domain refused during reduction, which never became commands."
        }
        Signal::ActSuperseded => "Acts a correction or a cancel in the same message replaced.",
        Signal::TurnDuration => "Wall-clock time from accepted input to persisted response.",
        Signal::ProjectionDuration => "Pure projection time of one case.",
        Signal::ReductionDuration => "Whole-turn reduction time.",
        Signal::PersistenceDuration => "Time spent in persistence for one turn.",
        Signal::ProviderLatency => "Latency of one provider call.",
        Signal::ExternalLatency => "Latency of one external command dispatch.",
        Signal::NarrationLatency => "Latency of one call that writes or reviews the reply.",
        Signal::TaskCompleted => "Model tasks finished, by purpose and verdict.",
        Signal::TaskRepaired => "Model task answers sent back for a repair.",
        Signal::TaskEscalated => "Model tasks re-run on a stronger model.",
        Signal::TaskVoteDisagreement => "Model task votes that found no majority.",
        Signal::BudgetExhausted => "Turns whose model calls reached a bound.",
        Signal::TaskLatency => "Latency of one model task call.",
        // `Signal` is `#[non_exhaustive]`: a signal this crate predates still
        // gets a metric, just a generic description.
        _ => "Turnframe signal.",
    }
}

/// Builds the labels actually emitted for a signal: the dimensions the signal
/// documents, restricted to the values the caller supplied and to those that
/// pass [`is_safe_label_value`].
#[must_use]
pub fn metric_labels(signal: Signal, labels: &SignalLabels) -> Vec<Label> {
    documented_labels(signal)
        .iter()
        .filter_map(|key| key.value_of(labels).map(|value| (*key, value)))
        .filter(|(_, value)| is_safe_label_value(value))
        .map(|(key, value)| Label::new(key.as_str(), value))
        .collect()
}

/// One row of the metric catalogue: everything the README table publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MetricDoc {
    /// The signal this metric reports.
    pub signal: Signal,
    /// The metric name, identical to [`Signal::name`].
    pub name: &'static str,
    /// `counter` or `histogram`.
    pub metric_type: &'static str,
    /// The recorded unit.
    pub unit: Unit,
    /// The dimensions the metric documents.
    pub labels: &'static [LabelKey],
    /// What the metric means.
    pub description: &'static str,
}

impl MetricDoc {
    /// The catalogue row for one signal.
    #[must_use]
    pub fn of(signal: Signal) -> Self {
        Self {
            signal,
            name: signal.name(),
            metric_type: metric_type(signal),
            unit: unit(signal),
            labels: documented_labels(signal),
            description: description(signal),
        }
    }

    /// The label names, comma separated, or `—` when the metric has none.
    #[must_use]
    pub fn label_list(&self) -> String {
        if self.labels.is_empty() {
            return String::from("—");
        }
        self.labels
            .iter()
            .map(|key| format!("`{}`", key.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The whole metric catalogue, in the order of [`Signal::ALL`].
#[must_use]
pub fn catalogue() -> Vec<MetricDoc> {
    Signal::ALL.into_iter().map(MetricDoc::of).collect()
}

/// Renders the metric catalogue as the markdown table published in the crate's
/// README. A test compares the README against this function so the two cannot
/// drift.
#[must_use]
pub fn catalogue_markdown() -> String {
    let mut out = String::from("| Metric | Type | Unit | Labels | Meaning |\n");
    out.push_str("| --- | --- | --- | --- | --- |\n");
    for doc in catalogue() {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            doc.name,
            doc.metric_type,
            unit_label(doc.unit),
            doc.label_list(),
            doc.description,
        ));
    }
    out
}

/// The unit as it is written in the catalogue table.
#[must_use]
pub fn unit_label(unit: Unit) -> &'static str {
    match unit {
        Unit::Milliseconds => "ms",
        Unit::Microseconds => "µs",
        _ => "count",
    }
}

/// Registers the description and unit of every metric with the installed
/// `metrics` recorder.
///
/// Call it once at start-up, after the exporter is installed and before the
/// first turn, so an exporter that publishes metadata (Prometheus `HELP` and
/// `TYPE`, OTLP descriptions) has it. Calling it with no recorder installed is
/// a no-op.
pub fn describe_all() {
    metrics::with_recorder(|recorder| {
        for signal in Signal::ALL {
            let name = metrics::KeyName::from(signal.name());
            let metric_unit = unit(signal);
            let text = metrics::SharedString::from(description(signal));
            match signal.kind() {
                SignalKind::DurationMillis | SignalKind::DurationMicros => {
                    recorder.describe_histogram(name, Some(metric_unit), text);
                }
                _ => recorder.describe_counter(name, Some(metric_unit), text),
            }
        }
    });
}

/// An [`Observer`] that turns signals into `metrics` counters and histograms.
///
/// Counters are incremented by one per occurrence. Duration signals are
/// recorded as histograms in the unit their name ends with — milliseconds for
/// `*_ms`, microseconds for `*_us` — with sub-unit precision preserved.
///
/// A duration signal reported through
/// [`observe_labeled`](Observer::observe_labeled) carries no measurement and is
/// dropped; a counter signal reported through
/// [`observe_duration`](Observer::observe_duration) is counted and the duration
/// ignored.
///
/// ```rust
/// use turnframe_core::ids::WorkflowKey;
/// use turnframe_core::observe::{Observer, Signal, SignalLabels};
/// use turnframe_telemetry::MetricsObserver;
///
/// let observer = MetricsObserver::new();
/// observer.observe_labeled(
///     &Signal::TurnCompleted,
///     &SignalLabels::workflow(WorkflowKey::from("trip")),
/// );
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct MetricsObserver;

impl MetricsObserver {
    /// Builds the observer. Emission goes to whichever recorder the
    /// application installed, so there is nothing to configure here.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn record(signal: Signal, labels: &SignalLabels, duration: Option<Duration>) {
        let key = Key::from_parts(signal.name(), metric_labels(signal, labels));
        metrics::with_recorder(|recorder| match signal.kind() {
            SignalKind::DurationMillis => {
                if let Some(duration) = duration {
                    recorder
                        .register_histogram(&key, &METADATA)
                        .record(duration.as_secs_f64() * 1_000.0);
                }
            }
            SignalKind::DurationMicros => {
                if let Some(duration) = duration {
                    recorder
                        .register_histogram(&key, &METADATA)
                        .record(duration.as_secs_f64() * 1_000_000.0);
                }
            }
            _ => recorder.register_counter(&key, &METADATA).increment(1),
        });
    }
}

impl Observer for MetricsObserver {
    fn observe(&self, signal: &Signal) {
        Self::record(*signal, &SignalLabels::none(), None);
    }

    fn observe_labeled(&self, signal: &Signal, labels: &SignalLabels) {
        Self::record(*signal, labels, None);
    }

    fn observe_duration(&self, signal: &Signal, duration: Duration, labels: &SignalLabels) {
        Self::record(*signal, labels, Some(duration));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use metrics::{
        Counter, CounterFn, Gauge, Histogram, HistogramFn, KeyName, Recorder, SharedString,
    };
    use turnframe_core::command::RiskClass;
    use turnframe_core::ids::WorkflowKey;
    use turnframe_core::interaction::InteractionKind;

    use super::*;

    /// `Signal` is `#[non_exhaustive]`, so a downstream crate cannot write a
    /// match over it without a wildcard arm. This const is the compile-time
    /// gate instead: `Signal::ALL` is a fixed-size array, so adding a variant
    /// in core changes its length and breaks this assertion until the fixture
    /// below is extended.
    const _: () = assert!(
        Signal::ALL.len() == 39,
        "a signal was added to turnframe-core: extend the fixture in this module"
    );

    #[derive(Debug, Default)]
    struct Captured {
        counters: Vec<(Key, u64)>,
        histograms: Vec<(Key, f64)>,
        described: Vec<(String, &'static str, Option<Unit>, String)>,
    }

    type Shared = Arc<Mutex<Captured>>;

    fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Captured> {
        shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[derive(Debug)]
    struct CapturedCounter {
        key: Key,
        shared: Shared,
    }

    impl CounterFn for CapturedCounter {
        fn increment(&self, value: u64) {
            lock(&self.shared).counters.push((self.key.clone(), value));
        }

        fn absolute(&self, value: u64) {
            lock(&self.shared).counters.push((self.key.clone(), value));
        }
    }

    #[derive(Debug)]
    struct CapturedHistogram {
        key: Key,
        shared: Shared,
    }

    impl HistogramFn for CapturedHistogram {
        fn record(&self, value: f64) {
            lock(&self.shared)
                .histograms
                .push((self.key.clone(), value));
        }
    }

    /// A recorder that keeps everything it was handed, so a test can assert on
    /// metric names, label sets and recorded values without an exporter.
    #[derive(Debug, Default)]
    struct TestRecorder {
        shared: Shared,
    }

    impl Recorder for TestRecorder {
        fn describe_counter(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
            lock(&self.shared).described.push((
                key.as_str().to_owned(),
                "counter",
                unit,
                description.to_string(),
            ));
        }

        fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
            lock(&self.shared).described.push((
                key.as_str().to_owned(),
                "gauge",
                unit,
                description.to_string(),
            ));
        }

        fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
            lock(&self.shared).described.push((
                key.as_str().to_owned(),
                "histogram",
                unit,
                description.to_string(),
            ));
        }

        fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
            Counter::from_arc(Arc::new(CapturedCounter {
                key: key.clone(),
                shared: Arc::clone(&self.shared),
            }))
        }

        fn register_gauge(&self, _key: &Key, _metadata: &Metadata<'_>) -> Gauge {
            Gauge::noop()
        }

        fn register_histogram(&self, key: &Key, _metadata: &Metadata<'_>) -> Histogram {
            Histogram::from_arc(Arc::new(CapturedHistogram {
                key: key.clone(),
                shared: Arc::clone(&self.shared),
            }))
        }
    }

    /// What one signal is expected to emit.
    struct Fixture {
        name: &'static str,
        labels: &'static [&'static str],
    }

    /// The expected wire shape of every signal. Kept as a match rather than a
    /// table so the fixture reads next to the signal it describes.
    fn fixture(signal: Signal) -> Fixture {
        const NONE: &[&str] = &[];
        const W: &[&str] = &["workflow"];
        const WE: &[&str] = &["workflow", "error_code"];
        const WR: &[&str] = &["workflow", "risk"];
        const WRE: &[&str] = &["workflow", "risk", "error_code"];
        const WI: &[&str] = &["workflow", "interaction"];
        const WIE: &[&str] = &["workflow", "interaction", "error_code"];
        const PMPE: &[&str] = &["provider", "model", "purpose", "error_code"];
        const PMP: &[&str] = &["provider", "model", "purpose"];
        const WO: &[&str] = &["workflow", "operation"];

        let (name, labels) = match signal {
            Signal::TurnReceived => ("turnframe.turn.received", W),
            Signal::TurnCompleted => ("turnframe.turn.completed", W),
            Signal::TurnFailed => ("turnframe.turn.failed", WE),
            Signal::TargetAmbiguous => ("turnframe.target.ambiguous", W),
            Signal::TargetMissing => ("turnframe.target.missing", W),
            Signal::TargetUnresolved => ("turnframe.target.unresolved", WE),
            Signal::CommandConfirmationRequired => ("turnframe.command.confirmation_required", WR),
            Signal::CommandExecuted => ("turnframe.command.executed", WR),
            Signal::CommandRejected => ("turnframe.command.rejected", WRE),
            Signal::CaseNotAuthorized => ("turnframe.case.not_authorized", W),
            Signal::CommandIdempotencyReplay => ("turnframe.command.idempotency_replay", WR),
            Signal::CommandRevisionConflict => ("turnframe.command.revision_conflict", WR),
            Signal::InteractionCreated => ("turnframe.interaction.created", WI),
            Signal::InteractionResolved => ("turnframe.interaction.resolved", WI),
            Signal::InteractionStale => ("turnframe.interaction.stale", WI),
            Signal::InteractionFailed => ("turnframe.interaction.failed", WIE),
            Signal::ClaimReceiptEmitted => ("turnframe.claim.receipt_emitted", W),
            Signal::ExternalOutcomeUnknown => ("turnframe.external.outcome_unknown", WE),
            Signal::ExternalReconciled => ("turnframe.external.reconciled", W),
            Signal::ProviderFallback => ("turnframe.provider.fallback", PMPE),
            Signal::ProviderCapabilityMismatch => ("turnframe.provider.capability_mismatch", PMPE),
            Signal::WorkflowInvariantViolation => ("turnframe.workflow.invariant_violation", WE),
            Signal::QuestionAnswered => ("turnframe.question.answered", W),
            Signal::QuestionUnanswered => ("turnframe.question.unanswered", W),
            Signal::ActSuperseded => ("turnframe.act.superseded", WO),
            Signal::ActRefused => ("turnframe.act.refused", W),
            Signal::TurnDuration => ("turnframe.turn.duration_ms", W),
            Signal::ProjectionDuration => ("turnframe.projection.duration_us", W),
            Signal::ReductionDuration => ("turnframe.reduction.duration_us", W),
            Signal::PersistenceDuration => ("turnframe.persistence.duration_ms", W),
            Signal::ProviderLatency => ("turnframe.provider.latency_ms", PMP),
            Signal::ExternalLatency => ("turnframe.external.latency_ms", W),
            Signal::NarrationLatency => ("turnframe.narration.latency_ms", PMP),
            Signal::TaskCompleted => ("turnframe.task.completed", PMPE),
            Signal::TaskRepaired => ("turnframe.task.repaired", PMPE),
            Signal::TaskEscalated => ("turnframe.task.escalated", PMPE),
            Signal::TaskVoteDisagreement => ("turnframe.task.vote_disagreement", PMP),
            Signal::BudgetExhausted => ("turnframe.budget.exhausted", WE),
            Signal::TaskLatency => ("turnframe.task.latency_ms", PMP),
            _ => ("", NONE),
        };
        assert!(!name.is_empty(), "no fixture for {signal:?}");
        Fixture { name, labels }
    }

    /// A label set that fills every dimension with a plausible value.
    fn full_labels() -> SignalLabels {
        SignalLabels::workflow(WorkflowKey::from("trip"))
            .with_provider("openai")
            .with_model("gpt-x")
            .with_purpose("extract")
            .with_risk(RiskClass::Destructive)
            .with_interaction(InteractionKind::ConfirmCommand)
            .with_error_code("rate_limited")
            .with_operation("trip.set_name")
    }

    fn label_names(key: &Key) -> Vec<String> {
        key.labels().map(|l| l.key().to_owned()).collect()
    }

    fn label_values(key: &Key) -> Vec<String> {
        key.labels().map(|l| l.value().to_owned()).collect()
    }

    #[test]
    fn every_signal_emits_its_name_and_documented_label_set() {
        for signal in Signal::ALL {
            let expected = fixture(signal);
            let recorder = TestRecorder::default();
            let shared = Arc::clone(&recorder.shared);
            metrics::with_local_recorder(&recorder, || {
                let observer = MetricsObserver::new();
                observer.observe_duration(&signal, Duration::from_millis(7), &full_labels());
            });

            let captured = lock(&shared);
            let key = match signal.kind() {
                SignalKind::DurationMillis | SignalKind::DurationMicros => {
                    assert_eq!(captured.counters.len(), 0, "{signal:?} is not a counter");
                    assert_eq!(captured.histograms.len(), 1, "{signal:?}");
                    captured.histograms[0].0.clone()
                }
                _ => {
                    assert_eq!(
                        captured.histograms.len(),
                        0,
                        "{signal:?} is not a histogram"
                    );
                    assert_eq!(captured.counters.len(), 1, "{signal:?}");
                    assert_eq!(captured.counters[0].1, 1, "{signal:?}");
                    captured.counters[0].0.clone()
                }
            };

            assert_eq!(key.name(), expected.name, "{signal:?}");
            assert_eq!(label_names(&key), expected.labels, "{signal:?}");
        }
    }

    #[test]
    fn durations_are_recorded_in_the_unit_of_the_name() {
        let cases = [
            (Signal::TurnDuration, 1_500.0_f64),
            (Signal::PersistenceDuration, 1_500.0),
            (Signal::ProviderLatency, 1_500.0),
            (Signal::ExternalLatency, 1_500.0),
            (Signal::NarrationLatency, 1_500.0),
            (Signal::TaskLatency, 1_500.0),
            (Signal::ProjectionDuration, 1_500_000.0),
            (Signal::ReductionDuration, 1_500_000.0),
        ];
        for (signal, expected) in cases {
            let recorder = TestRecorder::default();
            let shared = Arc::clone(&recorder.shared);
            metrics::with_local_recorder(&recorder, || {
                MetricsObserver::new().observe_duration(
                    &signal,
                    Duration::from_millis(1_500),
                    &SignalLabels::none(),
                );
            });
            let captured = lock(&shared);
            assert_eq!(captured.histograms.len(), 1, "{signal:?}");
            assert!(
                (captured.histograms[0].1 - expected).abs() < f64::EPSILON,
                "{signal:?}: {} != {expected}",
                captured.histograms[0].1
            );
        }
    }

    #[test]
    fn a_duration_signal_without_a_measurement_emits_nothing() {
        let recorder = TestRecorder::default();
        let shared = Arc::clone(&recorder.shared);
        metrics::with_local_recorder(&recorder, || {
            MetricsObserver::new().observe_labeled(&Signal::TurnDuration, &full_labels());
        });
        let captured = lock(&shared);
        assert!(captured.counters.is_empty());
        assert!(captured.histograms.is_empty());
    }

    #[test]
    fn no_label_value_is_a_uuid_or_longer_than_the_limit() {
        let adversarial =
            SignalLabels::workflow(WorkflowKey::from("0191f0f6-7d5b-7c2e-9a1e-2b3c4d5e6f70"))
                .with_provider("018f4e7c-3d2a-7b19-8f6e-112233445566")
                .with_model("m".repeat(MAX_LABEL_VALUE_LEN + 1))
                .with_purpose("the user asked to withdraw the trip for Marta Bianchi")
                .with_risk(RiskClass::Irreversible)
                .with_interaction(InteractionKind::Freeform)
                .with_error_code(String::new());

        for signal in Signal::ALL {
            let labels = metric_labels(signal, &adversarial);
            for label in &labels {
                let value = label.value();
                assert!(
                    uuid::Uuid::parse_str(value).is_err(),
                    "{signal:?}: {value} is a uuid"
                );
                assert!(
                    value.chars().count() <= MAX_LABEL_VALUE_LEN,
                    "{signal:?}: {value} is too long"
                );
                assert!(
                    !value.chars().any(char::is_whitespace),
                    "{signal:?}: {value} looks like prose"
                );
                assert!(!value.is_empty(), "{signal:?}: empty label value");
            }
            // Only the bounded enumerations survive an adversarial label set.
            let names: Vec<&str> = labels.iter().map(metrics::Label::key).collect();
            assert!(
                names.iter().all(|n| *n == "risk" || *n == "interaction"),
                "{signal:?}: {names:?}"
            );
        }
    }

    #[test]
    fn safe_label_values_accept_keys_and_codes() {
        assert!(is_safe_label_value("trip"));
        assert!(is_safe_label_value("openai"));
        assert!(is_safe_label_value("gpt-5.4-mini"));
        assert!(is_safe_label_value("trip.traveler_missing"));
        assert!(is_safe_label_value("external_regulated"));
        assert!(!is_safe_label_value(""));
        assert!(!is_safe_label_value("a b"));
        assert!(!is_safe_label_value(&"x".repeat(MAX_LABEL_VALUE_LEN + 1)));
        assert!(is_safe_label_value(&"x".repeat(MAX_LABEL_VALUE_LEN)));
        assert!(!is_safe_label_value("0191f0f6-7d5b-7c2e-9a1e-2b3c4d5e6f70"));
        assert!(!is_safe_label_value("0191f0f67d5b7c2e9a1e2b3c4d5e6f70"));
    }

    #[test]
    fn labels_not_documented_by_a_signal_are_dropped() {
        // `turn.received` documents only the workflow, so the provider and the
        // error code a caller attached never reach the metric.
        let labels = metric_labels(Signal::TurnReceived, &full_labels());
        assert_eq!(
            labels.iter().map(metrics::Label::key).collect::<Vec<_>>(),
            vec!["workflow"]
        );
    }

    #[test]
    fn describe_all_registers_every_metric_with_its_unit() {
        let recorder = TestRecorder::default();
        let shared = Arc::clone(&recorder.shared);
        metrics::with_local_recorder(&recorder, describe_all);

        let captured = lock(&shared);
        assert_eq!(captured.described.len(), Signal::ALL.len());
        for signal in Signal::ALL {
            let row = captured
                .described
                .iter()
                .find(|(name, ..)| name == signal.name())
                .unwrap_or_else(|| panic!("{signal:?} not described"));
            assert_eq!(row.1, metric_type(signal), "{signal:?}");
            assert_eq!(row.2, Some(unit(signal)), "{signal:?}");
            assert_eq!(row.3, description(signal), "{signal:?}");
            assert!(!row.3.is_empty());
        }
    }

    #[test]
    fn observe_without_labels_still_emits_the_metric() {
        let recorder = TestRecorder::default();
        let shared = Arc::clone(&recorder.shared);
        metrics::with_local_recorder(&recorder, || {
            MetricsObserver::new().observe(&Signal::TurnReceived);
        });
        let captured = lock(&shared);
        assert_eq!(captured.counters.len(), 1);
        assert_eq!(captured.counters[0].0.name(), "turnframe.turn.received");
        assert!(label_values(&captured.counters[0].0).is_empty());
    }

    #[test]
    fn a_turn_and_its_tasks_are_counted_by_effort() {
        let labels = SignalLabels::none().with_effort(turnframe_core::effort::Effort::High);
        for signal in [
            Signal::TurnCompleted,
            Signal::TurnDuration,
            Signal::TaskCompleted,
            Signal::TaskLatency,
        ] {
            assert!(
                documented_labels(signal).contains(&LabelKey::Effort),
                "{signal:?}"
            );
        }
        assert_eq!(LabelKey::Effort.value_of(&labels).as_deref(), Some("high"));
    }

    #[test]
    fn catalogue_covers_every_signal_and_names_match() {
        let catalogue = catalogue();
        assert_eq!(catalogue.len(), Signal::ALL.len());
        for doc in &catalogue {
            assert_eq!(doc.name, doc.signal.name());
            assert_eq!(doc.labels, documented_labels(doc.signal));
            assert!(!doc.description.is_empty());
        }
        let markdown = catalogue_markdown();
        for signal in Signal::ALL {
            assert!(markdown.contains(signal.name()), "{signal:?}");
        }
    }

    #[test]
    fn label_keys_have_distinct_names() {
        let mut names: Vec<&str> = LabelKey::ALL.iter().map(|k| k.as_str()).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique);
    }
}
