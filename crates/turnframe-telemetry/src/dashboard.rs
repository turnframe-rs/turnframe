//! The reliability dashboard of spec §26.3, as data.
//!
//! Spec §26.3 is a warning as much as a layout: *"a single 'agent accuracy'
//! percentage hides the most important distinctions."* A response that gives
//! the user the wrong wording and a response that rebooks the wrong flight are
//! not the same failure, and averaging them produces a number nobody can act
//! on. The seven panels below keep them apart.
//!
//! This module describes the dashboard; it does not draw one. [`Dashboard`] is
//! plain serializable data, so the same description can generate a Grafana or
//! Datadog board, a docs page ([`Dashboard::to_markdown`]) or an alert
//! inventory, and stay in step with the metrics because it names them by the
//! same constants the observers emit.

use serde::{Deserialize, Serialize};
use turnframe_core::observe::Signal;

/// One panel of the reliability dashboard (spec §26.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PanelId {
    /// Did the system do something to the world it should not have done, or
    /// does it not know what it did?
    SideEffectIntegrityFailures,
    /// Did the assistant tell the user something no committed event backs?
    ClaimIntegrityFailures,
    /// Did the system understand the request?
    SemanticUnderstandingFailures,
    /// How often does the system have to ask before it can act?
    ClarificationRate,
    /// How often does the user walk away from what it asked?
    AbandonmentRate,
    /// How often does the model layer itself fail?
    ProviderFailures,
    /// How good does the result feel to the person who asked?
    UserExperienceScores,
}

impl PanelId {
    /// Every panel, in the order of spec §26.3 — severity first.
    pub const ALL: [Self; 7] = [
        Self::SideEffectIntegrityFailures,
        Self::ClaimIntegrityFailures,
        Self::SemanticUnderstandingFailures,
        Self::ClarificationRate,
        Self::AbandonmentRate,
        Self::ProviderFailures,
        Self::UserExperienceScores,
    ];

    /// Stable machine-readable key of the panel.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SideEffectIntegrityFailures => "side_effect_integrity_failures",
            Self::ClaimIntegrityFailures => "claim_integrity_failures",
            Self::SemanticUnderstandingFailures => "semantic_understanding_failures",
            Self::ClarificationRate => "clarification_rate",
            Self::AbandonmentRate => "abandonment_rate",
            Self::ProviderFailures => "provider_failures",
            Self::UserExperienceScores => "user_experience_scores",
        }
    }
}

impl std::fmt::Display for PanelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a panel's numbers come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PanelSource {
    /// Every series is a metric this library emits.
    LibraryMetrics,
    /// The library supplies context but the score itself comes from the
    /// application: a rating, a survey, an offline evaluation (spec §27.6).
    ApplicationSupplied,
}

/// One panel: what it answers, which metrics feed it, and against what it is
/// normalized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Panel {
    /// Which panel this is.
    pub id: PanelId,
    /// Human title for the board.
    pub title: String,
    /// The operational question the panel answers.
    pub question: String,
    /// Metric names plotted on the panel, in the order they should be read.
    pub series: Vec<String>,
    /// Metric the series are divided by to become a rate, when the panel is a
    /// rate rather than a count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<String>,
    /// Where the numbers come from.
    pub source: PanelSource,
    /// Whether a non-zero value on this panel is a defect that should page
    /// somebody rather than a statistic to watch.
    pub alerts: bool,
}

/// A data-only description of the reliability dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Dashboard {
    /// The panels, in reading order.
    pub panels: Vec<Panel>,
}

