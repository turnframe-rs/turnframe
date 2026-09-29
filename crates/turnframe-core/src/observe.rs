//! Observability signals (spec §26.2, §28).
//!
//! [`Signal`] mirrors every metric of the spec under the `turnframe.` prefix,
//! plus the separately measured latencies of spec §28. An [`Observer`] receives
//! signals with optional structured [`SignalLabels`]; duration signals travel
//! through [`Observer::observe_duration`].
//!
//! **Never put user text, case text, quotes or free-form payloads in labels.**
//! Labels are identifiers and enumerations only (workflow key, provider key,
//! model key, request purpose, risk class, interaction kind, stable error
//! code). Case ids are deliberately absent: they belong in traces, not in
//! metric dimensions.

use std::time::Duration;

use crate::command::RiskClass;
use crate::ids::{ModelKey, OperationKey, ProviderKey, WorkflowKey};
use crate::interaction::InteractionKind;

/// A metric event. Names are stable and match spec §26.2 with the
/// `turnframe.` prefix; the duration variants follow spec §28.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Signal {
    /// A turn was received.
    TurnReceived,
    /// A turn completed.
    TurnCompleted,
    /// A turn failed.
    TurnFailed,
    /// A target was ambiguous.
    TargetAmbiguous,
    /// A target was missing.
    TargetMissing,
    /// An act named a target that produced no resolution at all: an act on the active
    /// card when none is open, a target shape the operation does not accept.
    ///
    /// Distinct from [`Self::TargetAmbiguous`] and [`Self::TargetMissing`], which are
    /// resolutions. This act leaves no resolution, no policy decision and no command,
    /// so without this nothing records why the turn did less; the label carries the
    /// rejection code.
    TargetUnresolved,
    /// The case directory refused to authorize a candidate for this actor.
    ///
    /// A candidate reaching the directory and being refused is ordinary in an
    /// application with a scope narrower than the account: a card outlives the
    /// scope it was written in and stops being addressable. A rate that climbs
    /// is not ordinary, and without this an operator has only a log line, so
    /// the one containment path below the account has no alert.
    CaseNotAuthorized,
    /// A command needs confirmation.
    CommandConfirmationRequired,
    /// A command executed.
    CommandExecuted,
    /// A command was rejected.
    CommandRejected,
    /// A command hit an idempotency replay.
    CommandIdempotencyReplay,
    /// A command hit a revision conflict.
    CommandRevisionConflict,
    /// An interaction was created.
    InteractionCreated,
    /// An interaction was resolved.
    InteractionResolved,
    /// An interaction response was stale.
    InteractionStale,
    /// An interaction failed.
    InteractionFailed,
    /// A receipt was emitted.
    ClaimReceiptEmitted,
    /// An external outcome is unknown.
    ExternalOutcomeUnknown,
    /// An external outcome was reconciled.
    ExternalReconciled,
    /// Provider fallback happened.
    ProviderFallback,
    /// A provider lacked a required capability.
    ProviderCapabilityMismatch,
    /// A workflow invariant was violated.
    WorkflowInvariantViolation,
    /// A question was answered.
    QuestionAnswered,
    /// A question could not be answered.
    QuestionUnanswered,
    /// An act the domain refused during reduction.
    ///
    /// Distinct from [`Self::CommandRejected`], which counts a command refused
    /// at execution: this one never became a command. Both are a turn doing
    /// less than its plan said, and until this existed only the second was
    /// countable.
    ActRefused,
    /// An act was dropped because a correction or a cancel in the same message replaced
    /// it. Usually right, and still a turn doing less than its acts said, so it is
    /// counted and labelled with the operation dropped.
    ActSuperseded,
    /// Wall-clock duration of a whole turn, from accepted input to persisted
    /// response, in milliseconds (spec §28).
    TurnDuration,
    /// Pure projection time of one case, in microseconds (spec §28).
    ProjectionDuration,
    /// Whole-turn reduction time, in microseconds (spec §28).
    ReductionDuration,
    /// Time spent in persistence for one turn, in milliseconds (spec §28).
    PersistenceDuration,
    /// Latency of one provider call, in milliseconds (spec §28).
    ProviderLatency,
    /// Latency of one external command dispatch, in milliseconds (spec §28).
    ExternalLatency,
    /// Latency of one call that writes or reviews the reply, in milliseconds (spec §28).
    NarrationLatency,
    /// A model task finished; labelled with its purpose and verdict.
    TaskCompleted,
    /// A model task's answer was sent back for a repair.
    TaskRepaired,
    /// A model task was escalated to a stronger model.
    TaskEscalated,
    /// The votes of a model task found no majority.
    TaskVoteDisagreement,
    /// A turn's model calls reached a bound; labelled with the bound.
    BudgetExhausted,
    /// Latency of one model task call, in milliseconds.
    TaskLatency,
}

