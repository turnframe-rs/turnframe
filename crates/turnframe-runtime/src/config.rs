//! Orchestration modes and runtime configuration (spec §11.1, §11.4, Appendix A).
//!
//! Two things live here. The first is [`OrchestrationMode`]: how much rope the
//! model gets before the deterministic pipeline takes over. The second is
//! [`OrchestratorConfig`]: the knobs an application turns, every one of which
//! defaults to the careful setting.
//!
//! # Safety settings are not knobs
//!
//! A few fields exist only so that a configuration file which tries to disable
//! them fails loudly instead of quietly working. [`OrchestratorConfig::validate`]
//! refuses `reject_unknown_fields = false`, `require_evidence_for_mutations =
//! false` and `fail_closed_on_policy_store_error = false`, because the library
//! does not implement the permissive behaviour at all: the plan types deny
//! unknown fields unconditionally, evidence is validated unconditionally, and
//! there is no fail-open path for an unavailable policy source (I19). A config
//! that asks for the permissive behaviour is a config whose author believes
//! something untrue, and that is worth an error.
//!
//! Sandboxed autonomy goes further: the variant cannot be written down without
//! a [`SandboxAcknowledgement`], whose only constructor is named after what it
//! gives up and logs a warning when called.

use std::fmt;
use std::time::Duration;

use serde::de::{Error as _, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use turnframe_core::command::RiskClass;
use turnframe_core::flow::BriefingBudget;
use turnframe_core::plan::limits::PlanLimits;
use turnframe_core::policy::PolicySnapshot;
use turnframe_core::reduce::SourcePolicy;
use turnframe_core::response::ToneProfile;
use turnframe_core::turn::TurnLimits;
use turnframe_tasks::{Budget, TaskProfiles};
use turnframe_understand::Settings;

/// A configuration value the library refuses to work with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A bound that must allow at least one of something was zero.
    #[error("{field} must be at least 1")]
    MustBePositive {
        /// Dotted path of the field, e.g. `execution.max_commands_per_turn`.
        field: &'static str,
    },
    /// A safety setting was turned off, and the library has no such mode.
    #[error("{field} may not be disabled: {because}")]
    UnsafeSetting {
        /// Dotted path of the field.
        field: &'static str,
        /// Why the setting cannot be disabled.
        because: &'static str,
    },
    /// Two settings ask for incompatible things.
    #[error("{first} contradicts {second}")]
    Contradiction {
        /// Dotted path of the first field.
        first: &'static str,
        /// Dotted path of the second field.
        second: &'static str,
    },
    /// A ratio expressed in per mille left the `0..=1000` range.
    #[error("{field} must be between 0 and 1000 per mille")]
    OutOfRange {
        /// Dotted path of the field.
        field: &'static str,
    },
}

/// The exact phrase that acknowledges sandboxed autonomy, on the wire and in
/// the name of [`SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes`].
pub const SANDBOX_ACKNOWLEDGEMENT: &str = "i-accept-unreviewed-autonomous-writes";

/// Proof that the caller knows what
/// [`OrchestrationMode::SandboxedAutonomous`] gives up.
///
/// The type has no public field and no `Default`, so the variant cannot be
/// written as a struct literal from another crate; the only way in is
/// [`Self::i_accept_unreviewed_autonomous_writes`], which logs a warning. On the
/// wire it is the literal string [`SANDBOX_ACKNOWLEDGEMENT`], so a TOML or JSON
/// configuration has to spell the sentence out too.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SandboxAcknowledgement(());

impl SandboxAcknowledgement {
    /// Acknowledges that sandboxed autonomy lets the model drive writes that no
    /// human reviewed, and emits a `WARN` on the `turnframe.config` target.
    ///
    /// Use it only for reversible, non-regulated domains. Even then
    /// [`OrchestrationMode::allows_risk`] keeps refusing the destructive,
    /// irreversible and externally regulated classes (§11.4).
    #[must_use]
    pub fn i_accept_unreviewed_autonomous_writes() -> Self {
        tracing::warn!(
            target: "turnframe.config",
            mode = "sandboxed_autonomous",
            acknowledgement = SANDBOX_ACKNOWLEDGEMENT,
            "sandboxed autonomous orchestration enabled: the model may drive writes without human review"
        );
        Self(())
    }
}

impl fmt::Debug for SandboxAcknowledgement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SANDBOX_ACKNOWLEDGEMENT)
    }
}

impl Serialize for SandboxAcknowledgement {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(SANDBOX_ACKNOWLEDGEMENT)
    }
}

impl<'de> Deserialize<'de> for SandboxAcknowledgement {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == SANDBOX_ACKNOWLEDGEMENT {
            Ok(Self::i_accept_unreviewed_autonomous_writes())
        } else {
            Err(D::Error::invalid_value(
                Unexpected::Str(&raw),
                &SANDBOX_ACKNOWLEDGEMENT,
            ))
        }
    }
}

/// What a sandboxed autonomous run may spend before it is cut off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ResourceBudget {
    /// Model calls of every purpose, across the whole turn.
    pub max_model_calls: u16,
    /// Read-tool calls across the whole turn.
    pub max_read_calls: u16,
    /// Prompt tokens the turn may send in total.
    pub max_prompt_tokens: u64,
    /// Wall clock the turn may take.
    pub max_wall_clock: Duration,
}

