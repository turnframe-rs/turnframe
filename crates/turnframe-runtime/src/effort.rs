//! The effort a turn runs at, resolved into the profiles, budgets and settings it runs
//! under. `medium` with nothing configured is the configuration as it is.

use serde::{Deserialize, Serialize};
pub use turnframe_core::effort::Effort;
use turnframe_provider::request::ReasoningEffort;
use turnframe_tasks::{
    Budget, Disagreement, ProfileChange, ProfileChanges, TaskKind, TaskProfiles,
};
use turnframe_understand::Settings;

use crate::config::OrchestratorConfig;

/// What a deployment changes about one level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct EffortOverrides {
    /// Task profile changes, over the level's own.
    pub tasks: ProfileChanges,
    /// The understanding budget, replacing the level's.
    pub budget: Option<Budget>,
    /// The reply budget, replacing the level's.
    pub reply_budget: Option<Budget>,
    /// The pipeline settings, replacing the level's.
    pub settings: Option<Settings>,
}

/// The default level, and what a deployment changes about each.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct EffortConfig {
    /// The level of a turn that does not force one.
    pub default: Effort,
    /// Changes to `low`.
    pub low: EffortOverrides,
    /// Changes to `medium`.
    pub medium: EffortOverrides,
    /// Changes to `high`.
    pub high: EffortOverrides,
}

impl EffortOverrides {
    /// No change to the level.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            tasks: ProfileChanges::new(),
            budget: None,
            reply_budget: None,
            settings: None,
        }
    }
}

impl EffortConfig {
    /// `medium` by default, and every level as shipped.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            default: Effort::Medium,
            low: EffortOverrides::none(),
            medium: EffortOverrides::none(),
            high: EffortOverrides::none(),
        }
    }

    /// What a deployment changes about `effort`.
    #[must_use]
    pub const fn overrides(&self, effort: Effort) -> &EffortOverrides {
        match effort {
            Effort::Low => &self.low,
            Effort::High => &self.high,
            _ => &self.medium,
        }
    }
}

/// One level, resolved: what a turn at that level runs under.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EffortProfile {
    /// The level.
    pub effort: Effort,
    /// Every task kind's profile.
    pub tasks: TaskProfiles,
    /// What understanding may spend.
    pub budget: Budget,
    /// What writing the reply may spend.
    pub reply_budget: Budget,
    /// How the understanding pipeline runs.
    pub settings: Settings,
    /// Whether step prose may be written, when narration asks for it.
    pub steps: bool,
}

/// The tasks that read the message, which `high` gives some reasoning. Not `extract`: on a mini
/// model, reasoning made it copy the words naming a field into the value.
const READING: [TaskKind; 8] = [
    TaskKind::Segment,
    TaskKind::Coverage,
    TaskKind::Route,
    TaskKind::Locate,
    TaskKind::Verify,
    TaskKind::QuestionFrame,
    TaskKind::CrossCheck,
    TaskKind::Respects,
];

/// The output cap of a task that reasons: billed per token used, so room costs nothing.
const REASONING_ROOM: u32 = 2_000;

/// The level's own changes, before a deployment's.
fn shipped(effort: Effort) -> ProfileChanges {
    let mut changes = ProfileChanges::default();
    match effort {
        Effort::Low => {
            let mut review = ProfileChange::default();
            review.review = Some(false);
            changes = changes.with(TaskKind::Acknowledge, review);
        }
        // How a message is split and routed decides every act after it: three readings,
        // and a split vote read once more, shown the answers that disagreed.
        Effort::Medium => {
            for kind in [TaskKind::Segment, TaskKind::Route] {
                let mut change = ProfileChange::default();
                change.votes = Some(3);
                change.on_disagreement = Some(Disagreement::Reread);
                changes = changes.with(kind, change);
            }
        }
        Effort::High => {
            for kind in READING {
                let mut change = ProfileChange::default();
                change.reasoning_effort = Some(ReasoningEffort::Low);
                // A provider counts reasoning against the output cap: room for both.
                change.max_output_tokens = Some(REASONING_ROOM);
                if matches!(kind, TaskKind::Segment | TaskKind::Route) {
                    change.votes = Some(3);
                    change.on_disagreement = Some(Disagreement::Reread);
                }
                if kind == TaskKind::Verify {
                    change.votes = Some(3);
                    change.on_disagreement = Some(Disagreement::Reread);
                }
                changes = changes.with(kind, change);
            }
            let mut review = ProfileChange::default();
            review.reasoning_effort = Some(ReasoningEffort::Low);
            review.max_output_tokens = Some(REASONING_ROOM);
            changes = changes.with(TaskKind::Review, review);
        }
        _ => {}
    }
    changes
}

/// Three times the calls and tokens, four more steps of depth, twice the wall clock.
fn scaled(budget: Budget) -> Budget {
    let mut scaled = budget;
    scaled.max_model_calls = budget.max_model_calls.map(|calls| calls.saturating_mul(3));
    scaled.max_prompt_tokens = budget
        .max_prompt_tokens
        .map(|tokens| tokens.saturating_mul(3));
    scaled.max_chain_depth = budget.max_chain_depth.map(|depth| depth.saturating_add(4));
    scaled.max_wall_clock_secs = budget
        .max_wall_clock_secs
        .map(|secs| secs.saturating_mul(2));
    scaled
}

