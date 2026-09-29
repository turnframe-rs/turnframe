//! Combining observers, and the observer tests assert against.
//!
//! An application usually wants more than one destination for the same signal:
//! a counter for the dashboard, a log line for the investigation, and — in a
//! test — a list it can make assertions about. [`CompositeObserver`] fans one
//! signal out to several observers; [`RecordingObserver`] keeps every signal it
//! was given so another crate's tests can check what a run reported.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use turnframe_core::observe::{Observer, Signal, SignalLabels};

/// Fans every signal out to several observers, in registration order.
///
/// ```rust
/// use std::sync::Arc;
///
/// use turnframe_core::observe::{Observer, Signal};
/// use turnframe_telemetry::{CompositeObserver, MetricsObserver, RecordingObserver};
///
/// let recorder = Arc::new(RecordingObserver::new());
/// let observer = CompositeObserver::new()
///     .with(MetricsObserver::new())
///     .with_shared(Arc::clone(&recorder));
///
/// observer.observe(&Signal::TurnReceived);
/// assert_eq!(recorder.count(Signal::TurnReceived), 1);
/// ```
#[derive(Clone, Default)]
pub struct CompositeObserver {
    observers: Vec<Arc<dyn Observer>>,
}

impl CompositeObserver {
    /// An empty composite. Signals given to it go nowhere.
    #[must_use]
    pub fn new() -> Self {
        Self {
            observers: Vec::new(),
        }
    }

    /// Adds an observer, taking ownership of it.
    #[must_use]
    pub fn with(mut self, observer: impl Observer + 'static) -> Self {
        self.observers.push(Arc::new(observer));
        self
    }

    /// Adds an observer the caller keeps a handle to, which is what a test
    /// needs in order to inspect a [`RecordingObserver`] afterwards.
    ///
    /// Generic over the concrete observer so `Arc::clone(&handle)` can be
    /// passed straight in; use [`CompositeObserver::push`] for a handle that is
    /// already erased to `Arc<dyn Observer>`.
    #[must_use]
    pub fn with_shared<O: Observer + 'static>(mut self, observer: Arc<O>) -> Self {
        self.observers.push(observer);
        self
    }

    /// Adds an already-erased observer to an existing composite, for a
    /// registry assembled at run time.
    pub fn push(&mut self, observer: Arc<dyn Observer>) {
        self.observers.push(observer);
    }

    /// How many observers are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.observers.len()
    }

    /// Returns `true` when no observer is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.observers.is_empty()
    }
}

impl fmt::Debug for CompositeObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompositeObserver")
            .field("observers", &self.observers.len())
            .finish()
    }
}

impl Observer for CompositeObserver {
    fn observe(&self, signal: &Signal) {
        for observer in &self.observers {
            observer.observe(signal);
        }
    }

    fn observe_labeled(&self, signal: &Signal, labels: &SignalLabels) {
        for observer in &self.observers {
            observer.observe_labeled(signal, labels);
        }
    }

    fn observe_duration(&self, signal: &Signal, duration: Duration, labels: &SignalLabels) {
        for observer in &self.observers {
            observer.observe_duration(signal, duration, labels);
        }
    }
}

/// One signal as a [`RecordingObserver`] kept it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecordedSignal {
    /// The signal.
    pub signal: Signal,
    /// The labels it arrived with; empty when it arrived without any.
    pub labels: SignalLabels,
    /// The measured duration, for a latency signal.
    pub duration: Option<Duration>,
}

/// An observer that keeps everything it was given, for assertions in tests.
///
/// It is cheap, thread-safe and never panics: a poisoned lock is recovered
/// rather than propagated, because a telemetry sink must not turn one test
/// failure into a cascade.
///
/// ```rust
/// use turnframe_core::ids::WorkflowKey;
/// use turnframe_core::observe::{Observer, Signal, SignalLabels};
/// use turnframe_telemetry::RecordingObserver;
///
/// let observer = RecordingObserver::new();
/// observer.observe_labeled(
///     &Signal::CommandExecuted,
///     &SignalLabels::workflow(WorkflowKey::from("trip")),
/// );
///
/// assert_eq!(observer.count(Signal::CommandExecuted), 1);
/// assert!(observer.contains(Signal::CommandExecuted));
/// assert_eq!(observer.labels_of(Signal::CommandExecuted).len(), 1);
/// ```
#[derive(Debug, Default)]
pub struct RecordingObserver {
    records: Mutex<Vec<RecordedSignal>>,
}

impl RecordingObserver {
    /// An observer with nothing recorded yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn guard(&self) -> MutexGuard<'_, Vec<RecordedSignal>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Everything recorded so far, in arrival order.
    #[must_use]
    pub fn records(&self) -> Vec<RecordedSignal> {
        self.guard().clone()
    }

    /// The signals recorded so far, in arrival order.
    #[must_use]
    pub fn signals(&self) -> Vec<Signal> {
        self.guard().iter().map(|record| record.signal).collect()
    }

    /// How many times a signal was recorded.
    #[must_use]
    pub fn count(&self, signal: Signal) -> usize {
        self.guard()
            .iter()
            .filter(|record| record.signal == signal)
            .count()
    }