impl ResourceBudget {
    /// A small budget: 8 model calls, 16 read calls, 200k prompt tokens, 60s.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            max_model_calls: 8,
            max_read_calls: 16,
            max_prompt_tokens: 200_000,
            max_wall_clock: Duration::from_secs(60),
        }
    }

    /// Returns a copy with another model-call budget.
    #[must_use]
    pub const fn with_max_model_calls(mut self, max_model_calls: u16) -> Self {
        self.max_model_calls = max_model_calls;
        self
    }

    /// Returns a copy with another read-call budget.
    #[must_use]
    pub const fn with_max_read_calls(mut self, max_read_calls: u16) -> Self {
        self.max_read_calls = max_read_calls;
        self
    }

    /// Returns a copy with another token budget.
    #[must_use]
    pub const fn with_max_prompt_tokens(mut self, max_prompt_tokens: u64) -> Self {
        self.max_prompt_tokens = max_prompt_tokens;
        self
    }

    /// Returns a copy with another wall-clock budget.
    #[must_use]
    pub const fn with_max_wall_clock(mut self, max_wall_clock: Duration) -> Self {
        self.max_wall_clock = max_wall_clock;
        self
    }

    fn validate(&self) -> Result<(), ConfigError> {
        positive(
            "mode.budget.max_model_calls",
            u64::from(self.max_model_calls),
        )?;
        positive("mode.budget.max_read_calls", u64::from(self.max_read_calls))?;
        positive("mode.budget.max_prompt_tokens", self.max_prompt_tokens)?;
        if self.max_wall_clock.is_zero() {
            return Err(ConfigError::MustBePositive {
                field: "mode.budget.max_wall_clock",
            });
        }
        Ok(())
    }
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self::conservative()
    }
}

/// How much autonomy the model gets: which risk classes are eligible at all (§11.4).
///
/// | Risk class | `Deterministic` | `SandboxedAutonomous` |
/// | --- | --- | --- |
/// | `ReadOnly`, `ReversibleLowRisk`, `SensitiveDataChange` | yes | yes |
/// | `Destructive`, `Irreversible`, `ExternalRegulated` | yes | **no** |
///
/// A domain that classifies a money movement as `ReversibleLowRisk` has mislabelled it,
/// and no mode can rescue that.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum OrchestrationMode {
    /// Understanding proposes, the reducer and policy decide. The default.
    #[default]
    Deterministic,
    /// Experimental mode for sandboxed, reversible domains only (§11.4). Build it with
    /// [`Self::sandboxed_autonomous`]; the acknowledgement cannot be built elsewhere.
    SandboxedAutonomous {
        /// What the turn may spend.
        budget: ResourceBudget,
        /// Proof the operator knows what this gives up.
        acknowledgement: SandboxAcknowledgement,
    },
}

impl OrchestrationMode {
    /// Builds the sandboxed autonomous mode.
    ///
    /// The `acknowledgement` argument is the whole point: obtaining one means
    /// calling [`SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes`],
    /// which logs a warning.
    #[must_use]
    pub const fn sandboxed_autonomous(
        budget: ResourceBudget,
        acknowledgement: SandboxAcknowledgement,
    ) -> Self {
        Self::SandboxedAutonomous {
            budget,
            acknowledgement,
        }
    }

    /// Whether commands of `risk` are eligible in this mode (§11.4).
    ///
    /// See the table on [`OrchestrationMode`].
    #[must_use]
    pub fn allows_risk(&self, risk: RiskClass) -> bool {
        match self {
            Self::SandboxedAutonomous { .. } => !matches!(
                risk,
                RiskClass::Destructive | RiskClass::Irreversible | RiskClass::ExternalRegulated
            ),
            Self::Deterministic => true,
        }
    }

    /// The risk classes this mode refuses, in ladder order.
    ///
    /// Feed them to [`PolicySnapshot::forbidden_risk_classes`] — or let
    /// [`OrchestratorConfig::policy_snapshot`] do it.
    #[must_use]
    pub fn forbidden_risk_classes(&self) -> Vec<RiskClass> {
        [
            RiskClass::ReadOnly,
            RiskClass::ReversibleLowRisk,
            RiskClass::SensitiveDataChange,
            RiskClass::Destructive,
            RiskClass::Irreversible,
            RiskClass::ExternalRegulated,
        ]
        .into_iter()
        .filter(|risk| !self.allows_risk(*risk))
        .collect()
    }

    /// The resource budget, for the sandboxed mode only.
    #[must_use]
    pub fn budget(&self) -> Option<&ResourceBudget> {
        match self {
            Self::SandboxedAutonomous { budget, .. } => Some(budget),
            _ => None,
        }
    }

    /// Returns `true` for [`Self::SandboxedAutonomous`].
    #[must_use]
    pub fn is_sandboxed(&self) -> bool {
        matches!(self, Self::SandboxedAutonomous { .. })
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if let Some(budget) = self.budget() {
            budget.validate()?;
        }
        Ok(())
    }
}

