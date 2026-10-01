//! How each task kind is run: which model, which settings, how many votes and repairs.
//!
//! Every field has a default per kind (see [`TaskProfile::default_for`]), and a
//! deployment overrides only what it names, in code or in TOML:
//!
//! ```toml
//! [route]
//! votes = 3
//! on_disagreement = "escalate"
//! escalate_to = "large"
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use turnframe_provider::request::ReasoningEffort;

use crate::task::TaskKind;

/// What a vote without a strict majority leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Disagreement {
    /// Run once more on the escalation model; without one, ask.
    Escalate,
    /// Hand the answers back so the caller can ask the user.
    #[default]
    Clarify,
    /// Treat the task as failed.
    Fail,
    /// Run once more, shown the answers that disagreed; that answer stands.
    Reread,
}

/// How one task kind is run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct TaskProfile {
    /// Whether the task runs at all; optional tasks are switched off here.
    pub enabled: bool,
    /// The pool tag of the model that answers. `None` takes the pool's order.
    pub model: Option<String>,
    /// The pool tag of the model an escalation runs on. `None` never escalates.
    pub escalate_to: Option<String>,
    /// Sampling temperature. `None` keeps the provider's default.
    pub temperature: Option<f32>,
    /// Temperature for votes when more than one is cast.
    pub vote_temperature: f32,
    /// Output cap in tokens.
    pub max_output_tokens: Option<u32>,
    /// How much a reasoning model may think.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Deadline of one call in seconds; the turn's budget may shorten it.
    pub timeout_secs: Option<u64>,
    /// Answers sampled and compared; `1` means no vote.
    pub votes: u8,
    /// What a vote without a strict majority leads to.
    pub on_disagreement: Disagreement,
    /// Rounds that send a structurally wrong answer back with the error.
    pub repairs: u8,
    /// Times a call is sent again as it was, after a provider failure another try
    /// can change: a refusal, a filter, a broken answer, a timeout.
    pub retries: u8,
    /// Whether a written block is reviewed before it is published.
    pub review: bool,
}

impl Default for TaskProfile {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            escalate_to: None,
            temperature: Some(0.0),
            vote_temperature: 0.7,
            max_output_tokens: None,
            reasoning_effort: Some(ReasoningEffort::Minimal),
            timeout_secs: None,
            votes: 1,
            on_disagreement: Disagreement::Clarify,
            repairs: 1,
            retries: 1,
            review: false,
        }
    }
}

impl TaskProfile {
    /// The shipped profile of `kind`.
    #[must_use]
    pub fn default_for(kind: TaskKind) -> Self {
        let base = Self::default();
        match kind {
            TaskKind::Segment => Self {
                max_output_tokens: Some(800),
                ..base
            },
            TaskKind::Coverage => Self {
                max_output_tokens: Some(200),
                repairs: 0,
                ..base
            },
            TaskKind::TakeUp | TaskKind::Route | TaskKind::Locate | TaskKind::QuestionFrame => {
                Self {
                    max_output_tokens: Some(150),
                    ..base
                }
            }
            TaskKind::Extract => Self {
                max_output_tokens: Some(600),
                ..base
            },
            // The one judge of a reading: at minimal reasoning it misjudged multi-part
            // messages; reasoning counts against the output cap, so it gets room.
            TaskKind::Verify => Self {
                max_output_tokens: Some(2_000),
                reasoning_effort: Some(ReasoningEffort::Low),
                repairs: 0,
                ..base
            },
            TaskKind::CrossCheck => Self {
                max_output_tokens: Some(400),
                ..base
            },
            TaskKind::Respects => Self {
                max_output_tokens: Some(200),
                ..base
            },
            TaskKind::Investigate => Self {
                enabled: false,
                max_output_tokens: Some(400),
                ..base
            },
            // A writer keeps the model's own temperature; reasoning would spend its cap.
            TaskKind::Acknowledge => Self {
                temperature: None,
                max_output_tokens: Some(300),
                review: true,
                ..base
            },
            TaskKind::Answer => Self {
                temperature: None,
                ..base
            },
            TaskKind::Review => Self {
                max_output_tokens: Some(300),
                ..base
            },
            // A progress line is a preview: one call, and nothing waits for it.
            TaskKind::Progress => Self {
                temperature: None,
                max_output_tokens: Some(80),
                repairs: 0,
                retries: 0,
                ..base
            },
            _ => base,
        }
    }

    /// A copy casting `votes` answers.
    #[must_use]
    pub fn with_votes(mut self, votes: u8) -> Self {
        self.votes = votes.max(1);
        self
    }

    /// A copy answered by the model tagged `tag`.
    #[must_use]
    pub fn on_model(mut self, tag: impl Into<String>) -> Self {
        self.model = Some(tag.into());
        self
    }