/// How a [`Signal`] is measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignalKind {
    /// A monotonically increasing count of occurrences.
    Counter,
    /// A distribution of durations recorded in milliseconds.
    DurationMillis,
    /// A distribution of durations recorded in microseconds.
    DurationMicros,
}

impl Signal {
    /// Every signal, for exhaustive registration.
    pub const ALL: [Self; 39] = [
        Self::TurnReceived,
        Self::TurnCompleted,
        Self::TurnFailed,
        Self::TargetAmbiguous,
        Self::TargetUnresolved,
        Self::TargetMissing,
        Self::CaseNotAuthorized,
        Self::CommandConfirmationRequired,
        Self::CommandExecuted,
        Self::CommandRejected,
        Self::CommandIdempotencyReplay,
        Self::CommandRevisionConflict,
        Self::InteractionCreated,
        Self::InteractionResolved,
        Self::InteractionStale,
        Self::InteractionFailed,
        Self::ClaimReceiptEmitted,
        Self::ExternalOutcomeUnknown,
        Self::ExternalReconciled,
        Self::ProviderFallback,
        Self::ProviderCapabilityMismatch,
        Self::WorkflowInvariantViolation,
        Self::QuestionAnswered,
        Self::QuestionUnanswered,
        Self::ActSuperseded,
        Self::ActRefused,
        Self::TurnDuration,
        Self::ProjectionDuration,
        Self::ReductionDuration,
        Self::PersistenceDuration,
        Self::ProviderLatency,
        Self::ExternalLatency,
        Self::NarrationLatency,
        Self::TaskCompleted,
        Self::TaskRepaired,
        Self::TaskEscalated,
        Self::TaskVoteDisagreement,
        Self::BudgetExhausted,
        Self::TaskLatency,
    ];

    /// The metric name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TurnReceived => "turnframe.turn.received",
            Self::TurnCompleted => "turnframe.turn.completed",
            Self::TurnFailed => "turnframe.turn.failed",
            Self::TargetAmbiguous => "turnframe.target.ambiguous",
            Self::TargetUnresolved => "turnframe.target.unresolved",
            Self::TargetMissing => "turnframe.target.missing",
            Self::CaseNotAuthorized => "turnframe.case.not_authorized",
            Self::CommandConfirmationRequired => "turnframe.command.confirmation_required",
            Self::CommandExecuted => "turnframe.command.executed",
            Self::CommandRejected => "turnframe.command.rejected",
            Self::CommandIdempotencyReplay => "turnframe.command.idempotency_replay",
            Self::CommandRevisionConflict => "turnframe.command.revision_conflict",
            Self::InteractionCreated => "turnframe.interaction.created",
            Self::InteractionResolved => "turnframe.interaction.resolved",
            Self::InteractionStale => "turnframe.interaction.stale",
            Self::InteractionFailed => "turnframe.interaction.failed",
            Self::ClaimReceiptEmitted => "turnframe.claim.receipt_emitted",
            Self::ExternalOutcomeUnknown => "turnframe.external.outcome_unknown",
            Self::ExternalReconciled => "turnframe.external.reconciled",
            Self::ProviderFallback => "turnframe.provider.fallback",
            Self::ProviderCapabilityMismatch => "turnframe.provider.capability_mismatch",
            Self::WorkflowInvariantViolation => "turnframe.workflow.invariant_violation",
            Self::QuestionAnswered => "turnframe.question.answered",
            Self::QuestionUnanswered => "turnframe.question.unanswered",
            Self::ActSuperseded => "turnframe.act.superseded",
            Self::ActRefused => "turnframe.act.refused",
            Self::TurnDuration => "turnframe.turn.duration_ms",
            Self::ProjectionDuration => "turnframe.projection.duration_us",
            Self::ReductionDuration => "turnframe.reduction.duration_us",
            Self::PersistenceDuration => "turnframe.persistence.duration_ms",
            Self::ProviderLatency => "turnframe.provider.latency_ms",
            Self::ExternalLatency => "turnframe.external.latency_ms",
            Self::NarrationLatency => "turnframe.narration.latency_ms",
            Self::TaskCompleted => "turnframe.task.completed",
            Self::TaskRepaired => "turnframe.task.repaired",
            Self::TaskEscalated => "turnframe.task.escalated",
            Self::TaskVoteDisagreement => "turnframe.task.vote_disagreement",
            Self::BudgetExhausted => "turnframe.budget.exhausted",
            Self::TaskLatency => "turnframe.task.latency_ms",
        }
    }

    /// How the signal is measured: an occurrence counter or a duration
    /// distribution in the unit the name ends with (`_ms`, `_us`).
    #[must_use]
    pub const fn kind(&self) -> SignalKind {
        match self {
            Self::TurnDuration
            | Self::PersistenceDuration
            | Self::ProviderLatency
            | Self::ExternalLatency
            | Self::NarrationLatency
            | Self::TaskLatency => SignalKind::DurationMillis,
            Self::ProjectionDuration | Self::ReductionDuration => SignalKind::DurationMicros,
            Self::TurnReceived
            | Self::TurnCompleted
            | Self::TurnFailed
            | Self::TargetAmbiguous
            | Self::TargetUnresolved
            | Self::ActSuperseded
            | Self::ActRefused
            | Self::TargetMissing
            | Self::CaseNotAuthorized
            | Self::CommandConfirmationRequired
            | Self::CommandExecuted
            | Self::CommandRejected
            | Self::CommandIdempotencyReplay
            | Self::CommandRevisionConflict
            | Self::InteractionCreated
            | Self::InteractionResolved
            | Self::InteractionStale
            | Self::InteractionFailed
            | Self::ClaimReceiptEmitted
            | Self::ExternalOutcomeUnknown
            | Self::ExternalReconciled
            | Self::ProviderFallback
            | Self::ProviderCapabilityMismatch
            | Self::WorkflowInvariantViolation
            | Self::QuestionAnswered
            | Self::QuestionUnanswered
            | Self::TaskCompleted
            | Self::TaskRepaired
            | Self::TaskEscalated
            | Self::TaskVoteDisagreement
            | Self::BudgetExhausted => SignalKind::Counter,
        }
    }

    /// Returns `true` for signals that indicate a safety-integrity problem
    /// rather than a language-quality problem (spec §26.3).
    #[must_use]
    pub const fn is_safety_signal(&self) -> bool {
        matches!(
            self,
            Self::CommandRevisionConflict
                | Self::InteractionStale
                | Self::ExternalOutcomeUnknown
                | Self::WorkflowInvariantViolation
                | Self::ProviderCapabilityMismatch
        )
    }
}