/// How much of a turn's files reaches a model.
///
/// # Why nothing ships
///
/// A limit an adopter cannot raise is not configurable, so there is no shipped
/// ceiling here either: an adopter who sets nothing sends every file the turn
/// carried, which is what the user attached and what they expect to be looked
/// at. Attachments are bounded by what one person put in one message, unlike a
/// conversation or a briefing, so there is no runaway to protect anybody from.
///
/// What a budget buys, when one is set, is that the runtime says which files it
/// left out — deterministically to the user and as a fact to the writing stage.
/// A silently truncated request produces an answer about the wrong document,
/// which is worse than an answer about none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AttachmentConfig {
    /// How many files may go into one request, or `None` for all of them.
    pub max_files: Option<usize>,
    /// How many bytes of file may go into one request, or `None` for all.
    ///
    /// Counted before encoding, which is what an application can reason about;
    /// the wire form is larger and the provider's own limit is the one that
    /// finally applies.
    pub max_total_bytes: Option<usize>,
}

impl AttachmentConfig {
    /// No limits, which is what ships.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            max_files: None,
            max_total_bytes: None,
        }
    }

    /// Returns a copy taking at most `max_files` files per request.
    #[must_use]
    pub const fn with_max_files(mut self, max_files: Option<usize>) -> Self {
        self.max_files = max_files;
        self
    }

    /// Returns a copy taking at most `max_total_bytes` of file per request.
    #[must_use]
    pub const fn with_max_total_bytes(mut self, max_total_bytes: Option<usize>) -> Self {
        self.max_total_bytes = max_total_bytes;
        self
    }
}

impl Default for AttachmentConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

/// How a turn is understood: the limits on what it may ask, the conversation shown, the
/// budget of its model tasks, which acts are verified, and each task kind's settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct UnderstandingConfig {
    /// Size limits on what one turn may be understood to ask.
    pub plan_limits: PlanLimits,
    /// Size limits on the turn input.
    pub turn_limits: TurnLimits,
    /// How many past turns are loaded, or `None` for all of them.
    pub transcript_turns: Option<usize>,
    /// How much of a workflow's per-phase guidance reaches a task, per record.
    pub briefing_budget: BriefingBudget,
    /// What understanding one turn may spend.
    pub budget: Budget,
    /// Which acts are verified, and how much conversation extraction sees.
    pub settings: Settings,
    /// Each task kind's model, sampling, votes, repairs and escalation.
    pub tasks: TaskProfiles,
}

impl UnderstandingConfig {
    /// No limits, the whole conversation, the shipped budget, settings and profiles.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            plan_limits: PlanLimits::conservative(),
            turn_limits: TurnLimits::conservative(),
            transcript_turns: None,
            briefing_budget: BriefingBudget::conservative(),
            budget: Budget::understanding(),
            settings: Settings::conservative(),
            tasks: TaskProfiles::new(),
        }
    }

    /// Returns a copy with other plan limits.
    #[must_use]
    pub fn with_plan_limits(mut self, plan_limits: PlanLimits) -> Self {
        self.plan_limits = plan_limits;
        self
    }

    /// Returns a copy with other turn limits.
    #[must_use]
    pub fn with_turn_limits(mut self, turn_limits: TurnLimits) -> Self {
        self.turn_limits = turn_limits;
        self
    }

    /// Returns a copy loading at most `transcript_turns` past turns.
    #[must_use]
    pub fn with_transcript_turns(mut self, transcript_turns: Option<usize>) -> Self {
        self.transcript_turns = transcript_turns;
        self
    }

    /// Returns a copy with another briefing budget.
    #[must_use]
    pub fn with_briefing_budget(mut self, budget: BriefingBudget) -> Self {
        self.briefing_budget = budget;
        self
    }

    /// Returns a copy with another task budget.
    #[must_use]
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// Returns a copy with other pipeline settings.
    #[must_use]
    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    /// Returns a copy with other task profiles.
    #[must_use]
    pub fn with_tasks(mut self, tasks: TaskProfiles) -> Self {
        self.tasks = tasks;
        self
    }

    /// The act limit, when one is set.
    #[must_use]
    pub const fn max_acts(&self) -> Option<usize> {
        self.plan_limits.max_acts
    }

    /// The question limit, when one is set.
    #[must_use]
    pub const fn max_questions(&self) -> Option<usize> {
        self.plan_limits.max_questions
    }

    fn validate(&self) -> Result<(), ConfigError> {
        // A limit that is not set is not a limit of zero.
        for (field, limit) in [
            ("understanding.plan_limits.max_acts", self.max_acts()),
            (
                "understanding.plan_limits.max_questions",
                self.max_questions(),
            ),
            (
                "understanding.plan_limits.max_constraints",
                self.plan_limits.max_constraints,
            ),
            (
                "understanding.turn_limits.max_text_bytes",
                self.turn_limits.max_text_bytes,
            ),
        ] {
            if let Some(limit) = limit {
                positive(field, limit as u64)?;
            }
        }
        positive(
            "understanding.budget.max_parallel",
            self.budget.max_parallel as u64,
        )?;
        positive(
            "understanding.budget.per_call_timeout_secs",
            self.budget.per_call_timeout_secs,
        )
    }
}

impl Default for UnderstandingConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

