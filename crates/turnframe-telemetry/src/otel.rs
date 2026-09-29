//! OpenTelemetry bridge (feature `otel`).
//!
//! The same signals, the same metric names and the same labels as
//! [`crate::metrics`], recorded on OpenTelemetry instruments instead of the
//! `metrics` facade. An application that already exports OTLP can therefore
//! drop the `metrics` exporter entirely and still get the catalogue this crate
//! documents, byte for byte.
//!
//! Instruments are built once, up front, from
//! [`Signal::ALL`](turnframe_core::observe::Signal::ALL): counters for
//! occurrence signals and `f64` histograms for the latency signals of spec §28,
//! each carrying the description and unit of the catalogue. Attributes come
//! from the same [`crate::metrics::metric_labels`] the `metrics` observer uses,
//! so a dimension that is unsafe on one backend is unsafe on neither.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use opentelemetry::baggage::BaggageExt;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry::trace::Span as _;
use opentelemetry::{Context, ContextGuard, KeyValue};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{Span, SpanData, SpanProcessor};
use turnframe_core::observe::{Observer, Signal, SignalKind, SignalLabels};

use crate::metrics::{description, metric_labels};
use crate::tracing::{ProviderCall, TraceGrouping};

/// OpenTelemetry unit of a millisecond histogram (UCUM).
const UNIT_MILLIS: &str = "ms";
/// OpenTelemetry unit of a microsecond histogram (UCUM).
const UNIT_MICROS: &str = "us";
/// OpenTelemetry unit of a dimensionless counter (UCUM).
const UNIT_COUNT: &str = "1";

/// An [`Observer`] that records signals on OpenTelemetry instruments.
///
/// ```rust,ignore
/// // Requires an SDK meter provider; the crate itself depends only on the API.
/// use opentelemetry::global;
/// use turnframe_telemetry::otel::OtelObserver;
///
/// let observer = OtelObserver::new(&global::meter("turnframe"));
/// ```
pub struct OtelObserver {
    counters: HashMap<Signal, Counter<u64>>,
    histograms: HashMap<Signal, Histogram<f64>>,
}

impl OtelObserver {
    /// Builds every instrument of the catalogue on `meter`.
    ///
    /// Instruments are created once and reused, which is what the
    /// OpenTelemetry API asks for: creating duplicates of the same instrument
    /// costs the SDK performance.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        let mut counters = HashMap::new();
        let mut histograms = HashMap::new();

        for signal in Signal::ALL {
            match signal.kind() {
                SignalKind::DurationMillis | SignalKind::DurationMicros => {
                    let unit = if matches!(signal.kind(), SignalKind::DurationMicros) {
                        UNIT_MICROS
                    } else {
                        UNIT_MILLIS
                    };
                    let histogram = meter
                        .f64_histogram(signal.name())
                        .with_description(description(signal))
                        .with_unit(unit)
                        .build();
                    histograms.insert(signal, histogram);
                }
                _ => {
                    let counter = meter
                        .u64_counter(signal.name())
                        .with_description(description(signal))
                        .with_unit(UNIT_COUNT)
                        .build();
                    counters.insert(signal, counter);
                }
            }
        }

        Self {
            counters,
            histograms,
        }
    }

    /// How many instruments were built. Equal to the size of the catalogue.
    #[must_use]
    pub fn instrument_count(&self) -> usize {
        self.counters.len() + self.histograms.len()
    }

    /// The attributes emitted for a signal: exactly the labels of
    /// [`crate::metrics::metric_labels`], as OpenTelemetry key/value pairs.
    #[must_use]
    pub fn attributes(signal: Signal, labels: &SignalLabels) -> Vec<KeyValue> {
        metric_labels(signal, labels)
            .into_iter()
            .map(|label| {
                let (key, value) = label.into_parts();
                KeyValue::new(key.into_owned(), value.into_owned())
            })
            .collect()
    }

    fn record(&self, signal: Signal, labels: &SignalLabels, duration: Option<Duration>) {
        let attributes = Self::attributes(signal, labels);
        match signal.kind() {
            SignalKind::DurationMillis => {
                if let (Some(histogram), Some(duration)) = (self.histograms.get(&signal), duration)
                {
                    histogram.record(duration.as_secs_f64() * 1_000.0, &attributes);
                }
            }
            SignalKind::DurationMicros => {
                if let (Some(histogram), Some(duration)) = (self.histograms.get(&signal), duration)
                {
                    histogram.record(duration.as_secs_f64() * 1_000_000.0, &attributes);
                }
            }
            _ => {
                if let Some(counter) = self.counters.get(&signal) {
                    counter.add(1, &attributes);
                }
            }
        }
    }
}

