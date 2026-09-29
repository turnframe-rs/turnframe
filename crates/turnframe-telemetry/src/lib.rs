//! `turnframe-telemetry`: metrics, tracing and dashboards for a Turnframe runtime.
//!
//! The runtime reports what it did through one small contract in
//! [`turnframe_core::observe`]; this crate ships the implementations of it an
//! application wants in production, plus the data to build a dashboard from
//! them. Every metric of §26.2, the latencies of §28, the identifiers of §26.1
//! as tracing fields, one span per provider call described with the
//! OpenTelemetry GenAI conventions, and a vendor-neutral [`TraceGrouping`] that
//! reaches every span rather than only the trace root.
//!
//! Two rules decide what it will not do, and
//! [`docs/telemetry.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/telemetry.md)
//! says why. **Prompt and completion text stays out by default** —
//! [`ContentRecorder`] is the only way in, is disabled unless an application
//! enables it, and passes every string through a redactor the application
//! wrote. **No user text and no case text ever becomes a metric label**, which
//! is enforced three ways rather than asked for.
//!
//! ```rust
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! use turnframe_core::ids::WorkflowKey;
//! use turnframe_core::observe::{Observer, Signal, SignalLabels};
//! use turnframe_telemetry::{CompositeObserver, MetricsObserver, TracingObserver, metrics};
//!
//! // Once, after the metrics exporter is installed.
//! metrics::describe_all();
//!
//! let observer: Arc<dyn Observer> = Arc::new(
//!     CompositeObserver::new()
//!         .with(MetricsObserver::new())
//!         .with(TracingObserver::new()),
//! );
//!
//! // The runtime then reports what it did:
//! let labels = SignalLabels::workflow(WorkflowKey::from("trip"));
//! observer.observe_labeled(&Signal::TurnReceived, &labels);
//! observer.observe_duration(&Signal::TurnDuration, Duration::from_millis(412), &labels);
//! ```
//!
//! In tests, swap the composite for a [`RecordingObserver`] and assert on the
//! signals the code under test produced.
//!
//! # Modules
//!
//! * [`crate::metrics`]: the [`MetricsObserver`], the label rules and the metric
//!   catalogue that generates this crate's README table.
//! * [`crate::tracing`]: the [`TracingObserver`], the §26.1 identifier fields and
//!   the turn and stage span helpers.
//! * [`crate::attrs`]: every attribute key in one place, so an adapter and an
//!   application spell them identically.
//! * [`crate::composite`]: [`CompositeObserver`] and [`RecordingObserver`].
//! * [`crate::dashboard`]: a data-only description of the §26.3 dashboard.
//! * [`crate::otel`] (feature `otel`): the same signals as OpenTelemetry
//!   instruments, and the grouping carried in baggage and stamped onto every
//!   span by [`GroupingSpanProcessor`].
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod attrs;
pub mod composite;
pub mod dashboard;
pub mod metrics;
#[cfg(feature = "otel")]
pub mod otel;
pub mod tracing;

pub use crate::composite::{CompositeObserver, RecordedSignal, RecordingObserver};
pub use crate::dashboard::{Dashboard, Panel, PanelId, PanelSource};
pub use crate::metrics::{LabelKey, MetricDoc, MetricsObserver};
pub use crate::tracing::{
    ContentRecorder, ContentRedactor, ContentRole, PipelineStage, ProviderCall, TraceGrouping,
    TracingObserver, TurnIdentifiers,
};

#[cfg(feature = "otel")]
pub use crate::otel::{GroupingSpanProcessor, OtelObserver};

/// Borrows an optional string-like identifier as a `&str`.
///
/// Written against [`AsRef<str>`] rather than a concrete type so it keeps
/// working whether a `turnframe-core` label field is a `String` or one of the
/// string newtypes of [`turnframe_core::ids`].
pub(crate) fn opt_str<T: AsRef<str>>(value: &Option<T>) -> Option<&str> {
    value.as_ref().map(AsRef::as_ref)
}

/// Renders a unit-variant enum as its serde name (e.g. `RiskClass::ReadOnly` →
/// `"read_only"`).
///
/// Deriving the label from the serialization instead of a `match` means a new
/// variant in a `turnframe-core` enum gets a correct label without a change
/// here, and no `#[non_exhaustive]` wildcard can silently mislabel one.
/// Anything that does not serialize to a plain string yields `None` and is
/// dropped.
pub(crate) fn enum_label<T: serde::Serialize + ?Sized>(value: &T) -> Option<String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use turnframe_core::command::RiskClass;
    use turnframe_core::ids::WorkflowKey;
    use turnframe_core::interaction::InteractionKind;

    use super::{enum_label, opt_str};

    #[test]
    fn opt_str_borrows_strings_and_newtypes() {
        assert_eq!(opt_str(&Some(String::from("openai"))), Some("openai"));
        assert_eq!(opt_str(&Some(WorkflowKey::from("trip"))), Some("trip"));
        assert_eq!(opt_str::<String>(&None), None);
    }

    #[test]
    fn enum_label_uses_the_serde_name() {
        assert_eq!(
            enum_label(&RiskClass::ReadOnly).as_deref(),
            Some("read_only")
        );
        assert_eq!(
            enum_label(&RiskClass::ExternalRegulated).as_deref(),
            Some("external_regulated")
        );
        assert_eq!(
            enum_label(&InteractionKind::ConfirmCommand).as_deref(),
            Some("confirm_command")
        );
    }

    #[test]
    fn enum_label_rejects_non_string_shapes() {
        assert_eq!(enum_label(&42_u32), None);
        assert_eq!(enum_label(&vec!["a"]), None);
    }
}