/// How the assistant is allowed to talk (spec §18, §19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct NarrationConfig {
    /// Whether a model writes the answer and transition blocks at all. With
    /// narration off the turn is receipts, notices and cards only.
    pub enabled: bool,
    /// What the narration tasks of one turn may spend.
    #[serde(default = "Budget::narration")]
    pub budget: Budget,
    /// Voice the narrator should use.
    pub tone: ToneProfile,
    /// Cap on one written block, in characters; `None`, the default, is no cap. A
    /// longer block is refused whole and reported, never cut mid-sentence.
    pub max_answer_chars: Option<usize>,
    /// Source requirement for answers resting on general domain knowledge.
    /// Answers resting on case state are always `AuthoritativeOnly`.
    pub default_source_policy: SourcePolicy,
    /// Whether narration may be regenerated after commands committed. This is
    /// the only retry I17 permits once effects may exist.
    pub allow_retry_after_commit: bool,
    /// Whether each understanding step is also said in the turn's language, as a
    /// `TurnEvent::StepSaid`: one small model call per step. Off by default.
    #[serde(default)]
    pub steps: bool,
}

impl NarrationConfig {
    /// Narration on, neutral tone, no cap on answer length, any source,
    /// post-commit narration retries allowed.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            enabled: true,
            budget: Budget::narration(),
            tone: ToneProfile::Neutral,
            max_answer_chars: None,
            default_source_policy: SourcePolicy::AnySource,
            allow_retry_after_commit: true,
            steps: false,
        }
    }

    /// Returns a copy with narration switched on or off.
    ///
    /// With narration off the turn is receipts, notices and cards only, and
    /// every question still gets a block — an explicitly unsupported one
    /// (spec §19.4). Silence is never how a question disappears.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Returns a copy with another budget for the narration tasks.
    #[must_use]
    pub const fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// Returns a copy that says, or stops saying, each understanding step.
    #[must_use]
    pub const fn with_steps(mut self, steps: bool) -> Self {
        self.steps = steps;
        self
    }

    /// Returns a copy with another tone.
    #[must_use]
    pub const fn with_tone(mut self, tone: ToneProfile) -> Self {
        self.tone = tone;
        self
    }

    /// Returns a copy with another source policy for general-knowledge answers.
    #[must_use]
    pub const fn with_default_source_policy(mut self, policy: SourcePolicy) -> Self {
        self.default_source_policy = policy;
        self
    }

    /// Returns a copy with a cap on one generated answer, or with none.
    #[must_use]
    pub const fn with_max_answer_chars(mut self, max_answer_chars: Option<usize>) -> Self {
        self.max_answer_chars = max_answer_chars;
        self
    }

    fn validate(&self) -> Result<(), ConfigError> {
        match self.max_answer_chars {
            Some(max) => positive("narration.max_answer_chars", max as u64),
            None => Ok(()),
        }
    }
}

impl Default for NarrationConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

/// How server-created cards behave (spec §15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct InteractionConfig {
    /// Time to live given to cards the runtime creates. `None` means no expiry.
    pub default_ttl: Option<Duration>,
    /// How many candidates a server-defined selection card may offer.
    ///
    /// Beyond this the act is rejected and the user is asked to narrow it down,
    /// rather than shown a truncated list — truncating would let list order
    /// decide which cases the user may pick, which is exactly what I8 forbids.
    pub max_selection_candidates: usize,
    /// Whether a selection card owns unqualified answers for the case it is
    /// filed under (I5).
    ///
    /// The default is `false`, and that is a considered choice. A card asking
    /// "which of these did you mean?" has to be stored against *some* case row,
    /// but the whole point is that nobody knows yet which case the user meant.
    /// Letting it block that row would wedge a case the user may never have been
    /// talking about, and would compete with the blocking card that case might
    /// legitimately need. Turn it on only where a selection really does own the
    /// conversation until it is answered.
    pub selection_cards_block_the_case: bool,
    /// Whether confirmation cards are invalidated by a revision change.
    pub confirmation_cards_bind_to_revision: bool,
    /// Whether a card whose command failed goes back to `Active` so the user
    /// may answer again, instead of being recorded `Failed` (spec §15.5).
    ///
    /// The default is `false`, and that is the careful reading. A command that
    /// the domain refused, or that was planned against a revision the case has
    /// left, will usually be refused again for the same reason; re-offering the
    /// same button invites the user to keep pressing it. Turn it on where the
    /// failure is genuinely transient and the card is still the right question.
    pub restore_card_after_failed_command: bool,
}

impl InteractionConfig {
    /// One-day expiry, at most 8 candidates, non-blocking selection cards,
    /// confirmations bound to the revision they were rendered against.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            default_ttl: Some(Duration::from_secs(24 * 60 * 60)),
            max_selection_candidates: 8,
            selection_cards_block_the_case: false,
            confirmation_cards_bind_to_revision: true,
            restore_card_after_failed_command: false,
        }
    }

    /// Returns a copy that restores a card whose command failed.
    #[must_use]
    pub const fn restoring_failed_cards(mut self) -> Self {
        self.restore_card_after_failed_command = true;
        self
    }

    /// Returns a copy with another time to live.
    #[must_use]
    pub const fn with_default_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.default_ttl = ttl;
        self
    }

    /// Returns a copy with another candidate cap.
    #[must_use]
    pub const fn with_max_selection_candidates(mut self, max: usize) -> Self {
        self.max_selection_candidates = max;
        self
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.max_selection_candidates < 2 {
            return Err(ConfigError::MustBePositive {
                field: "interaction.max_selection_candidates",
            });
        }
        if self.default_ttl.is_some_and(|ttl| ttl.is_zero()) {
            return Err(ConfigError::MustBePositive {
                field: "interaction.default_ttl",
            });
        }
        Ok(())
    }
}