    /// A copy escalating to the model tagged `tag`.
    #[must_use]
    pub fn escalating_to(mut self, tag: impl Into<String>) -> Self {
        self.escalate_to = Some(tag.into());
        self
    }

    /// A copy deciding a vote without a majority as `disagreement` says.
    #[must_use]
    pub fn on_disagreement(mut self, disagreement: Disagreement) -> Self {
        self.on_disagreement = disagreement;
        self
    }

    /// A copy with `repairs` repair rounds.
    #[must_use]
    pub fn with_repairs(mut self, repairs: u8) -> Self {
        self.repairs = repairs;
        self
    }

    /// A copy with the task switched on or off.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// A copy thinking as much as `reasoning` allows.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning;
        self
    }

    /// A copy whose written block is reviewed, or not.
    #[must_use]
    pub fn with_review(mut self, review: bool) -> Self {
        self.review = review;
        self
    }
}

/// The profiles of every task kind: the shipped ones, and a deployment's overrides.
///
/// Keyed by the kind's name (`segment`, `route`, …), which is also how TOML names them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskProfiles {
    overrides: BTreeMap<String, TaskProfile>,
}

impl TaskProfiles {
    /// The shipped profiles, with no override.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            overrides: BTreeMap::new(),
        }
    }

    /// The profile `kind` runs under.
    #[must_use]
    pub fn get(&self, kind: TaskKind) -> TaskProfile {
        self.overrides
            .get(kind.as_str())
            .cloned()
            .unwrap_or_else(|| TaskProfile::default_for(kind))
    }

    /// Replaces the profile of `kind`.
    #[must_use]
    pub fn with(mut self, kind: TaskKind, profile: TaskProfile) -> Self {
        self.overrides.insert(kind.as_str().to_owned(), profile);
        self
    }

    /// Adjusts the profile of `kind`, starting from what it is now.
    #[must_use]
    pub fn adjust(self, kind: TaskKind, change: impl FnOnce(TaskProfile) -> TaskProfile) -> Self {
        let current = self.get(kind);
        self.with(kind, change(current))
    }

    /// Every overridden kind name, so a configuration can be checked for typos.
    pub fn overridden(&self) -> impl Iterator<Item = &str> {
        self.overrides.keys().map(String::as_str)
    }

    /// Every pool tag the profiles name, so the pool can be checked for them.
    #[must_use]
    pub fn tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = TaskKind::ALL
            .iter()
            .map(|kind| self.get(*kind))
            .flat_map(|profile| [profile.model, profile.escalate_to])
            .flatten()
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }
}

/// The fields of a [`TaskProfile`] to change; a field left out keeps its value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ProfileChange {
    /// [`TaskProfile::enabled`].
    pub enabled: Option<bool>,
    /// [`TaskProfile::model`].
    pub model: Option<String>,
    /// [`TaskProfile::escalate_to`].
    pub escalate_to: Option<String>,
    /// [`TaskProfile::max_output_tokens`].
    pub max_output_tokens: Option<u32>,
    /// [`TaskProfile::reasoning_effort`].
    pub reasoning_effort: Option<ReasoningEffort>,
    /// [`TaskProfile::timeout_secs`].
    pub timeout_secs: Option<u64>,
    /// [`TaskProfile::votes`].
    pub votes: Option<u8>,
    /// [`TaskProfile::on_disagreement`].
    pub on_disagreement: Option<Disagreement>,
    /// [`TaskProfile::repairs`].
    pub repairs: Option<u8>,
    /// [`TaskProfile::retries`].
    pub retries: Option<u8>,
    /// [`TaskProfile::review`].
    pub review: Option<bool>,
}

impl ProfileChange {
    /// `profile` with every field this change names.
    #[must_use]
    pub fn apply(&self, mut profile: TaskProfile) -> TaskProfile {
        let change = self.clone();
        if let Some(value) = change.enabled {
            profile.enabled = value;
        }
        if let Some(value) = change.model {
            profile.model = Some(value);
        }
        if let Some(value) = change.escalate_to {
            profile.escalate_to = Some(value);
        }
        if let Some(value) = change.max_output_tokens {
            profile.max_output_tokens = Some(value);
        }
        if let Some(value) = change.reasoning_effort {
            profile.reasoning_effort = Some(value);
        }
        if let Some(value) = change.timeout_secs {
            profile.timeout_secs = Some(value);
        }
        if let Some(value) = change.votes {
            profile.votes = value.max(1);
        }
        if let Some(value) = change.on_disagreement {
            profile.on_disagreement = value;
        }
        if let Some(value) = change.repairs {
            profile.repairs = value;
        }
        if let Some(value) = change.retries {
            profile.retries = value;
        }
        if let Some(value) = change.review {
            profile.review = value;
        }
        profile
    }
}