impl fmt::Debug for OtelObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtelObserver")
            .field("counters", &self.counters.len())
            .field("histograms", &self.histograms.len())
            .finish()
    }
}

impl Observer for OtelObserver {
    fn observe(&self, signal: &Signal) {
        self.record(*signal, &SignalLabels::none(), None);
    }

    fn observe_labeled(&self, signal: &Signal, labels: &SignalLabels) {
        self.record(*signal, labels, None);
    }

    fn observe_duration(&self, signal: &Signal, duration: Duration, labels: &SignalLabels) {
        self.record(*signal, labels, Some(duration));
    }
}

// ---------------------------------------------------------------------------
// GenAI semantic conventions and trace grouping
// ---------------------------------------------------------------------------

/// The GenAI attributes of a provider call, as OpenTelemetry key/values.
///
/// Exactly the pairs of [`ProviderCall::attributes`], with the same keys from
/// [`crate::attrs`], so the `tracing` span and the OpenTelemetry span carry the
/// same description of the same call. Token usage keeps the same rule: the
/// input-token attribute is net of cached tokens, so a consumer that reads both
/// never double counts.
///
/// ```rust,ignore
/// use turnframe_telemetry::otel::provider_call_attributes;
/// use turnframe_telemetry::tracing::ProviderCall;
///
/// let call = ProviderCall::new("openai", "extract", "gpt-x")
///     .with_cached_usage(1_000, 800, 120);
/// let attributes = provider_call_attributes(&call);
/// ```
#[must_use]
pub fn provider_call_attributes(call: &ProviderCall) -> Vec<KeyValue> {
    call.attributes()
        .into_iter()
        .map(|(key, value)| KeyValue::new(key, value))
        .collect()
}

/// The trace grouping as OpenTelemetry key/values.
#[must_use]
pub fn grouping_attributes(grouping: &TraceGrouping) -> Vec<KeyValue> {
    grouping
        .fields()
        .into_iter()
        .map(|(key, value)| KeyValue::new(key, value))
        .collect()
}

/// Attaches the trace grouping to the current OpenTelemetry context as
/// baggage, and returns the guard that detaches it.
///
/// Baggage is what makes the grouping reach *every* span rather than only the
/// root: these backends filter at the level of the individual span, so a
/// session id that sits on the trace root alone is not enough. Anything started
/// while the guard is alive — including work in other libraries — sees the same
/// baggage on its context.
///
/// Copying the baggage onto each span is a job for a span processor, and a
/// processor needs `opentelemetry_sdk`, which this crate deliberately does not
/// depend on. It is a few lines in the application's telemetry setup:
/// implement `on_start` to read [`grouping_from_baggage`] out of the incoming
/// context and set the returned attributes on the span.
///
/// ```rust,ignore
/// use turnframe_core::ids::{AccountId, ConversationId};
/// use turnframe_telemetry::otel::attach_grouping;
/// use turnframe_telemetry::tracing::TraceGrouping;
///
/// let grouping = TraceGrouping::for_conversation(ConversationId::nil())
///     .with_account(&AccountId::from("acct-1"))
///     .with_environment("production");
/// let _guard = attach_grouping(&grouping);
/// // ... run the turn; every span started here carries the grouping baggage.
/// ```
#[must_use]
pub fn attach_grouping(grouping: &TraceGrouping) -> ContextGuard {
    Context::map_current(|current| current.with_baggage(grouping_attributes(grouping))).attach()
}

