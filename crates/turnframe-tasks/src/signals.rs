//! The provider signals of every model call a task makes (spec §26.2): latency, the
//! narration latency of calls after the commit, fallbacks, and capability mismatches.
//! Labels are configured keys, normalized purposes and closed codes, never text.

use turnframe_core::observe::{Observer, Signal, SignalLabels};
use turnframe_provider::capabilities::MissingCapability;
use turnframe_provider::fallback::{AttemptOutcome, FallbackStage, ProviderAttempt};
use turnframe_provider::ids::ModelRef;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::router::{RejectionReason, RoutingError};

/// The provider dimensions of one model call: the configured provider and model
/// keys and the normalized purpose (spec §20.2).
pub(crate) fn provider_labels(model: &ModelRef, purpose: ModelPurpose) -> SignalLabels {
    SignalLabels::none()
        .with_provider(model.provider.clone())
        .with_model(model.model.clone())
        .with_purpose(purpose.as_str())
}

/// Reports one provider attempt: its latency, its narration latency when it was
/// made after commit, and the fallback it caused when it moved on.
///
/// Called once per attempt, by the stage that made it, so an attempt that never
/// reaches the orchestrator — every attempt of a task that ends in an
/// error — is still counted.
pub(crate) fn observe_attempt(observer: &dyn Observer, attempt: &ProviderAttempt) {
    let labels = provider_labels(&attempt.model, attempt.purpose);
    observer.observe_duration(&Signal::ProviderLatency, attempt.latency, &labels);
    if attempt.stage == FallbackStage::PostCommitNarration {
        observer.observe_duration(&Signal::NarrationLatency, attempt.latency, &labels);
    }
    if let AttemptOutcome::FellBack { code } = &attempt.outcome {
        observer.observe_labeled(
            &Signal::ProviderFallback,
            &labels.with_error_code(code.clone()),
        );
    }
}

/// The same, for a whole trail at once.
pub(crate) fn observe_attempts(observer: &dyn Observer, attempts: &[ProviderAttempt]) {
    for attempt in attempts {
        observe_attempt(observer, attempt);
    }
}

/// Reports every candidate routing turned down for a capability it lacks
/// (spec §20.4, §20.6).
///
/// This is everything the runtime can see. [`ProviderRouter::select`] returns
/// its rejection list only when it admitted nobody at all, so a pool where a
/// weak profile is refused and a strong one answers produces no signal here:
/// the refusal never leaves the provider crate. Wiring the successful case
/// would mean widening the router's return type, which belongs to
/// `turnframe-provider`.
///
/// [`ProviderRouter::select`]: turnframe_provider::router::ProviderRouter::select
pub(crate) fn observe_routing_error(observer: &dyn Observer, error: &RoutingError) {
    let RoutingError::NoCandidate {
        purpose,
        rejections,
        ..
    } = error
    else {
        return;
    };
    for rejection in rejections {
        let RejectionReason::Capability(mismatch) = &rejection.reason else {
            continue;
        };
        let code = mismatch
            .missing
            .first()
            .map_or("capability", missing_capability_code);
        observer.observe_labeled(
            &Signal::ProviderCapabilityMismatch,
            &provider_labels(&rejection.model, *purpose).with_error_code(code),
        );
    }
}