/// Changes to several task kinds, keyed by the kind's name as TOML writes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProfileChanges {
    changes: BTreeMap<String, ProfileChange>,
}

impl ProfileChanges {
    /// No change.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            changes: BTreeMap::new(),
        }
    }

    /// Changes `kind` as `change` says.
    #[must_use]
    pub fn with(mut self, kind: TaskKind, change: ProfileChange) -> Self {
        self.changes.insert(kind.as_str().to_owned(), change);
        self
    }

    /// `profiles` with every change applied.
    #[must_use]
    pub fn apply(&self, mut profiles: TaskProfiles) -> TaskProfiles {
        for kind in TaskKind::ALL {
            if let Some(change) = self.changes.get(kind.as_str()) {
                profiles = profiles.adjust(kind, |profile| change.apply(profile));
            }
        }
        profiles
    }

    /// Every pool tag the changes name, so the pool can be checked for them.
    #[must_use]
    pub fn tags(&self) -> Vec<String> {
        self.changes
            .values()
            .flat_map(|change| [change.model.clone(), change.escalate_to.clone()])
            .flatten()
            .collect()
    }
}

impl<'de> Deserialize<'de> for ProfileChanges {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let changes = BTreeMap::<String, ProfileChange>::deserialize(deserializer)?;
        if let Some(unknown) = changes.keys().find(|name| {
            !TaskKind::ALL
                .iter()
                .any(|kind| kind.as_str() == name.as_str())
        }) {
            return Err(serde::de::Error::custom(format!(
                "`{unknown}` is not a task kind"
            )));
        }
        Ok(Self { changes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verifier_thinks_before_it_judges() {
        let verify = TaskProfile::default_for(TaskKind::Verify);
        assert_eq!(verify.reasoning_effort, Some(ReasoningEffort::Low));
        assert!(
            verify.max_output_tokens >= Some(2_000),
            "reasoning counts against the cap"
        );
        assert_eq!(
            TaskProfile::default_for(TaskKind::Extract).reasoning_effort,
            Some(ReasoningEffort::Minimal)
        );
    }

    #[test]
    fn a_reply_is_written_at_the_least_effort() {
        for kind in [TaskKind::Acknowledge, TaskKind::Answer, TaskKind::Progress] {
            let writer = TaskProfile::default_for(kind);
            assert_eq!(
                writer.reasoning_effort,
                Some(ReasoningEffort::Minimal),
                "{kind:?}"
            );
            assert_eq!(
                writer.temperature, None,
                "{kind:?} keeps the model's own voice"
            );
        }
    }

    #[test]
    fn an_override_names_only_what_changes() {
        let profiles: TaskProfiles = toml::from_str(
            r#"
            [route]
            votes = 3
            on_disagreement = "escalate"
            escalate_to = "large"
            "#,
        )
        .expect("parses");
        let route = profiles.get(TaskKind::Route);
        assert_eq!(route.votes, 3);
        assert_eq!(route.on_disagreement, Disagreement::Escalate);
        assert_eq!(route.escalate_to.as_deref(), Some("large"));
        assert_eq!(profiles.get(TaskKind::Extract).max_output_tokens, Some(600));
        assert_eq!(profiles.tags(), vec!["large".to_owned()]);
    }

    #[test]
    fn a_change_keeps_what_it_does_not_name() {
        let changes: ProfileChanges = toml::from_str(
            r#"
            [route]
            votes = 3
            "#,
        )
        .expect("parses");
        let profiles = changes.apply(TaskProfiles::new());
        let route = profiles.get(TaskKind::Route);
        assert_eq!(route.votes, 3);
        assert_eq!(
            route.max_output_tokens,
            Some(150),
            "route's own cap survives"
        );
    }

    #[test]
    fn a_change_to_a_task_that_does_not_exist_is_refused_by_name() {
        let refused = toml::from_str::<ProfileChanges>("[extrakt]\nvotes = 3\n").unwrap_err();
        assert!(refused.to_string().contains("extrakt"), "{refused}");
    }

    #[test]
    fn optional_tasks_ship_as_the_spec_says() {
        let profiles = TaskProfiles::new();
        assert!(profiles.get(TaskKind::Coverage).enabled);
        assert!(!profiles.get(TaskKind::Investigate).enabled);
        assert!(profiles.get(TaskKind::Acknowledge).review);
        assert!(!profiles.get(TaskKind::Answer).review);
        assert_eq!(profiles.get(TaskKind::Verify).repairs, 0);
    }
}