/// Structured, text-free labels attached to a signal.
///
/// Every field is an identifier or an enumeration with a bounded value set.
/// Observers may drop labels a signal does not document; they must never add
/// free text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SignalLabels {
    /// Workflow the signal concerns.
    pub workflow: Option<WorkflowKey>,
    /// Provider key, for provider signals.
    pub provider: Option<ProviderKey>,
    /// Risk class, for command signals.
    pub risk: Option<RiskClass>,
    /// Model key, for provider signals. Configured model identifiers only.
    pub model: Option<ModelKey>,
    /// Normalized request purpose (e.g. `"extract"`), for provider
    /// signals (spec §20.2).
    pub purpose: Option<String>,
    /// Interaction kind, for interaction signals.
    pub interaction: Option<InteractionKind>,
    /// Stable machine-readable code that qualifies a failure or rejection
    /// (a [`RejectionCode`](crate::error::RejectionCode), a provider failure
    /// code, an invariant kind...). Never a message.
    pub error_code: Option<String>,
    /// Operation an act names, for signals about one act rather than one turn.
    ///
    /// Bounded like [`Self::workflow`] is, because the set of operations is the
    /// catalog and the catalog is finite. Never an argument or a value.
    pub operation: Option<OperationKey>,
    /// The effort of the turn the signal belongs to.
    pub effort: Option<crate::effort::Effort>,
}

impl SignalLabels {
    /// No labels.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Labels with a workflow.
    #[must_use]
    pub fn workflow(workflow: WorkflowKey) -> Self {
        Self {
            workflow: Some(workflow),
            ..Self::default()
        }
    }

    /// Adds a provider key.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Adds a risk class.
    #[must_use]
    pub fn with_risk(mut self, risk: RiskClass) -> Self {
        self.risk = Some(risk);
        self
    }