impl Default for InteractionConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

/// How commands are executed (Appendix A, spec §13.4, §16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ExecutionConfig {
    /// Time budget for one command batch.
    pub default_timeout: Duration,
    /// Cap on the commands one turn may compile. Exceeding it refuses the turn
    /// rather than executing a prefix.
    pub max_commands_per_turn: usize,
    /// Must stay `true`: an unavailable policy source stops the turn (I19).
    pub fail_closed_on_policy_store_error: bool,
    /// Whether one case may commit while another fails in the same turn.
    pub allow_cross_case_partial_success: bool,
    /// Whether mutations on one case default to committing together
    /// ([`AtomicityScope::PerCase`](turnframe_core::command::AtomicityScope::PerCase)).
    pub group_mutations_per_case: bool,
}

impl ExecutionConfig {
    /// 30 seconds, 32 commands, fail closed, no cross-case partial success,
    /// per-case grouping.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            default_timeout: Duration::from_secs(30),
            max_commands_per_turn: 32,
            fail_closed_on_policy_store_error: true,
            allow_cross_case_partial_success: false,
            group_mutations_per_case: true,
        }
    }

    /// Returns a copy with another command budget.
    #[must_use]
    pub const fn with_max_commands_per_turn(mut self, max: usize) -> Self {
        self.max_commands_per_turn = max;
        self
    }

    /// Returns a copy with another batch timeout.
    #[must_use]
    pub const fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.default_timeout.is_zero() {
            return Err(ConfigError::MustBePositive {
                field: "execution.default_timeout",
            });
        }
        positive(
            "execution.max_commands_per_turn",
            self.max_commands_per_turn as u64,
        )?;
        if !self.fail_closed_on_policy_store_error {
            return Err(ConfigError::UnsafeSetting {
                field: "execution.fail_closed_on_policy_store_error",
                because: "there is no fail-open path for an unavailable policy source (I19)",
            });
        }
        Ok(())
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

/// What the runtime records about itself (spec §26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ObservabilityConfig {
    /// Whether [`Signal`](turnframe_core::observe::Signal)s are emitted.
    pub emit_metrics: bool,
    /// Whether a [`ReplayRecord`](turnframe_core::replay::ReplayRecord) is
    /// persisted for every turn (I20).
    pub record_replay: bool,
    /// Trace sampling, in per mille, so the configuration stays exactly
    /// comparable and hashable.
    pub trace_sample_per_mille: u16,
}

impl ObservabilityConfig {
    /// Metrics on, replay on, every turn traced.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            emit_metrics: true,
            record_replay: true,
            trace_sample_per_mille: 1000,
        }
    }

    /// Returns a copy with another trace sample rate.
    #[must_use]
    pub const fn with_trace_sample_per_mille(mut self, per_mille: u16) -> Self {
        self.trace_sample_per_mille = per_mille;
        self
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.trace_sample_per_mille > 1000 {
            return Err(ConfigError::OutOfRange {
                field: "observability.trace_sample_per_mille",
            });
        }
        Ok(())
    }
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

/// What may leave the runtime and for how long it is kept (spec §25.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PrivacyConfig {
    /// Whether the user's raw text may be attached to traces and logs.
    pub allow_user_text_in_telemetry: bool,
    /// Whether prompts sent to providers are stored with the replay record.
    pub store_model_prompts: bool,
    /// How long a replay record is kept.
    pub replay_retention_days: u32,
    /// Whether attachment file names are redacted from telemetry.
    pub redact_attachment_filenames: bool,
}

impl PrivacyConfig {
    /// No user text in telemetry, no stored prompts, 90 days of replay,
    /// file names redacted.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            allow_user_text_in_telemetry: false,
            store_model_prompts: false,
            replay_retention_days: 90,
            redact_attachment_filenames: true,
        }
    }

    /// Returns a copy with another retention window.
    #[must_use]
    pub const fn with_replay_retention_days(mut self, days: u32) -> Self {
        self.replay_retention_days = days;
        self
    }

    fn validate(&self) -> Result<(), ConfigError> {
        positive(
            "privacy.replay_retention_days",
            u64::from(self.replay_retention_days),
        )
    }
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

/// Everything the runtime needs to know before it handles a turn (Appendix A).
///
/// Provider routing is deliberately absent: it belongs to the provider layer and
/// this crate depends on `turnframe-core` alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct OrchestratorConfig {
    /// How much autonomy the model gets.
    pub mode: OrchestrationMode,
    /// How the turn is understood.
    pub understanding: UnderstandingConfig,
    /// How much of the turn's files reaches a model.
    pub attachments: AttachmentConfig,
    /// How the assistant talks.
    pub narration: NarrationConfig,
    /// How cards behave.
    pub interaction: InteractionConfig,
    /// How commands execute.
    pub execution: ExecutionConfig,
    /// What is measured and recorded.
    pub observability: ObservabilityConfig,
    /// What may leave the runtime.
    pub privacy: PrivacyConfig,
    /// The default effort, and what each level changes.
    #[serde(default)]
    pub effort: crate::effort::EffortConfig,
}