impl Dashboard {
    /// The dashboard of spec §26.3.
    ///
    /// ```rust
    /// use turnframe_telemetry::{Dashboard, PanelId};
    ///
    /// let dashboard = Dashboard::reliability();
    /// let claims = dashboard.panel(PanelId::ClaimIntegrityFailures).expect("panel");
    /// assert!(claims.series.contains(&String::from("turnframe.claim.receipt_emitted")));
    /// assert!(claims.alerts);
    /// ```
    #[must_use]
    pub fn reliability() -> Self {
        Self {
            panels: vec![
                Panel {
                    id: PanelId::SideEffectIntegrityFailures,
                    title: String::from("Side-effect integrity failures"),
                    question: String::from(
                        "Did a command act on a state the user never saw, or did an effect leave \
                         the system in a state nobody can describe?",
                    ),
                    series: names(&[
                        Signal::CommandRevisionConflict,
                        Signal::InteractionStale,
                        Signal::ExternalOutcomeUnknown,
                        Signal::ExternalReconciled,
                        Signal::WorkflowInvariantViolation,
                    ]),
                    denominator: Some(name(Signal::CommandExecuted)),
                    source: PanelSource::LibraryMetrics,
                    alerts: true,
                },
                Panel {
                    id: PanelId::ClaimIntegrityFailures,
                    title: String::from("Claim integrity failures"),
                    // The question used to be «did the assistant assert an
                    // outcome no event backs», answered by a counter of blocks
                    // a word matcher had refused. That matcher is gone — it
                    // could not see a negation, so it withheld the true
                    // sentence on exactly the turns that mattered — and the
                    // panel now answers the question that has an honest
                    // source: every receipt on screen came from a committed
                    // event, because that is the only way one is rendered.
                    question: String::from(
                        "Are the operational receipts the user sees derived from committed \
                         events?",
                    ),
                    series: names(&[Signal::ClaimReceiptEmitted]),
                    denominator: Some(name(Signal::ClaimReceiptEmitted)),
                    source: PanelSource::LibraryMetrics,
                    alerts: true,
                },
                Panel {
                    id: PanelId::SemanticUnderstandingFailures,
                    title: String::from("Semantic understanding failures"),
                    question: String::from(
                        "Did an understanding task need a repair or disagree with itself, or \
                         name a target that does not resolve?",
                    ),
                    series: names(&[
                        Signal::TaskRepaired,
                        Signal::TaskVoteDisagreement,
                        Signal::TargetAmbiguous,
                        Signal::TargetMissing,
                    ]),
                    denominator: Some(name(Signal::TurnReceived)),
                    source: PanelSource::LibraryMetrics,
                    alerts: false,
                },
                Panel {
                    id: PanelId::ClarificationRate,
                    title: String::from("Clarification rate"),
                    question: String::from(
                        "How often does a turn have to stop and ask instead of acting?",
                    ),
                    series: names(&[
                        Signal::InteractionCreated,
                        Signal::CommandConfirmationRequired,
                        Signal::QuestionUnanswered,
                    ]),
                    denominator: Some(name(Signal::TurnReceived)),
                    source: PanelSource::LibraryMetrics,
                    alerts: false,
                },
                Panel {
                    id: PanelId::AbandonmentRate,
                    title: String::from("Abandonment rate"),
                    question: String::from(
                        "How often is a card the system opened never answered, answered too late, \
                         or answered in a way that could not be accepted?",
                    ),
                    series: names(&[
                        Signal::InteractionResolved,
                        Signal::InteractionStale,
                        Signal::InteractionFailed,
                    ]),
                    denominator: Some(name(Signal::InteractionCreated)),
                    source: PanelSource::LibraryMetrics,
                    alerts: false,
                },
                Panel {
                    id: PanelId::ProviderFailures,
                    title: String::from("Provider failures"),
                    question: String::from(
                        "How often does the model layer fail, fall back, or lack a capability the \
                         turn required?",
                    ),
                    series: names(&[
                        Signal::ProviderFallback,
                        Signal::ProviderCapabilityMismatch,
                        Signal::ProviderLatency,
                        Signal::TurnFailed,
                    ]),
                    denominator: Some(name(Signal::TurnReceived)),
                    source: PanelSource::LibraryMetrics,
                    alerts: true,
                },
                Panel {
                    id: PanelId::UserExperienceScores,
                    title: String::from("User experience scores"),
                    question: String::from(
                        "Did the answer feel right and arrive quickly? The score itself comes \
                         from the application; the library supplies the latency and the answer \
                         coverage beside it.",
                    ),
                    series: names(&[
                        Signal::TurnDuration,
                        Signal::NarrationLatency,
                        Signal::QuestionAnswered,
                        Signal::QuestionUnanswered,
                    ]),
                    denominator: None,
                    source: PanelSource::ApplicationSupplied,
                    alerts: false,
                },
            ],
        }
    }

    /// The panel with this identifier, if the dashboard has one.
    #[must_use]
    pub fn panel(&self, id: PanelId) -> Option<&Panel> {
        self.panels.iter().find(|panel| panel.id == id)
    }

    /// Every metric name the dashboard plots, deduplicated, in reading order.
    #[must_use]
    pub fn series(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for panel in &self.panels {
            for series in panel.series.iter().chain(panel.denominator.iter()) {
                if !out.contains(series) {
                    out.push(series.clone());
                }
            }
        }
        out
    }