/// Stable, bounded code of an unmet capability.
///
/// [`MissingCapability`] is `#[non_exhaustive]` and carries the required and
/// declared transports inside its variants; only the variant name is a safe
/// metric dimension, so only the variant name leaves here.
const fn missing_capability_code(missing: &MissingCapability) -> &'static str {
    match missing {
        MissingCapability::StructuredOutput { .. } => "structured_output",
        MissingCapability::ToolCalling => "tool_calling",
        MissingCapability::Streaming => "streaming",
        MissingCapability::Vision => "vision",
        MissingCapability::Documents => "documents",
        MissingCapability::ContextWindow { .. } => "context_window",
        _ => "capability",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;
    use std::time::Duration;

    use turnframe_provider::capabilities::{CapabilityMismatch, StructuredOutputCapability};
    use turnframe_provider::ids::{AttemptNumber, RequestId};
    use turnframe_provider::router::CandidateRejection;

    #[derive(Default)]
    struct Seen(Mutex<Vec<(Signal, SignalLabels)>>);

    impl Observer for Seen {
        fn observe(&self, signal: &Signal) {
            self.observe_labeled(signal, &SignalLabels::none());
        }

        fn observe_labeled(&self, signal: &Signal, labels: &SignalLabels) {
            if let Ok(mut seen) = self.0.lock() {
                seen.push((*signal, labels.clone()));
            }
        }
    }

    impl Seen {
        fn signals(&self) -> Vec<Signal> {
            self.0
                .lock()
                .map(|seen| seen.iter().map(|(signal, _)| *signal).collect())
                .unwrap_or_default()
        }

        fn labels_of(&self, signal: Signal) -> Vec<SignalLabels> {
            self.0
                .lock()
                .map(|seen| {
                    seen.iter()
                        .filter(|(found, _)| *found == signal)
                        .map(|(_, labels)| labels.clone())
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    fn attempt(stage: FallbackStage, outcome: AttemptOutcome) -> ProviderAttempt {
        ProviderAttempt {
            attempt: AttemptNumber::FIRST,
            request_id: RequestId::new(),
            purpose: ModelPurpose::Acknowledge,
            stage,
            model: ModelRef::new("openai", "gpt-x"),
            outcome,
            class: None,
            latency: Duration::from_millis(7),
            input_tokens: None,
            output_tokens: None,
            temperature: None,
            finish_reasons: Vec::new(),
        }
    }

    #[test]
    fn a_post_commit_attempt_reports_both_latencies_and_no_fallback() {
        let seen = Seen::default();
        observe_attempt(
            &seen,
            &attempt(
                FallbackStage::PostCommitNarration,
                AttemptOutcome::Succeeded,
            ),
        );
        assert_eq!(
            seen.signals(),
            vec![Signal::ProviderLatency, Signal::NarrationLatency]
        );
    }

    #[test]
    fn a_pre_commit_attempt_is_never_narration() {
        let seen = Seen::default();
        observe_attempt(
            &seen,
            &attempt(FallbackStage::PreCommit, AttemptOutcome::Succeeded),
        );
        assert_eq!(seen.signals(), vec![Signal::ProviderLatency]);
    }

    #[test]
    fn an_attempt_that_moved_on_reports_the_fallback_with_its_code() {
        let seen = Seen::default();
        observe_attempt(
            &seen,
            &attempt(
                FallbackStage::PreCommit,
                AttemptOutcome::FellBack {
                    code: "server_error".to_owned(),
                },
            ),
        );
        assert!(seen.signals().contains(&Signal::ProviderFallback));
        let labels = seen.labels_of(Signal::ProviderFallback);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].error_code.as_deref(), Some("server_error"));
        assert_eq!(labels[0].purpose.as_deref(), Some("acknowledge"));
    }

    #[test]
    fn only_capability_rejections_become_a_mismatch() {
        let seen = Seen::default();
        observe_routing_error(
            &seen,
            &RoutingError::NoCandidate {
                purpose: ModelPurpose::Extract,
                required_structured_output: Vec::new(),
                rejections: vec![
                    CandidateRejection {
                        model: ModelRef::new("local", "llama"),
                        reason: RejectionReason::Capability(CapabilityMismatch {
                            missing: vec![MissingCapability::StructuredOutput {
                                required: vec![StructuredOutputCapability::NativeJsonSchema],
                                declared: StructuredOutputCapability::PromptOnly,
                            }],
                        }),
                    },
                    CandidateRejection {
                        model: ModelRef::new("openai", "gpt-x"),
                        reason: RejectionReason::Denylisted,
                    },
                ],
            },
        );
        let labels = seen.labels_of(Signal::ProviderCapabilityMismatch);
        assert_eq!(labels.len(), 1, "a denylist is not a capability");
        assert_eq!(labels[0].error_code.as_deref(), Some("structured_output"));
    }

    #[test]
    fn an_empty_pool_names_no_candidate_to_blame() {
        let seen = Seen::default();
        observe_routing_error(
            &seen,
            &RoutingError::NoProvidersConfigured {
                purpose: ModelPurpose::Acknowledge,
            },
        );
        assert!(seen.signals().is_empty());
    }

    #[test]
    fn every_rejection_code_is_a_bounded_label() {
        for code in [
            missing_capability_code(&MissingCapability::ToolCalling),
            missing_capability_code(&MissingCapability::Streaming),
        ] {
            assert!(!code.is_empty());
            assert!(!code.contains(char::is_whitespace), "{code}");
        }
    }
}