impl OrchestratorConfig {
    /// Every section at its conservative default, in
    /// [`OrchestrationMode::Deterministic`].
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            mode: OrchestrationMode::Deterministic,
            understanding: UnderstandingConfig::conservative(),
            attachments: AttachmentConfig::conservative(),
            narration: NarrationConfig::conservative(),
            interaction: InteractionConfig::conservative(),
            execution: ExecutionConfig::conservative(),
            observability: ObservabilityConfig::conservative(),
            privacy: PrivacyConfig::conservative(),
            effort: crate::effort::EffortConfig::conservative(),
        }
    }

    /// Returns a copy with another default effort and other level changes.
    #[must_use]
    pub fn with_effort(mut self, effort: crate::effort::EffortConfig) -> Self {
        self.effort = effort;
        self
    }

    /// Returns a copy with another attachment budget.
    #[must_use]
    pub const fn with_attachments(mut self, attachments: AttachmentConfig) -> Self {
        self.attachments = attachments;
        self
    }

    /// Returns a copy in another orchestration mode.
    #[must_use]
    pub fn with_mode(mut self, mode: OrchestrationMode) -> Self {
        self.mode = mode;
        self
    }

    /// Returns a copy with another understanding section.
    #[must_use]
    pub fn with_understanding(mut self, understanding: UnderstandingConfig) -> Self {
        self.understanding = understanding;
        self
    }

    /// Returns a copy with another narration section.
    #[must_use]
    pub const fn with_narration(mut self, narration: NarrationConfig) -> Self {
        self.narration = narration;
        self
    }

    /// Returns a copy with another interaction section.
    #[must_use]
    pub const fn with_interaction(mut self, interaction: InteractionConfig) -> Self {
        self.interaction = interaction;
        self
    }

    /// Returns a copy with another execution section.
    #[must_use]
    pub const fn with_execution(mut self, execution: ExecutionConfig) -> Self {
        self.execution = execution;
        self
    }

    /// Returns a copy with another observability section.
    #[must_use]
    pub const fn with_observability(mut self, observability: ObservabilityConfig) -> Self {
        self.observability = observability;
        self
    }

    /// Returns a copy with another privacy section.
    #[must_use]
    pub const fn with_privacy(mut self, privacy: PrivacyConfig) -> Self {
        self.privacy = privacy;
        self
    }

    /// Tightens `base` with the risk classes the mode refuses (§11.4).
    ///
    /// The result only ever forbids more than `base` did: a mode cannot widen a
    /// policy snapshot, and [`origin_satisfies`](turnframe_core::command::origin_satisfies)
    /// keeps its own floor regardless of either.
    #[must_use]
    pub fn policy_snapshot(&self, base: PolicySnapshot) -> PolicySnapshot {
        let mut snapshot = base;
        for risk in self.mode.forbidden_risk_classes() {
            if !snapshot.forbidden_risk_classes.contains(&risk) {
                snapshot.forbidden_risk_classes.push(risk);
            }
        }
        snapshot.forbidden_risk_classes.sort_unstable();
        snapshot
    }

    /// Checks every section and the rules that span sections.
    ///
    /// Cross-section rules:
    ///
    /// * storing model prompts while the replay record is switched off asks for
    ///   prompts nothing will ever write;
    /// * sandboxed autonomy without a replay record leaves the one mode where
    ///   the model drives writes with no way to explain what it did (I20).
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.mode.validate()?;
        self.understanding.validate()?;
        self.narration.validate()?;
        self.interaction.validate()?;
        self.execution.validate()?;
        self.observability.validate()?;
        self.privacy.validate()?;
        self.validate_effort()?;
        if self.privacy.store_model_prompts && !self.observability.record_replay {
            return Err(ConfigError::Contradiction {
                first: "privacy.store_model_prompts",
                second: "observability.record_replay",
            });
        }
        if self.mode.is_sandboxed() && !self.observability.record_replay {
            return Err(ConfigError::Contradiction {
                first: "mode.sandboxed_autonomous",
                second: "observability.record_replay",
            });
        }
        Ok(())
    }
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

impl OrchestratorConfig {
    /// A level's budget replacing the configured one is held to the same bounds.
    fn validate_effort(&self) -> Result<(), ConfigError> {
        use turnframe_core::effort::Effort;
        const FIELDS: [(Effort, [&str; 4]); 3] = [
            (
                Effort::Low,
                [
                    "effort.low.budget.max_parallel",
                    "effort.low.budget.per_call_timeout_secs",
                    "effort.low.reply_budget.max_parallel",
                    "effort.low.reply_budget.per_call_timeout_secs",
                ],
            ),
            (
                Effort::Medium,
                [
                    "effort.medium.budget.max_parallel",
                    "effort.medium.budget.per_call_timeout_secs",
                    "effort.medium.reply_budget.max_parallel",
                    "effort.medium.reply_budget.per_call_timeout_secs",
                ],
            ),
            (
                Effort::High,
                [
                    "effort.high.budget.max_parallel",
                    "effort.high.budget.per_call_timeout_secs",
                    "effort.high.reply_budget.max_parallel",
                    "effort.high.reply_budget.per_call_timeout_secs",
                ],
            ),
        ];
        for (effort, fields) in FIELDS {
            let overrides = self.effort.overrides(effort);
            for (budget, [parallel, timeout]) in [
                (overrides.budget, [fields[0], fields[1]]),
                (overrides.reply_budget, [fields[2], fields[3]]),
            ] {
                if let Some(budget) = budget {
                    positive(parallel, budget.max_parallel as u64)?;
                    positive(timeout, budget.per_call_timeout_secs)?;
                }
            }
        }
        Ok(())
    }
}