    /// Renders the dashboard as the markdown published in the documentation.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("# Reliability dashboard (spec §26.3)\n\n");
        out.push_str(
            "A single \"agent accuracy\" percentage hides the most important distinctions, so \
             these seven panels stay separate.\n",
        );
        for panel in &self.panels {
            out.push_str(&format!("\n## {}\n\n{}\n\n", panel.title, panel.question));
            out.push_str(&format!(
                "- Source: {}\n",
                match panel.source {
                    PanelSource::LibraryMetrics => "library metrics",
                    PanelSource::ApplicationSupplied => "application-supplied score",
                }
            ));
            out.push_str(&format!(
                "- Alerts: {}\n",
                if panel.alerts { "yes" } else { "no" }
            ));
            if let Some(denominator) = &panel.denominator {
                out.push_str(&format!("- Normalized by: `{denominator}`\n"));
            }
            out.push_str("- Series:\n");
            for series in &panel.series {
                out.push_str(&format!("  - `{series}`\n"));
            }
        }
        out
    }
}

impl Default for Dashboard {
    fn default() -> Self {
        Self::reliability()
    }
}

fn name(signal: Signal) -> String {
    signal.name().to_owned()
}

fn names(signals: &[Signal]) -> Vec<String> {
    signals.iter().copied().map(name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dashboard_has_every_panel_of_26_3_once() {
        let dashboard = Dashboard::reliability();
        assert_eq!(dashboard.panels.len(), PanelId::ALL.len());
        for id in PanelId::ALL {
            let panel = dashboard
                .panel(id)
                .unwrap_or_else(|| panic!("{id} missing"));
            assert_eq!(panel.id, id);
            assert!(!panel.title.is_empty());
            assert!(!panel.question.is_empty());
            assert!(!panel.series.is_empty(), "{id} plots nothing");
        }
    }

    #[test]
    fn every_series_is_a_metric_this_crate_emits() {
        let known: Vec<&str> = Signal::ALL.iter().map(Signal::name).collect();
        for series in Dashboard::reliability().series() {
            assert!(known.contains(&series.as_str()), "unknown metric {series}");
        }
    }

    #[test]
    fn every_safety_signal_appears_on_an_alerting_panel() {
        let dashboard = Dashboard::reliability();
        let alerting: Vec<String> = dashboard
            .panels
            .iter()
            .filter(|panel| panel.alerts)
            .flat_map(|panel| panel.series.clone())
            .collect();
        for signal in Signal::ALL.into_iter().filter(Signal::is_safety_signal) {
            assert!(
                alerting.contains(&signal.name().to_owned()),
                "{signal:?} is a safety signal but no alerting panel plots it"
            );
        }
    }

    #[test]
    fn integrity_and_semantics_are_never_merged() {
        let dashboard = Dashboard::reliability();
        let side_effects = dashboard
            .panel(PanelId::SideEffectIntegrityFailures)
            .expect("panel");
        let semantics = dashboard
            .panel(PanelId::SemanticUnderstandingFailures)
            .expect("panel");
        assert!(
            side_effects
                .series
                .iter()
                .all(|series| !semantics.series.contains(series)),
            "a series feeds both the integrity and the semantics panel"
        );
        assert!(side_effects.alerts);
        assert!(!semantics.alerts);
    }

    #[test]
    fn the_dashboard_round_trips_through_json() {
        let dashboard = Dashboard::reliability();
        let json = serde_json::to_string(&dashboard).expect("serializable");
        let back: Dashboard = serde_json::from_str(&json).expect("deserializable");
        assert_eq!(back, dashboard);
        assert!(json.contains("side_effect_integrity_failures"));
        assert!(json.contains("application_supplied"));
    }

    #[test]
    fn markdown_names_every_panel_and_series() {
        let dashboard = Dashboard::reliability();
        let markdown = dashboard.to_markdown();
        for panel in &dashboard.panels {
            assert!(markdown.contains(&panel.title), "{}", panel.title);
        }
        for series in dashboard.series() {
            assert!(markdown.contains(&series), "{series}");
        }
        assert!(markdown.contains("application-supplied score"));
    }

    #[test]
    fn the_default_dashboard_is_the_reliability_one() {
        assert_eq!(Dashboard::default(), Dashboard::reliability());
    }
}