    /// Adds a model key.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<ModelKey>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Adds a normalized request purpose.
    #[must_use]
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = Some(purpose.into());
        self
    }

    /// Labels the signal with the turn's effort.
    #[must_use]
    pub fn with_effort(mut self, effort: crate::effort::Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Adds an interaction kind.
    #[must_use]
    pub fn with_interaction(mut self, kind: InteractionKind) -> Self {
        self.interaction = Some(kind);
        self
    }

    /// Adds a stable error or rejection code.
    #[must_use]
    pub fn with_operation(mut self, operation: impl Into<OperationKey>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    /// Returns a copy carrying a failure or rejection code.
    #[must_use]
    pub fn with_error_code(mut self, code: impl Into<String>) -> Self {
        self.error_code = Some(code.into());
        self
    }
}

/// Receives signals. Implementations must not block.
pub trait Observer: Send + Sync {
    /// Observes a signal without labels.
    fn observe(&self, signal: &Signal);

    /// Observes a signal with labels. The default drops the labels.
    fn observe_labeled(&self, signal: &Signal, labels: &SignalLabels) {
        let _ = labels;
        self.observe(signal);
    }

    /// Observes a duration signal ([`SignalKind::DurationMillis`] or
    /// [`SignalKind::DurationMicros`]) with its measured value. The default
    /// drops the value and forwards to [`Observer::observe_labeled`].
    fn observe_duration(&self, signal: &Signal, duration: Duration, labels: &SignalLabels) {
        let _ = duration;
        self.observe_labeled(signal, labels);
    }
}

/// An observer that ignores everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {
    fn observe(&self, _signal: &Signal) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};

    #[test]
    fn names_are_unique_and_prefixed() {
        let names: BTreeSet<&str> = Signal::ALL.iter().map(Signal::name).collect();
        assert_eq!(names.len(), Signal::ALL.len());
        assert!(names.iter().all(|n| n.starts_with("turnframe.")));
    }

    /// Fails to compile when a variant is added without touching this match,
    /// and fails at runtime when a variant is missing from `ALL`.
    #[test]
    fn all_lists_every_variant() {
        for signal in Signal::ALL {
            let listed = match signal {
                Signal::TurnReceived
                | Signal::TurnCompleted
                | Signal::TurnFailed
                | Signal::TargetAmbiguous
                | Signal::TargetUnresolved
                | Signal::TargetMissing
                | Signal::CaseNotAuthorized
                | Signal::CommandConfirmationRequired
                | Signal::CommandExecuted
                | Signal::CommandRejected
                | Signal::CommandIdempotencyReplay
                | Signal::CommandRevisionConflict
                | Signal::InteractionCreated
                | Signal::InteractionResolved
                | Signal::InteractionStale
                | Signal::InteractionFailed
                | Signal::ClaimReceiptEmitted
                | Signal::ExternalOutcomeUnknown
                | Signal::ExternalReconciled
                | Signal::ProviderFallback
                | Signal::ProviderCapabilityMismatch
                | Signal::WorkflowInvariantViolation
                | Signal::QuestionAnswered
                | Signal::QuestionUnanswered
                | Signal::ActSuperseded
                | Signal::ActRefused
                | Signal::TurnDuration
                | Signal::ProjectionDuration
                | Signal::ReductionDuration
                | Signal::PersistenceDuration
                | Signal::ProviderLatency
                | Signal::ExternalLatency
                | Signal::NarrationLatency
                | Signal::TaskCompleted
                | Signal::TaskRepaired
                | Signal::TaskEscalated
                | Signal::TaskVoteDisagreement
                | Signal::BudgetExhausted
                | Signal::TaskLatency => true,
            };
            assert!(listed);
        }
        let distinct: HashSet<Signal> = Signal::ALL.into_iter().collect();
        assert_eq!(distinct.len(), Signal::ALL.len());
    }

    #[test]
    fn kind_matches_name_suffix() {
        for signal in Signal::ALL {
            let name = signal.name();
            match signal.kind() {
                SignalKind::DurationMillis => assert!(name.ends_with("_ms"), "{name}"),
                SignalKind::DurationMicros => assert!(name.ends_with("_us"), "{name}"),
                SignalKind::Counter => {
                    assert!(!name.ends_with("_ms") && !name.ends_with("_us"), "{name}");
                }
            }
        }
    }

    #[test]
    fn labels_builders_set_every_field() {
        let labels = SignalLabels::workflow(WorkflowKey::from("trip"))
            .with_provider("openai")
            .with_model("gpt-x")
            .with_purpose("extract")
            .with_risk(RiskClass::Destructive)
            .with_interaction(InteractionKind::ConfirmCommand)
            .with_error_code("rate_limited");
        assert_eq!(
            labels.workflow.as_ref().map(WorkflowKey::as_str),
            Some("trip")
        );
        assert_eq!(
            labels.provider.as_ref().map(ProviderKey::as_str),
            Some("openai")
        );
        assert_eq!(labels.model.as_ref().map(ModelKey::as_str), Some("gpt-x"));
        assert_eq!(labels.purpose.as_deref(), Some("extract"));
        assert_eq!(labels.risk, Some(RiskClass::Destructive));
        assert_eq!(labels.interaction, Some(InteractionKind::ConfirmCommand));
        assert_eq!(labels.error_code.as_deref(), Some("rate_limited"));
    }

    #[test]
    fn noop_observer_accepts_everything() {
        let observer = NoopObserver;
        for signal in Signal::ALL {
            observer.observe(&signal);
            observer.observe_labeled(&signal, &SignalLabels::workflow(WorkflowKey::from("w")));
            observer.observe_duration(&signal, Duration::from_millis(3), &SignalLabels::none());
        }
    }
}