/// The grouping attributes carried in a context's baggage.
///
/// This is what a baggage-copying span processor sets on every starting span.
/// Only the keys of [`crate::attrs::GROUPING_KEYS`] are returned, so unrelated
/// baggage an application put on the context is never copied onto spans by
/// accident.
#[must_use]
pub fn grouping_from_baggage(context: &Context) -> Vec<KeyValue> {
    let baggage = context.baggage();
    crate::attrs::GROUPING_KEYS
        .iter()
        .filter_map(|key| {
            baggage
                .get(*key)
                .map(|value| KeyValue::new(*key, value.as_str().to_owned()))
        })
        .collect()
}

/// A span processor that copies the trace grouping out of OpenTelemetry baggage onto every
/// span that starts underneath it.
///
/// LLM observability backends filter on each span, not only the trace root, so a
/// `session.id` on the turn's first span leaves its children unattributable.
/// [`attach_grouping`] puts the grouping in baggage and this turns it into attributes as each
/// span starts; only the keys of [`crate::attrs::GROUPING_KEYS`] are copied, so an
/// application's own baggage never leaks into telemetry. Hold the [`attach_grouping`] guard
/// for the whole turn: a span that already started cannot gain attributes.
///
/// ```rust,no_run
/// use opentelemetry::trace::{Tracer, TracerProvider};
/// use opentelemetry_sdk::trace::SdkTracerProvider;
/// use turnframe_core::ids::{AccountId, ConversationId};
/// use turnframe_telemetry::otel::{GroupingSpanProcessor, attach_grouping};
/// use turnframe_telemetry::tracing::TraceGrouping;
///
/// let provider = SdkTracerProvider::builder()
///     .with_span_processor(GroupingSpanProcessor::new())
///     // .with_span_processor(your_exporting_processor)
///     .build();
/// let tracer = provider.tracer("turnframe");
///
/// let grouping = TraceGrouping::for_conversation(ConversationId::nil())
///     .with_account(&AccountId::from("acct-1"))
///     .with_environment("production");
///
/// // Attach first, then open spans: every span started while the guard is
/// // alive carries session.id, user.id, deployment.environment.name.
/// let _guard = attach_grouping(&grouping);
/// let mut span = tracer.start("turnframe.turn");
/// # use opentelemetry::trace::Span as _;
/// span.end();
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct GroupingSpanProcessor;