fn positive(field: &'static str, value: u64) -> Result<(), ConfigError> {
    if value == 0 {
        Err(ConfigError::MustBePositive { field })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> OrchestrationMode {
        OrchestrationMode::sandboxed_autonomous(
            ResourceBudget::conservative(),
            SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes(),
        )
    }

    #[test]
    fn mode_risk_table() {
        let sandboxed = sandbox();
        let table = [
            (RiskClass::ReadOnly, true),
            (RiskClass::ReversibleLowRisk, true),
            (RiskClass::SensitiveDataChange, true),
            (RiskClass::Destructive, false),
            (RiskClass::Irreversible, false),
            (RiskClass::ExternalRegulated, false),
        ];
        for (risk, sandboxed_allows) in table {
            assert!(OrchestrationMode::Deterministic.allows_risk(risk));
            assert_eq!(sandboxed.allows_risk(risk), sandboxed_allows, "{risk:?}");
        }
        assert!(
            OrchestrationMode::Deterministic
                .forbidden_risk_classes()
                .is_empty()
        );
    }

    #[test]
    fn mode_shape_helpers() {
        assert!(sandbox().is_sandboxed());
        assert_eq!(sandbox().budget(), Some(&ResourceBudget::conservative()));
        assert_eq!(OrchestrationMode::Deterministic.budget(), None);
        assert_eq!(
            OrchestrationMode::default(),
            OrchestrationMode::Deterministic
        );
    }

    #[test]
    fn a_sandbox_tightens_the_policy_snapshot_and_never_widens_it() {
        let config = OrchestratorConfig::conservative().with_mode(sandbox());
        let snapshot = config.policy_snapshot(PolicySnapshot::conservative());
        assert!(snapshot.is_risk_forbidden(RiskClass::Destructive));
        assert!(snapshot.is_risk_forbidden(RiskClass::ExternalRegulated));
        assert!(!snapshot.is_risk_forbidden(RiskClass::ReversibleLowRisk));
        // A snapshot that already forbade more keeps forbidding it.
        let strict = PolicySnapshot::sandbox();
        let merged = OrchestratorConfig::conservative().policy_snapshot(strict.clone());
        assert_eq!(
            merged.forbidden_risk_classes.len(),
            strict.forbidden_risk_classes.len()
        );
        assert!(merged.is_risk_forbidden(RiskClass::SensitiveDataChange));
    }

    #[test]
    fn the_acknowledgement_is_the_only_way_into_the_sandbox() {
        let json = serde_json::to_value(sandbox()).unwrap();
        assert_eq!(json["kind"], "sandboxed_autonomous");
        assert_eq!(json["acknowledgement"], SANDBOX_ACKNOWLEDGEMENT);
        let back: OrchestrationMode = serde_json::from_value(json).unwrap();
        assert_eq!(back, sandbox());
        let mut wrong = serde_json::to_value(sandbox()).unwrap();
        wrong["acknowledgement"] = serde_json::json!("sure why not");
        assert!(serde_json::from_value::<OrchestrationMode>(wrong).is_err());
        let mut missing = serde_json::to_value(sandbox()).unwrap();
        missing
            .as_object_mut()
            .unwrap()
            .remove("acknowledgement")
            .unwrap();
        assert!(serde_json::from_value::<OrchestrationMode>(missing).is_err());
        assert_eq!(
            format!(
                "{:?}",
                SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes()
            ),
            SANDBOX_ACKNOWLEDGEMENT
        );
    }

    #[test]
    fn conservative_config_validates_and_round_trips() {
        let config = OrchestratorConfig::conservative();
        assert_eq!(config.validate(), Ok(()));
        assert_eq!(config, OrchestratorConfig::default());
        let json = serde_json::to_string(&config).unwrap();
        let back: OrchestratorConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, config);
        assert!(serde_json::from_str::<OrchestratorConfig>(r#"{"extra": 1}"#).is_err());
        // Nothing is bounded unless a deployment bounds it.
        assert_eq!(config.understanding.max_acts(), None);
        assert_eq!(config.understanding.max_questions(), None);
    }

    #[test]
    fn config_validation_table() {
        let base = OrchestratorConfig::conservative();
        let cases: Vec<(&str, OrchestratorConfig, Option<ConfigError>)> = vec![
            ("conservative", base.clone(), None),
            (
                "no acts allowed",
                base.clone().with_understanding(
                    UnderstandingConfig::conservative()
                        .with_plan_limits(PlanLimits::conservative().with_max_acts(Some(0))),
                ),
                Some(ConfigError::MustBePositive {
                    field: "understanding.plan_limits.max_acts",
                }),
            ),
            (
                "silent answers",
                base.clone().with_narration(NarrationConfig {
                    max_answer_chars: Some(0),
                    ..NarrationConfig::conservative()
                }),
                Some(ConfigError::MustBePositive {
                    field: "narration.max_answer_chars",
                }),
            ),
            (
                "selection with one candidate",
                base.clone().with_interaction(
                    InteractionConfig::conservative().with_max_selection_candidates(1),
                ),
                Some(ConfigError::MustBePositive {
                    field: "interaction.max_selection_candidates",
                }),
            ),
            (
                "cards expiring instantly",
                base.clone().with_interaction(
                    InteractionConfig::conservative().with_default_ttl(Some(Duration::ZERO)),
                ),
                Some(ConfigError::MustBePositive {
                    field: "interaction.default_ttl",
                }),
            ),
            (
                "no command budget",
                base.clone()
                    .with_execution(ExecutionConfig::conservative().with_max_commands_per_turn(0)),
                Some(ConfigError::MustBePositive {
                    field: "execution.max_commands_per_turn",
                }),
            ),
            (
                "instant timeout",
                base.clone().with_execution(
                    ExecutionConfig::conservative().with_default_timeout(Duration::ZERO),
                ),
                Some(ConfigError::MustBePositive {
                    field: "execution.default_timeout",
                }),
            ),
            (
                "fail open on policy",
                base.clone().with_execution(ExecutionConfig {
                    fail_closed_on_policy_store_error: false,
                    ..ExecutionConfig::conservative()
                }),
                Some(ConfigError::UnsafeSetting {
                    field: "execution.fail_closed_on_policy_store_error",
                    because: "there is no fail-open path for an unavailable policy source (I19)",
                }),
            ),
            (
                "impossible sampling",
                base.clone().with_observability(
                    ObservabilityConfig::conservative().with_trace_sample_per_mille(1001),
                ),
                Some(ConfigError::OutOfRange {
                    field: "observability.trace_sample_per_mille",
                }),
            ),
            (
                "no retention",
                base.clone()
                    .with_privacy(PrivacyConfig::conservative().with_replay_retention_days(0)),
                Some(ConfigError::MustBePositive {
                    field: "privacy.replay_retention_days",
                }),
            ),
            (
                "prompts stored but no replay",
                base.clone()
                    .with_privacy(PrivacyConfig {
                        store_model_prompts: true,
                        ..PrivacyConfig::conservative()
                    })
                    .with_observability(ObservabilityConfig {
                        record_replay: false,
                        ..ObservabilityConfig::conservative()
                    }),
                Some(ConfigError::Contradiction {
                    first: "privacy.store_model_prompts",
                    second: "observability.record_replay",
                }),
            ),
            (
                "sandbox without replay",
                base.clone()
                    .with_mode(sandbox())
                    .with_observability(ObservabilityConfig {
                        record_replay: false,
                        ..ObservabilityConfig::conservative()
                    }),
                Some(ConfigError::Contradiction {
                    first: "mode.sandboxed_autonomous",
                    second: "observability.record_replay",
                }),
            ),
            (
                "empty sandbox budget",
                base.with_mode(OrchestrationMode::sandboxed_autonomous(
                    ResourceBudget::conservative().with_max_model_calls(0),
                    SandboxAcknowledgement::i_accept_unreviewed_autonomous_writes(),
                )),
                Some(ConfigError::MustBePositive {
                    field: "mode.budget.max_model_calls",
                }),
            ),
        ];
        for (name, config, expected) in cases {
            assert_eq!(config.validate().err(), expected, "case {name}");
        }
    }

    #[test]
    fn budget_builders_and_defaults() {
        let budget = ResourceBudget::default()
            .with_max_read_calls(3)
            .with_max_prompt_tokens(10)
            .with_max_wall_clock(Duration::from_secs(5));
        assert_eq!(budget.max_read_calls, 3);
        assert_eq!(budget.max_prompt_tokens, 10);
        assert_eq!(budget.max_wall_clock, Duration::from_secs(5));
        assert_eq!(budget.max_model_calls, 8);
        assert_eq!(
            ResourceBudget::conservative()
                .with_max_wall_clock(Duration::ZERO)
                .validate(),
            Err(ConfigError::MustBePositive {
                field: "mode.budget.max_wall_clock"
            })
        );
    }

    #[test]
    fn section_builders_keep_the_rest() {
        let narration = NarrationConfig::conservative()
            .with_tone(ToneProfile::Neutral)
            .with_default_source_policy(SourcePolicy::AuthoritativeOnly);
        assert_eq!(
            narration.default_source_policy,
            SourcePolicy::AuthoritativeOnly
        );
        assert!(narration.enabled);
        let understanding =
            UnderstandingConfig::conservative().with_turn_limits(TurnLimits::conservative());
        assert_eq!(understanding.budget, Budget::understanding());
        assert_eq!(
            UnderstandingConfig::default(),
            UnderstandingConfig::conservative()
        );
        assert_eq!(NarrationConfig::default(), NarrationConfig::conservative());
        assert_eq!(
            InteractionConfig::default(),
            InteractionConfig::conservative()
        );
        assert_eq!(ExecutionConfig::default(), ExecutionConfig::conservative());
        assert_eq!(
            ObservabilityConfig::default(),
            ObservabilityConfig::conservative()
        );
        assert_eq!(PrivacyConfig::default(), PrivacyConfig::conservative());
    }
}