    /// Whether a signal was recorded at least once.
    #[must_use]
    pub fn contains(&self, signal: Signal) -> bool {
        self.guard().iter().any(|record| record.signal == signal)
    }

    /// The label sets a signal was recorded with, in arrival order.
    #[must_use]
    pub fn labels_of(&self, signal: Signal) -> Vec<SignalLabels> {
        self.guard()
            .iter()
            .filter(|record| record.signal == signal)
            .map(|record| record.labels.clone())
            .collect()
    }

    /// The durations a latency signal was recorded with, in arrival order.
    #[must_use]
    pub fn durations_of(&self, signal: Signal) -> Vec<Duration> {
        self.guard()
            .iter()
            .filter(|record| record.signal == signal)
            .filter_map(|record| record.duration)
            .collect()
    }

    /// How many signals were recorded in total.
    #[must_use]
    pub fn len(&self) -> usize {
        self.guard().len()
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.guard().is_empty()
    }

    /// Drops everything recorded so far.
    pub fn clear(&self) {
        self.guard().clear();
    }

    fn push(&self, signal: Signal, labels: &SignalLabels, duration: Option<Duration>) {
        self.guard().push(RecordedSignal {
            signal,
            labels: labels.clone(),
            duration,
        });
    }
}

impl Observer for RecordingObserver {
    fn observe(&self, signal: &Signal) {
        self.push(*signal, &SignalLabels::none(), None);
    }

    fn observe_labeled(&self, signal: &Signal, labels: &SignalLabels) {
        self.push(*signal, labels, None);
    }

    fn observe_duration(&self, signal: &Signal, duration: Duration, labels: &SignalLabels) {
        self.push(*signal, labels, Some(duration));
    }
}

#[cfg(test)]
mod tests {
    use turnframe_core::ids::WorkflowKey;

    use super::*;

    #[test]
    fn a_composite_fans_out_to_every_observer() {
        let first = Arc::new(RecordingObserver::new());
        let second = Arc::new(RecordingObserver::new());
        let composite = CompositeObserver::new()
            .with_shared(Arc::clone(&first))
            .with_shared(Arc::clone(&second));

        assert_eq!(composite.len(), 2);
        assert!(!composite.is_empty());

        composite.observe(&Signal::TurnReceived);
        composite.observe_labeled(
            &Signal::CommandExecuted,
            &SignalLabels::workflow(WorkflowKey::from("trip")),
        );
        composite.observe_duration(
            &Signal::TurnDuration,
            Duration::from_millis(12),
            &SignalLabels::none(),
        );

        for recorder in [&first, &second] {
            assert_eq!(recorder.len(), 3);
            assert_eq!(recorder.count(Signal::TurnReceived), 1);
            assert_eq!(
                recorder.labels_of(Signal::CommandExecuted)[0]
                    .workflow
                    .as_ref()
                    .map(WorkflowKey::as_str),
                Some("trip")
            );
            assert_eq!(
                recorder.durations_of(Signal::TurnDuration),
                vec![Duration::from_millis(12)]
            );
        }
    }

    #[test]
    fn an_empty_composite_is_a_sink() {
        let composite = CompositeObserver::new();
        assert!(composite.is_empty());
        assert_eq!(composite.len(), 0);
        composite.observe(&Signal::TurnReceived);
        assert_eq!(
            format!("{composite:?}"),
            "CompositeObserver { observers: 0 }"
        );
    }

    #[test]
    fn with_takes_ownership_of_an_observer() {
        let composite = CompositeObserver::new().with(RecordingObserver::new());
        assert_eq!(composite.len(), 1);
        composite.observe(&Signal::TurnCompleted);
    }

    #[test]
    fn push_adds_to_an_existing_composite() {
        let recorder = Arc::new(RecordingObserver::new());
        let erased: Arc<dyn Observer> = recorder.clone();
        let mut composite = CompositeObserver::new();
        composite.push(erased);
        composite.observe(&Signal::QuestionAnswered);
        assert!(recorder.contains(Signal::QuestionAnswered));
    }

    #[test]
    fn a_recording_observer_keeps_order_and_clears() {
        let observer = RecordingObserver::new();
        assert!(observer.is_empty());
        observer.observe(&Signal::TurnReceived);
        observer.observe(&Signal::TurnCompleted);
        assert_eq!(
            observer.signals(),
            vec![Signal::TurnReceived, Signal::TurnCompleted]
        );
        assert_eq!(observer.records().len(), 2);
        assert!(!observer.contains(Signal::TurnFailed));
        observer.clear();
        assert!(observer.is_empty());
        assert_eq!(observer.count(Signal::TurnReceived), 0);
    }

    #[test]
    fn durations_are_only_kept_for_measured_signals() {
        let observer = RecordingObserver::new();
        observer.observe_labeled(&Signal::TurnDuration, &SignalLabels::none());
        assert!(observer.durations_of(Signal::TurnDuration).is_empty());
        observer.observe_duration(
            &Signal::TurnDuration,
            Duration::from_micros(900),
            &SignalLabels::none(),
        );
        assert_eq!(
            observer.durations_of(Signal::TurnDuration),
            vec![Duration::from_micros(900)]
        );
    }
}