impl GroupingSpanProcessor {
    /// Builds the processor. It holds no state and never blocks.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl SpanProcessor for GroupingSpanProcessor {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        for attribute in grouping_from_baggage(cx) {
            span.set_attribute(attribute);
        }
    }

    /// Nothing to do: the grouping was already stamped at `on_start`, which is
    /// what the OpenTelemetry SDK asks for — a context read at `on_end` would
    /// see whatever context happens to be active, not the span's own.
    fn on_end(&self, _span: SpanData) {}

    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use opentelemetry::metrics::{MeterProvider, NoopMeterProvider};
    use opentelemetry::trace::{TraceContextExt, Tracer, TracerProvider};
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
    use turnframe_core::command::RiskClass;
    use turnframe_core::ids::{AccountId, ConversationId, WorkflowKey};

    use super::*;

    #[test]
    fn every_signal_gets_exactly_one_instrument() {
        let observer = OtelObserver::new(&NoopMeterProvider::new().meter("turnframe"));
        assert_eq!(observer.instrument_count(), Signal::ALL.len());
        for signal in Signal::ALL {
            let counted = observer.counters.contains_key(&signal);
            let measured = observer.histograms.contains_key(&signal);
            assert!(counted ^ measured, "{signal:?}");
        }
    }

    #[test]
    fn attributes_match_the_metrics_label_set() {
        let labels = SignalLabels::workflow(WorkflowKey::from("trip"))
            .with_risk(RiskClass::Destructive)
            .with_error_code("trip.traveler_missing");
        for signal in Signal::ALL {
            let expected = metric_labels(signal, &labels);
            let attributes = OtelObserver::attributes(signal, &labels);
            assert_eq!(attributes.len(), expected.len(), "{signal:?}");
            for (attribute, label) in attributes.iter().zip(expected.iter()) {
                assert_eq!(attribute.key.as_str(), label.key(), "{signal:?}");
                assert_eq!(attribute.value.to_string(), label.value(), "{signal:?}");
            }
        }
    }

    #[test]
    fn recording_through_a_noop_meter_is_harmless() {
        let observer = OtelObserver::new(&NoopMeterProvider::new().meter("turnframe"));
        for signal in Signal::ALL {
            observer.observe(&signal);
            observer.observe_labeled(&signal, &SignalLabels::none());
            observer.observe_duration(&signal, Duration::from_millis(2), &SignalLabels::none());
        }
        assert!(format!("{observer:?}").starts_with("OtelObserver"));
    }

    #[test]
    fn provider_call_attributes_mirror_the_tracing_ones() {
        let call = ProviderCall::new("openai", "extract", "gpt-x")
            .with_temperature(0.0)
            .with_response("resp-1", "gpt-x")
            .with_finish_reason("stop")
            .with_cached_usage(1_000, 800, 120);

        let expected = call.attributes();
        let attributes = provider_call_attributes(&call);
        assert_eq!(attributes.len(), expected.len());
        for (attribute, (key, value)) in attributes.iter().zip(expected.iter()) {
            assert_eq!(attribute.key.as_str(), *key);
            assert_eq!(attribute.value.to_string(), *value);
        }

        let input = attributes
            .iter()
            .find(|kv| kv.key.as_str() == crate::attrs::GEN_AI_USAGE_INPUT_TOKENS)
            .expect("input tokens");
        assert_eq!(
            input.value.to_string(),
            "200",
            "input tokens must be net of cache"
        );
    }

    /// Keeps every finished span so a test can look at the attributes the
    /// grouping processor stamped on it during `on_start`.
    #[derive(Debug, Clone, Default)]
    struct CapturingProcessor {
        spans: Arc<Mutex<Vec<SpanData>>>,
    }

    impl SpanProcessor for CapturingProcessor {
        fn on_start(&self, _span: &mut Span, _cx: &Context) {}

        fn on_end(&self, span: SpanData) {
            self.spans
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(span);
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }
    }

    /// A provider with the grouping processor in front of a capturing one, and
    /// the handle the test reads the finished spans back from.
    fn provider_with_grouping() -> (SdkTracerProvider, Arc<Mutex<Vec<SpanData>>>) {
        let capture = CapturingProcessor::default();
        let spans = Arc::clone(&capture.spans);
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_span_processor(GroupingSpanProcessor::new())
            .with_span_processor(capture)
            .build();
        (provider, spans)
    }

    fn span_named(spans: &Arc<Mutex<Vec<SpanData>>>, name: &str) -> SpanData {
        spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|span| span.name == name)
            .unwrap_or_else(|| panic!("span {name} was never finished"))
            .clone()
    }

    fn attributes_of(spans: &Arc<Mutex<Vec<SpanData>>>, name: &str) -> Vec<KeyValue> {
        span_named(spans, name).attributes
    }

    fn attribute_value(attributes: &[KeyValue], key: &str) -> Option<String> {
        attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    }

    #[test]
    fn the_grouping_lands_on_a_child_span_not_only_on_the_root() {
        let (provider, spans) = provider_with_grouping();
        let tracer = provider.tracer("turnframe");

        let grouping = TraceGrouping::for_conversation(ConversationId::nil())
            .with_account(&AccountId::from("acct-1"))
            .with_environment("production")
            .with_release("v0.1.0")
            .with_tag("trip");

        {
            // Ordering: the grouping is attached before any span is opened.
            let _grouping_guard = attach_grouping(&grouping);
            let parent = tracer.start("turnframe.turn");
            // Make the parent current so the next span is genuinely its child.
            let parent_guard = Context::current_with_span(parent).attach();
            let mut child = tracer.start("turnframe.stage");
            child.end();
            drop(parent_guard);
        }

        let parent = span_named(&spans, "turnframe.turn");
        let child = span_named(&spans, "turnframe.stage");
        assert_eq!(
            child.parent_span_id,
            parent.span_context.span_id(),
            "the second span must really be a child of the first"
        );

        for (key, value) in grouping.fields() {
            assert_eq!(
                attribute_value(&child.attributes, key).as_deref(),
                Some(value.as_str()),
                "{key} did not reach the child span"
            );
        }
        // The raw tenant identifier is nowhere on the span.
        assert!(
            child
                .attributes
                .iter()
                .all(|kv| kv.value.to_string() != "acct-1")
        );

        // The root carries it too, so a trace-level filter still works.
        assert_eq!(
            attribute_value(&parent.attributes, crate::attrs::SESSION_ID).as_deref(),
            Some(ConversationId::nil().to_string().as_str())
        );
    }

    #[test]
    fn unrelated_baggage_never_becomes_a_span_attribute() {
        let (provider, spans) = provider_with_grouping();
        let tracer = provider.tracer("turnframe");

        {
            let context = Context::map_current(|current| {
                current.with_baggage([
                    KeyValue::new(crate::attrs::SESSION_ID, "sess-1"),
                    KeyValue::new("internal.traveler_email", "someone@example.com"),
                ])
            });
            let _guard = context.attach();
            let mut span = tracer.start("turnframe.stage");
            span.end();
        }

        let attributes = attributes_of(&spans, "turnframe.stage");
        assert_eq!(
            attribute_value(&attributes, crate::attrs::SESSION_ID).as_deref(),
            Some("sess-1"),
            "the grouping key should still be copied"
        );
        assert_eq!(
            attribute_value(&attributes, "internal.traveler_email"),
            None,
            "baggage outside the grouping keys must not leak onto spans"
        );
        assert!(
            attributes
                .iter()
                .all(|kv| crate::attrs::GROUPING_KEYS.contains(&kv.key.as_str()))
        );
    }

    #[test]
    fn a_span_opened_before_the_grouping_is_attached_carries_nothing() {
        let (provider, spans) = provider_with_grouping();
        let tracer = provider.tracer("turnframe");
        let grouping = TraceGrouping::for_conversation(ConversationId::nil());

        // Ordering matters: on_start reads the context at creation time, so a
        // span opened first can never be back-filled.
        let mut early = tracer.start("turnframe.early");
        let guard = attach_grouping(&grouping);
        early.end();
        drop(guard);

        assert!(attributes_of(&spans, "turnframe.early").is_empty());
    }

    #[test]
    fn grouping_travels_through_baggage_and_comes_back_whole() {
        let grouping = TraceGrouping::for_conversation(ConversationId::nil())
            .with_account(&AccountId::from("acct-1"))
            .with_environment("production")
            .with_release("v0.1.0")
            .with_tag("trip");

        let guard = attach_grouping(&grouping);
        let recovered = Context::map_current(grouping_from_baggage);
        drop(guard);

        let expected = grouping_attributes(&grouping);
        assert_eq!(recovered.len(), expected.len());
        for (key, value) in expected
            .iter()
            .map(|kv| (kv.key.as_str(), kv.value.to_string()))
        {
            let found = recovered
                .iter()
                .find(|kv| kv.key.as_str() == key)
                .unwrap_or_else(|| panic!("{key} missing from baggage"));
            assert_eq!(found.value.to_string(), value);
        }
        // Nothing outside the documented grouping keys is copied.
        assert!(
            recovered
                .iter()
                .all(|kv| crate::attrs::GROUPING_KEYS.contains(&kv.key.as_str()))
        );
    }
}