/// What a turn at `effort` runs under.
#[must_use]
pub fn resolve(config: &OrchestratorConfig, effort: Effort) -> EffortProfile {
    let base = &config.understanding;
    let mut settings = base.settings;
    let mut budget = base.budget;
    let mut steps = true;
    match effort {
        Effort::Low => {
            settings = settings.with_transcript(2);
            steps = false;
        }
        // One verify vote per act: a verdict finding fault is voted on twice more.
        Effort::Medium => settings = settings.with_doubt_votes(2),
        Effort::High => {
            settings = settings
                .with_transcript(6)
                .with_reread_small_talk(true)
                .with_cross_check_rounds(2);
            budget = scaled(budget);
        }
        _ => {}
    }
    let overrides = config.effort.overrides(effort);
    let tasks = overrides
        .tasks
        .apply(shipped(effort).apply(base.tasks.clone()));
    EffortProfile {
        effort,
        tasks,
        budget: overrides.budget.unwrap_or(budget),
        reply_budget: overrides.reply_budget.unwrap_or(config.narration.budget),
        settings: overrides.settings.unwrap_or(settings),
        steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrchestratorConfig;
    use turnframe_provider::request::ReasoningEffort;
    use turnframe_tasks::{Disagreement, TaskKind};

    #[test]
    fn medium_votes_on_how_a_message_is_split_and_routed() {
        let config = OrchestratorConfig::conservative();
        let medium = resolve(&config, Effort::Medium);
        for kind in [TaskKind::Segment, TaskKind::Route] {
            let profile = medium.tasks.get(kind);
            assert_eq!(profile.votes, 3, "{kind:?}");
            assert_eq!(profile.on_disagreement, Disagreement::Reread, "{kind:?}");
        }
        assert_eq!(
            medium.tasks.get(TaskKind::Extract),
            config.understanding.tasks.get(TaskKind::Extract)
        );
        assert_eq!(
            resolve(&config, Effort::Low)
                .tasks
                .get(TaskKind::Segment)
                .votes,
            1
        );
    }

    #[test]
    fn medium_is_the_configuration_as_it_is_beside_its_votes() {
        let config = OrchestratorConfig::conservative();
        let medium = resolve(&config, Effort::Medium);
        assert_eq!(medium.budget, config.understanding.budget);
        assert_eq!(medium.reply_budget, config.narration.budget);
        assert_eq!(
            medium.settings,
            config.understanding.settings.with_doubt_votes(2)
        );
        assert!(medium.steps);
    }

    #[test]
    fn medium_votes_again_on_a_verdict_finding_fault() {
        let config = OrchestratorConfig::conservative();
        assert_eq!(resolve(&config, Effort::Medium).settings.doubt_votes, 2);
        assert_eq!(resolve(&config, Effort::Low).settings.doubt_votes, 0);
        assert_eq!(resolve(&config, Effort::High).settings.doubt_votes, 0);
    }

    #[test]
    fn high_buys_votes_reasoning_and_the_whole_turn_check() {
        let config = OrchestratorConfig::conservative();
        let high = resolve(&config, Effort::High);
        let segment = high.tasks.get(TaskKind::Segment);
        assert_eq!(segment.votes, 3);
        assert_eq!(segment.on_disagreement, Disagreement::Reread);
        assert_eq!(segment.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(high.tasks.get(TaskKind::Verify).votes, 3);
        assert_eq!(
            high.tasks.get(TaskKind::Verify).on_disagreement,
            Disagreement::Reread
        );
        assert_eq!(high.settings.cross_check_rounds, 2);
        assert!(high.settings.reread_small_talk);
        assert_eq!(high.settings.transcript, 6);
        assert_eq!(
            high.budget.max_model_calls,
            config
                .understanding
                .budget
                .max_model_calls
                .map(|calls| calls * 3)
        );
    }

    #[test]
    fn high_copies_values_as_medium_does() {
        let config = OrchestratorConfig::conservative();
        let high = resolve(&config, Effort::High);
        let medium = resolve(&config, Effort::Medium);
        assert_eq!(
            high.tasks.get(TaskKind::Extract).reasoning_effort,
            medium.tasks.get(TaskKind::Extract).reasoning_effort
        );
    }

    #[test]
    fn high_leaves_room_for_reasoning_in_every_answer() {
        let high = resolve(&OrchestratorConfig::conservative(), Effort::High);
        for kind in READING.into_iter().chain([TaskKind::Review]) {
            assert!(
                high.tasks.get(kind).max_output_tokens >= Some(2_000),
                "{kind:?}: reasoning is counted against the cap"
            );
        }
    }

    #[test]
    fn low_drops_the_review_and_the_step_prose_and_keeps_verification() {
        let config = OrchestratorConfig::conservative();
        let low = resolve(&config, Effort::Low);
        assert!(!low.tasks.get(TaskKind::Acknowledge).review);
        assert!(!low.steps);
        assert_eq!(low.settings.verify, config.understanding.settings.verify);
        assert_eq!(low.settings.transcript, 2);
    }

    #[test]
    fn a_deployment_changes_a_level_field_by_field() {
        let effort: EffortConfig = toml::from_str(
            r#"
            default = "high"
            [high.tasks.locate]
            model = "large"
            "#,
        )
        .unwrap();
        let mut config = OrchestratorConfig::conservative();
        config.effort = effort;
        let high = resolve(&config, config.effort.default);
        let locate = high.tasks.get(TaskKind::Locate);
        assert_eq!(locate.model.as_deref(), Some("large"));
        assert_eq!(
            locate.reasoning_effort,
            Some(ReasoningEffort::Low),
            "the level's own change stays"
        );
    }

    #[test]
    fn a_level_naming_a_task_that_does_not_exist_is_refused() {
        let refused =
            toml::from_str::<EffortConfig>("[high.tasks.extrakt]\nvotes = 3\n").unwrap_err();
        assert!(refused.to_string().contains("extrakt"), "{refused}");
    }
}
