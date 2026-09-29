//! What a run produced: per item, per suite, and in a form a build can gate on
//! (spec §26.3, §27.6).
//!
//! # One number would be a lie
//!
//! §26.3 is blunt about it: "a single 'agent accuracy' percentage hides the
//! most important distinctions". A corpus in which the assistant sent a
//! rebooking it should not have, but phrased three replies beautifully, can
//! average to a very healthy figure. So the report keeps the dashboard's
//! categories apart — side-effect integrity, operational claim integrity,
//! semantic interpretation, clarification, abandonment, provider failure and
//! the judge's user-experience scores — and refuses to combine them into one.
//!
//! # Three numbers, and each one sees what the others cannot
//!
//! The pass rate is about **effects**: what the turn did and did not do. It is
//! the number a release blocks on, and on its own it flatters the design,
//! because every reading the runtime refused to carry out reads as a clean
//! turn. [`ItemReport::refused_proposals`] is the correction: the model
//! proposed an act and nothing was journaled, which is a fact about the model's
//! reading rather than about the effect.
//!
//! Neither of them can see a turn whose answer never became a plan at all.
//! [`ItemReport::samples_with_discards`] is that one: the runtime read what the
//! model produced and threw it away whole — an invented citation, a question
//! quoting nothing, a document of the wrong shape — and asked again. It is
//! usually invisible, since the repair round recovers and the effects come out
//! right, and when it is not invisible the turn simply has no effects, which
//! looks exactly like a turn that correctly had nothing to do.
//!
//! # Samples and votes stay apart too
//!
//! [`ItemReport::deterministic_pass_rate`] counts **samples**, never votes. A
//! judge score never enters it. [`CriterionSummary`] carries the judge's
//! numbers with their vote spread, so a reader can see whether a low score is
//! the model's fault or the judge's disagreement with itself.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::assertions::{AssertionFailure, ExpectationName};
use crate::config::EvalConfig;
use crate::corpus::{ItemFingerprint, ItemId, Tag};
use crate::judge::{CriterionOutcome, JudgeCriterion, ratio};

/// The reliability categories of the specification's dashboard (§26.3).
///
/// A failure belongs to exactly one, derived from the expectation it broke, so
/// a forbidden command that fired is never averaged into anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReliabilityCategory {
    /// An effect happened that should not have, or did not happen that should
    /// have. The category a release blocks on.
    SideEffectIntegrity,
    /// What the turn said about itself did not match what it did.
    OperationalClaimIntegrity,
    /// The model's reading of the message was wrong, before any effect.
    SemanticInterpretation,
    /// The turn asked, or failed to ask, for a clarification or a confirmation.
    Clarification,
    /// The turn produced nothing usable at all.
    Abandonment,
    /// The provider layer failed.
    ProviderFailure,
    /// What a judge thought of the wording.
    UserExperience,
}

impl ReliabilityCategory {
    /// The category a broken expectation belongs to.
    #[must_use]
    pub const fn of(expectation: ExpectationName) -> Self {
        match expectation {
            ExpectationName::Commands
            | ExpectationName::ForbiddenCommand
            | ExpectationName::Events
            | ExpectationName::ForbiddenEvent
            | ExpectationName::TruncatedLedger
            | ExpectationName::CaseRevision
            // A value that ended up wrong, or moved when it should not have, is
            // a side effect like any other: something happened to a record. It
            // is not a claim about what the assistant SAID, which is the other
            // category and a different kind of harm.
            | ExpectationName::CaseState
            | ExpectationName::WorkflowState
            | ExpectationName::CaseCount => Self::SideEffectIntegrity,
            ExpectationName::Outcome
            | ExpectationName::ResponseBlocks
            | ExpectationName::TurnPhase => Self::OperationalClaimIntegrity,
            ExpectationName::Acts | ExpectationName::TargetResolution => {
                Self::SemanticInterpretation
            }
            ExpectationName::InteractionStatus => Self::Clarification,
        }
    }

    /// The snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SideEffectIntegrity => "side_effect_integrity",
            Self::OperationalClaimIntegrity => "operational_claim_integrity",
            Self::SemanticInterpretation => "semantic_interpretation",
            Self::Clarification => "clarification",
            Self::Abandonment => "abandonment",
            Self::ProviderFailure => "provider_failure",
            Self::UserExperience => "user_experience",
        }
    }
}

impl std::fmt::Display for ReliabilityCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One execution of one item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleReport {
    /// 1-based position in the item's samples.
    pub sample: u32,
    /// Deterministic expectations that did not hold.
    #[serde(default)]
    pub failures: Vec<AssertionFailure>,
    /// The harness could not even set the sample up. Distinct from a failing
    /// assertion: nothing was measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_error: Option<String>,
    /// Canonical rendering of what this run did, for counting distinct
    /// behaviours across samples.
    pub signature: String,
    /// Judge opinions about this sample, when the item asked for any.
    #[serde(default)]
    pub judge: Vec<CriterionOutcome>,
    /// How many provider attempts failed or fell back.
    #[serde(default)]
    pub provider_failures: usize,
    /// How many cards the turn created.
    #[serde(default)]
    pub cards_created: usize,
    /// How many acts the message was understood to ask for.
    ///
    /// The pair below is the whole point of recording it: a turn where the model
    /// proposed something and nothing was journaled is a turn the runtime
    /// REFUSED, and that is a fact about the model's reading rather than about
    /// the effect. A report that shows only the effect says an assistant is
    /// perfect exactly where its reading is worst — every refusal reads as a
    /// clean turn — which flatters a design whose whole claim is that it makes
    /// bad readings harmless.
    #[serde(default)]
    pub acts_proposed: usize,
    /// How many of them the reduction refused outright.
    ///
    /// Only `rejected`: an act the domain turned down. Not the one that
    /// changes nothing, not the one awaiting a confirmation, not the one a
    /// later act superseded — see
    /// [`refused_proposals`](ItemReport::refused_proposals).
    #[serde(default)]
    pub acts_refused: usize,
    /// How many commands the turn journaled.
    ///
    /// Not comparable one-to-one with [`Self::acts_proposed`]: one act can
    /// produce several commands and a command type is not an operation name. It
    /// is here to be read against zero — nothing journaled after something was
    /// proposed — and not as a ratio of the two.
    #[serde(default)]
    pub commands_journaled: usize,
    /// The turn produced no usable answer at all.
    #[serde(default)]
    pub abandoned: bool,
    /// The stable code of every answer the runtime threw away whole this turn,
    /// in order and with repeats.
    ///
    /// The third number of a reliability report, and the one neither of the
    /// other two can reach. The pass rate is about effects and the refused
    /// proposals are about a plan the runtime declined to carry out; this is
    /// about a plan that never became one, because the runtime read the model's
    /// answer and refused it whole — a citation the user's message does not
    /// contain, a question that does not quote what it answers, a document that
    /// does not match its schema.
    ///
    /// Codes rather than the full records, because a report is read in
    /// aggregate and the runtime's own wording names positions inside one
    /// turn's plan. The full records, reasons included, are on
    /// [`Observation::discarded_answers`](crate::observation::Observation::discarded_answers).
    ///
    /// [`Observation`]: crate::observation::Observation
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discarded_answers: Vec<String>,
    /// What the turn actually said, when the run was configured to keep it.
    ///
    /// Empty unless
    /// [`ExecutionConfig::record_answers`](crate::config::ExecutionConfig::record_answers)
    /// is on — see there for why that is the default. It is here for a person
    /// curating a corpus, never for an assertion: nothing in this crate reads
    /// it, and an expectation that did would be measuring prose.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answer: String,
    /// How each understanding task did, where the item says what it expects of it.
    #[serde(default)]
    pub tasks: crate::understanding::TaskScores,
}

impl SampleReport {
    /// Returns `true` when every deterministic expectation held.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty() && self.harness_error.is_none()
    }
}

/// How much the agent under test varied across the samples of one item.
///
/// This is the number `samples_per_item` exists to produce. It is about the
/// **model**; no judge vote contributes to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variance {
    /// How many samples ran.
    pub samples: usize,
    /// How many distinct behaviours were observed. One means the model was
    /// perfectly repeatable — whether or not it was right.
    pub distinct_behaviours: usize,
    /// Fraction of samples that satisfied every deterministic expectation.
    pub pass_rate: f64,
    /// Variance of the pass indicator, `p * (1 - p)`. Zero when every sample
    /// agreed; at its maximum when half of them did.
    pub pass_variance: f64,
    /// How often each distinct behaviour occurred, most frequent first.
    pub behaviours: Vec<BehaviourCount>,
}

impl Variance {
    /// Returns `true` when the samples disagreed with each other.
    #[must_use]
    pub const fn is_flaky(&self) -> bool {
        self.distinct_behaviours > 1
    }
}

/// One distinct behaviour and how often it happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviourCount {
    /// The canonical signature of the behaviour.
    pub signature: String,
    /// How many samples produced it.
    pub count: usize,
    /// Whether samples with this behaviour passed.
    pub passed: bool,
}

/// The judge's numbers for one criterion of one item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionSummary {
    /// What was graded.
    pub criterion: JudgeCriterion,
    /// How many samples were judged.
    pub samples_judged: usize,
    /// Mean of the per-sample majority scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_score: Option<f64>,
    /// Lowest per-sample majority score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_score: Option<u8>,
    /// Highest per-sample majority score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_score: Option<u8>,
    /// Mean spread between the votes *within* a sample: the judge's own
    /// variance, which is what `votes_per_sample` reduces.
    pub mean_vote_spread: f64,
    /// How many votes returned nothing at all.
    pub failed_votes: usize,
}

/// Everything one item produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemReport {
    /// The item.
    pub id: ItemId,
    /// Its name.
    pub name: String,
    /// Its tags.
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// What the item contained when this run measured it, part by part, with
    /// the corpus's `derived` declaration recorded alongside.
    ///
    /// [`crate::baseline::compare`] reads it to answer "is this still the same
    /// experiment?". A report archived before fingerprints existed carries
    /// [`ItemFingerprint::is_unknown`], and a comparison against it counts the
    /// item as an unverified pairing rather than pretending it checked one.
    #[serde(default)]
    pub fingerprint: ItemFingerprint,
    /// Every sample, in order.
    pub samples: Vec<SampleReport>,
}

impl ItemReport {
    /// How many samples ran.
    #[must_use]
    pub fn total_samples(&self) -> usize {
        self.samples.len()
    }

    /// Samples where the model proposed something and the runtime journaled
    /// nothing.
    ///
    /// The second of the three numbers described at the top of this module,
    /// and the one a corpus of
    /// forbidden effects cannot produce on its own. A refused proposal is not a
    /// failure — the item may well pass, and should, because nothing happened —
    /// but it is the model reading the turn wrongly, and a design that claims to
    /// make wrong readings harmless has to be able to say how often it is doing
    /// that work.
    ///
    /// Samples the harness could not set up are not counted: nothing was
    /// proposed there because nothing ran.
    ///
    /// Counted from the REDUCTION's verdict on each act, not from the command
    /// count. «Proposed something and journaled nothing» reads like the same
    /// question and is not: an act that is valid and changes nothing, and one
    /// that is waiting for a person to confirm it, both journal zero commands
    /// and neither was refused — so the design working reported as the model
    /// failing. It went the other way too: a plan holding one refusal beside
    /// one act that did write journals a command, and the refusal disappeared.
    #[must_use]
    pub fn refused_proposals(&self) -> usize {
        self.samples
            .iter()
            .filter(|sample| sample.harness_error.is_none() && sample.acts_refused > 0)
            .count()
    }

    /// Samples where the runtime threw away at least one model answer.
    ///
    /// Counted per sample and not per answer, so the number is comparable with
    /// [`total_samples`](Self::total_samples): a turn that lost three answers
    /// in a row is one turn that struggled, not three.
    ///
    /// Samples the harness could not set up are not counted, for the same
    /// reason as in [`refused_proposals`](Self::refused_proposals): nothing
    /// ran, so nothing was discarded.
    #[must_use]
    pub fn samples_with_discards(&self) -> usize {
        self.samples
            .iter()
            .filter(|sample| sample.harness_error.is_none() && !sample.discarded_answers.is_empty())
            .count()
    }

    /// How many satisfied every deterministic expectation.
    #[must_use]
    pub fn samples_passed(&self) -> usize {
        self.samples.iter().filter(|s| s.passed()).count()
    }

    /// Fraction of samples that satisfied every deterministic expectation.
    ///
    /// Judge scores never enter this number.
    #[must_use]
    pub fn deterministic_pass_rate(&self) -> f64 {
        ratio(self.samples_passed(), self.total_samples())
    }

    /// Returns `true` when some samples passed and others did not. A flaky item
    /// is a result, not an error.
    #[must_use]
    pub fn is_flaky(&self) -> bool {
        let passed = self.samples_passed();
        passed > 0 && passed < self.total_samples()
    }

    /// How much the agent varied across samples.
    #[must_use]
    pub fn variance(&self) -> Variance {
        let mut behaviours: Vec<BehaviourCount> = Vec::new();
        for sample in &self.samples {
            match behaviours
                .iter_mut()
                .find(|found| found.signature == sample.signature)
            {
                Some(found) => found.count += 1,
                None => behaviours.push(BehaviourCount {
                    signature: sample.signature.clone(),
                    count: 1,
                    passed: sample.passed(),
                }),
            }
        }
        behaviours.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.signature.cmp(&right.signature))
        });
        let pass_rate = self.deterministic_pass_rate();
        Variance {
            samples: self.total_samples(),
            distinct_behaviours: behaviours.len(),
            pass_rate,
            pass_variance: pass_rate * (1.0 - pass_rate),
            behaviours,
        }
    }

    /// Every deterministic failure of every sample.
    #[must_use]
    pub fn failures(&self) -> Vec<&AssertionFailure> {
        self.samples
            .iter()
            .flat_map(|sample| sample.failures.iter())
            .collect()
    }

    /// How many samples failed at least one expectation of `category`.
    #[must_use]
    pub fn samples_failing(&self, category: ReliabilityCategory) -> usize {
        self.samples
            .iter()
            .filter(|sample| {
                sample
                    .failures
                    .iter()
                    .any(|failure| ReliabilityCategory::of(failure.expectation) == category)
            })
            .count()
    }

    /// The judge's numbers, one row per criterion that was graded.
    #[must_use]
    pub fn judge_summaries(&self) -> Vec<CriterionSummary> {
        let mut summaries = Vec::new();
        for criterion in JudgeCriterion::ALL {
            let outcomes: Vec<&CriterionOutcome> = self
                .samples
                .iter()
                .flat_map(|sample| sample.judge.iter())
                .filter(|outcome| outcome.criterion == criterion)
                .collect();
            if outcomes.is_empty() {
                continue;
            }
            summaries.push(summarize(criterion, &outcomes));
        }
        summaries
    }
}

fn summarize(criterion: JudgeCriterion, outcomes: &[&CriterionOutcome]) -> CriterionSummary {
    let majorities: Vec<u8> = outcomes
        .iter()
        .filter_map(|outcome| outcome.majority_score())
        .collect();
    let mean_score = if majorities.is_empty() {
        None
    } else {
        let total: u32 = majorities.iter().map(|score| u32::from(*score)).sum();
        Some(f64::from(total) / precise(majorities.len()))
    };
    let spread_total: u32 = outcomes
        .iter()
        .map(|outcome| u32::from(outcome.spread()))
        .sum();
    CriterionSummary {
        criterion,
        samples_judged: outcomes.len(),
        mean_score,
        min_score: majorities.iter().copied().min(),
        max_score: majorities.iter().copied().max(),
        mean_vote_spread: f64::from(spread_total) / precise(outcomes.len()),
        failed_votes: outcomes.iter().map(|o| o.failed_votes()).sum(),
    }
}

fn precise(value: usize) -> f64 {
    if value == 0 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

/// The dashboard of §26.3, with nothing averaged across its rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reliability {
    /// Samples whose effects were wrong.
    pub side_effect_integrity: CategoryStats,
    /// Samples whose claims about themselves were wrong.
    pub operational_claim_integrity: CategoryStats,
    /// Samples whose reading of the message was wrong.
    pub semantic_interpretation: CategoryStats,
    /// Samples whose cards were not what the corpus expected.
    pub clarification_integrity: CategoryStats,
    /// Fraction of samples in which the turn raised at least one card.
    pub clarification_rate: f64,
    /// Fraction of samples in which the turn produced nothing usable.
    pub abandonment_rate: f64,
    /// Fraction of samples in which a provider attempt failed or fell back.
    pub provider_failure_rate: f64,
    /// What the judges thought, never mixed into anything above.
    pub user_experience: Vec<CriterionSummary>,
}

/// Samples counted for one category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryStats {
    /// Total samples in the run.
    pub samples: usize,
    /// Samples with at least one failure in this category.
    pub failing_samples: usize,
    /// Individual failures in this category.
    pub failures: usize,
}

impl CategoryStats {
    /// Fraction of samples with at least one failure in this category.
    #[must_use]
    pub fn failure_rate(&self) -> f64 {
        ratio(self.failing_samples, self.samples)
    }
}

/// One whole run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalReport {
    /// The suite that ran.
    pub suite: String,
    /// When the run finished.
    pub generated_at: DateTime<Utc>,
    /// The configuration it ran under, so a reader can see how many samples and
    /// how many votes produced these numbers.
    pub config: EvalConfig,
    /// Every item, in suite order.
    pub items: Vec<ItemReport>,
}

impl EvalReport {
    /// Builds a report.
    #[must_use]
    pub fn new(
        suite: impl Into<String>,
        generated_at: DateTime<Utc>,
        config: EvalConfig,
        items: Vec<ItemReport>,
    ) -> Self {
        Self {
            suite: suite.into(),
            generated_at,
            config,
            items,
        }
    }

    /// One item by identifier.
    #[must_use]
    pub fn item(&self, id: &ItemId) -> Option<&ItemReport> {
        self.items.iter().find(|item| &item.id == id)
    }

    /// Total samples across the run.
    #[must_use]
    pub fn total_samples(&self) -> usize {
        self.items.iter().map(ItemReport::total_samples).sum()
    }

    /// Samples that satisfied every deterministic expectation.
    #[must_use]
    pub fn samples_passed(&self) -> usize {
        self.items.iter().map(ItemReport::samples_passed).sum()
    }

    /// Fraction of samples that satisfied every deterministic expectation.
    #[must_use]
    pub fn deterministic_pass_rate(&self) -> f64 {
        ratio(self.samples_passed(), self.total_samples())
    }

    /// Items whose samples disagreed with each other.
    #[must_use]
    pub fn flaky_items(&self) -> Vec<&ItemReport> {
        self.items.iter().filter(|item| item.is_flaky()).collect()
    }

    /// The dashboard of §26.3.
    #[must_use]
    pub fn reliability(&self) -> Reliability {
        let samples = self.total_samples();
        let all: Vec<&SampleReport> = self
            .items
            .iter()
            .flat_map(|item| item.samples.iter())
            .collect();
        Reliability {
            side_effect_integrity: self.stats(ReliabilityCategory::SideEffectIntegrity),
            operational_claim_integrity: self.stats(ReliabilityCategory::OperationalClaimIntegrity),
            semantic_interpretation: self.stats(ReliabilityCategory::SemanticInterpretation),
            clarification_integrity: self.stats(ReliabilityCategory::Clarification),
            clarification_rate: ratio(all.iter().filter(|s| s.cards_created > 0).count(), samples),
            abandonment_rate: ratio(all.iter().filter(|s| s.abandoned).count(), samples),
            provider_failure_rate: ratio(
                all.iter().filter(|s| s.provider_failures > 0).count(),
                samples,
            ),
            user_experience: self.judge_summaries(),
        }
    }

    /// The judge's numbers across the whole suite.
    #[must_use]
    pub fn judge_summaries(&self) -> Vec<CriterionSummary> {
        let mut summaries = Vec::new();
        for criterion in JudgeCriterion::ALL {
            let outcomes: Vec<&CriterionOutcome> = self
                .items
                .iter()
                .flat_map(|item| item.samples.iter())
                .flat_map(|sample| sample.judge.iter())
                .filter(|outcome| outcome.criterion == criterion)
                .collect();
            if outcomes.is_empty() {
                continue;
            }
            summaries.push(summarize(criterion, &outcomes));
        }
        summaries
    }

    fn stats(&self, category: ReliabilityCategory) -> CategoryStats {
        let mut stats = CategoryStats {
            samples: self.total_samples(),
            ..CategoryStats::default()
        };
        for item in &self.items {
            stats.failing_samples += item.samples_failing(category);
            stats.failures += item
                .failures()
                .into_iter()
                .filter(|failure| ReliabilityCategory::of(failure.expectation) == category)
                .count();
        }
        stats
    }

    /// The machine-readable form, for a continuous integration gate to archive
    /// and for [`crate::baseline`] to read back.
    ///
    /// # Errors
    ///
    /// [`ReportError::Serialize`] when the report cannot be rendered as JSON.
    pub fn to_json(&self) -> Result<String, ReportError> {
        serde_json::to_string_pretty(self).map_err(|error| ReportError::Serialize {
            message: error.to_string(),
        })
    }

    /// Reads a report back from its machine-readable form.
    ///
    /// # Errors
    ///
    /// [`ReportError::Deserialize`] when the document is not a report.
    pub fn from_json(source: &str) -> Result<Self, ReportError> {
        serde_json::from_str(source).map_err(|error| ReportError::Deserialize {
            message: error.to_string(),
        })
    }

    /// A readable summary, one block per item plus the dashboard.
    #[must_use]
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "suite {}: {} items, {} samples/item, {} votes/sample",
            self.suite,
            self.items.len(),
            self.config.execution.samples_per_item,
            self.config.judging.votes_per_sample
        );
        let _ = writeln!(
            out,
            "deterministic pass rate {:.0}% ({}/{} samples)",
            self.deterministic_pass_rate() * 100.0,
            self.samples_passed(),
            self.total_samples()
        );
        // The second number, on its own line and never folded into the first.
        // A refused proposal is a passing sample — nothing happened, which is
        // what the item asked — so a report that showed only the pass rate would
        // hide exactly the turns where the model read the situation wrongly and
        // the runtime carried it.
        let refused: usize = self.items.iter().map(ItemReport::refused_proposals).sum();
        let measured: usize = self
            .items
            .iter()
            .flat_map(|item| &item.samples)
            .filter(|sample| sample.harness_error.is_none())
            .count();
        if measured > 0 {
            #[allow(clippy::cast_precision_loss)] // counts, not quantities
            let share = refused as f64 / measured as f64 * 100.0;
            let _ = writeln!(
                out,
                "refused proposals {share:.0}% ({refused}/{measured} measured samples): \
                 the model proposed an act and nothing was journaled"
            );
            // The third number. A discarded answer is invisible in both of the
            // others: the sample usually passes, because the repair round
            // recovered, and when it does not the turn simply has no effects to
            // report. This is where a turn that closed without proposing or
            // asking anything finally shows up as something other than silence.
            let discarded: usize = self
                .items
                .iter()
                .map(ItemReport::samples_with_discards)
                .sum();
            #[allow(clippy::cast_precision_loss)] // counts, not quantities
            let share = discarded as f64 / measured as f64 * 100.0;
            let _ = writeln!(
                out,
                "discarded answers {share:.0}% ({discarded}/{measured} measured samples): \
                 the runtime threw the model's answer away whole{}",
                match self.discard_codes() {
                    codes if codes.is_empty() => String::new(),
                    codes => format!(": {codes}"),
                }
            );
        }
        if let Some(line) = self.task_accuracy() {
            let _ = writeln!(out, "understanding by task: {line}");
        }
        for item in &self.items {
            write_item(&mut out, item);
        }
        write_reliability(&mut out, &self.reliability());
        out
    }

    /// Each understanding task's accuracy over the samples whose item says what it
    /// expects of that task, as `segment 40/46 · route 30/31`; `None` when no item
    /// says.
    #[must_use]
    pub fn task_accuracy(&self) -> Option<String> {
        let samples: Vec<&SampleReport> = self
            .items
            .iter()
            .flat_map(|item| &item.samples)
            .filter(|sample| sample.harness_error.is_none())
            .collect();
        let tasks = crate::understanding::TaskScores::default().by_task();
        let line: Vec<String> = tasks
            .iter()
            .enumerate()
            .filter_map(|(at, (task, _))| {
                let scored: Vec<bool> = samples
                    .iter()
                    .filter_map(|sample| sample.tasks.by_task()[at].1)
                    .collect();
                if scored.is_empty() {
                    return None;
                }
                let passed = scored.iter().filter(|pass| **pass).count();
                Some(format!("{task} {passed}/{}", scored.len()))
            })
            .collect();
        (!line.is_empty()).then(|| line.join(" · "))
    }

    /// The discarded-answer codes of the whole run, commonest first, rendered
    /// as `code×n`.
    ///
    /// Which code it is decides what to do about it, and the two that this
    /// corpus produces want opposite answers: `evidence` means the model cited
    /// something the turn does not contain, which is a reading the grounding
    /// rule caught, while `schema_violation` means the document was the wrong
    /// shape, which is a schema the model cannot satisfy. Reporting only a rate
    /// would leave the reader unable to tell them apart.
    #[must_use]
    pub fn discard_codes(&self) -> String {
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for sample in self.items.iter().flat_map(|item| &item.samples) {
            for code in &sample.discarded_answers {
                *counts.entry(code.as_str()).or_default() += 1;
            }
        }
        let mut ordered: Vec<(&str, usize)> = counts.into_iter().collect();
        // Commonest first, and alphabetical within a tie so two runs of the same
        // numbers render the same string.
        ordered.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
        ordered
            .iter()
            .map(|(code, count)| format!("{code}×{count}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Applies a gate's thresholds.
    #[must_use]
    pub fn gate(&self, thresholds: &GateThresholds) -> GateOutcome {
        let mut violations = Vec::new();
        let rate = self.deterministic_pass_rate();
        if rate < thresholds.min_deterministic_pass_rate {
            violations.push(GateViolation {
                scope: self.suite.clone(),
                rule: "min_deterministic_pass_rate".to_owned(),
                detail: format!("{rate:.3} < {:.3}", thresholds.min_deterministic_pass_rate),
            });
        }
        let reliability = self.reliability();
        if thresholds.forbid_side_effect_failures
            && reliability.side_effect_integrity.failing_samples > 0
        {
            violations.push(GateViolation {
                scope: self.suite.clone(),
                rule: "forbid_side_effect_failures".to_owned(),
                detail: format!(
                    "{} samples failed a side-effect integrity assertion",
                    reliability.side_effect_integrity.failing_samples
                ),
            });
        }
        if reliability.abandonment_rate > thresholds.max_abandonment_rate {
            violations.push(GateViolation {
                scope: self.suite.clone(),
                rule: "max_abandonment_rate".to_owned(),
                detail: format!(
                    "{:.3} > {:.3}",
                    reliability.abandonment_rate, thresholds.max_abandonment_rate
                ),
            });
        }
        if !thresholds.allow_flaky_items {
            for item in self.flaky_items() {
                violations.push(GateViolation {
                    scope: item.id.to_string(),
                    rule: "allow_flaky_items".to_owned(),
                    detail: format!(
                        "{}/{} samples passed",
                        item.samples_passed(),
                        item.total_samples()
                    ),
                });
            }
        }
        if let Some(minimum) = thresholds.min_judge_score {
            for summary in &reliability.user_experience {
                if summary.mean_score.is_some_and(|score| score < minimum) {
                    violations.push(GateViolation {
                        scope: summary.criterion.to_string(),
                        rule: "min_judge_score".to_owned(),
                        detail: format!(
                            "{:.2} < {minimum:.2}",
                            summary.mean_score.unwrap_or_default()
                        ),
                    });
                }
            }
        }
        GateOutcome {
            passed: violations.is_empty(),
            violations,
        }
    }
}

fn write_item(out: &mut String, item: &ItemReport) {
    use std::fmt::Write as _;
    let variance = item.variance();
    let _ = writeln!(
        out,
        "  {}: {}/{} samples passed, {} distinct behaviour(s){}",
        item.id,
        item.samples_passed(),
        item.total_samples(),
        variance.distinct_behaviours,
        if item.is_flaky() { ", FLAKY" } else { "" }
    );
    // An unmeasured sample is not a failing one, and a summary that hid the
    // difference would read as "the agent got it wrong" when the truth is that
    // nobody asked it anything.
    for sample in &item.samples {
        if let Some(error) = &sample.harness_error {
            let _ = writeln!(out, "      sample {} not measured: {error}", sample.sample);
        }
    }
    // Only when it happened: an ordinary item's block is unchanged, and an
    // item whose answers were being thrown away says so next to its own
    // numbers rather than only in the run-wide total.
    let discarded = item.samples_with_discards();
    if discarded > 0 {
        let mut codes: Vec<&str> = item
            .samples
            .iter()
            .flat_map(|sample| sample.discarded_answers.iter().map(String::as_str))
            .collect();
        codes.sort_unstable();
        codes.dedup();
        // The measured samples, not every sample: the numerator already
        // excludes the ones the harness could not set up, and mixing the two
        // printed `1/2` under a run-wide line that correctly said `1/1`.
        let measured = item
            .samples
            .iter()
            .filter(|sample| sample.harness_error.is_none())
            .count();
        let _ = writeln!(
            out,
            "      {discarded}/{measured} sample(s) had an answer discarded whole: {}",
            codes.join(", ")
        );
    }
    for failure in item.failures() {
        let _ = writeln!(
            out,
            "      [{}] {failure}",
            ReliabilityCategory::of(failure.expectation)
        );
    }
    for summary in item.judge_summaries() {
        let _ = writeln!(
            out,
            "      judge {}: mean {:.2} over {} sample(s), vote spread {:.2}",
            summary.criterion,
            summary.mean_score.unwrap_or_default(),
            summary.samples_judged,
            summary.mean_vote_spread
        );
    }
}

fn write_reliability(out: &mut String, reliability: &Reliability) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "reliability (spec §26.3, categories kept apart):");
    for (label, stats) in [
        ("side-effect integrity", reliability.side_effect_integrity),
        (
            "operational claim integrity",
            reliability.operational_claim_integrity,
        ),
        (
            "semantic interpretation",
            reliability.semantic_interpretation,
        ),
        (
            "clarification integrity",
            reliability.clarification_integrity,
        ),
    ] {
        let _ = writeln!(
            out,
            "  {label}: {} failing sample(s), {} failure(s)",
            stats.failing_samples, stats.failures
        );
    }
    let _ = writeln!(
        out,
        "  clarification rate {:.0}%, abandonment rate {:.0}%, provider failure rate {:.0}%",
        reliability.clarification_rate * 100.0,
        reliability.abandonment_rate * 100.0,
        reliability.provider_failure_rate * 100.0
    );
    for summary in &reliability.user_experience {
        let _ = writeln!(
            out,
            "  user experience, {}: mean {:.2}, vote spread {:.2}",
            summary.criterion,
            summary.mean_score.unwrap_or_default(),
            summary.mean_vote_spread
        );
    }
}

/// What a continuous integration gate refuses to merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GateThresholds {
    /// Minimum fraction of samples that must satisfy every deterministic
    /// expectation.
    pub min_deterministic_pass_rate: f64,
    /// Maximum fraction of samples that may produce nothing usable.
    pub max_abandonment_rate: f64,
    /// Whether an item whose samples disagree may still pass.
    pub allow_flaky_items: bool,
    /// Whether any side-effect integrity failure fails the gate outright,
    /// whatever the pass rate is. On by default: §26.3 exists so this is not a
    /// percentage.
    pub forbid_side_effect_failures: bool,
    /// Minimum mean judge score, when the run judged anything.
    pub min_judge_score: Option<f64>,
}

impl Default for GateThresholds {
    fn default() -> Self {
        Self {
            min_deterministic_pass_rate: 1.0,
            max_abandonment_rate: 0.0,
            allow_flaky_items: false,
            forbid_side_effect_failures: true,
            min_judge_score: None,
        }
    }
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateOutcome {
    /// Whether the run may merge.
    pub passed: bool,
    /// Every threshold that was not met.
    pub violations: Vec<GateViolation>,
}

/// One unmet threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateViolation {
    /// The suite, item or criterion it is about.
    pub scope: String,
    /// Which threshold.
    pub rule: String,
    /// The numbers.
    pub detail: String,
}

impl std::fmt::Display for GateViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]: {}", self.rule, self.scope, self.detail)
    }
}

/// Why a report could not be rendered or read back.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReportError {
    /// The report could not be rendered as JSON.
    #[error("report could not be serialized: {message}")]
    Serialize {
        /// What `serde` said.
        message: String,
    },
    /// The document is not a report.
    #[error("report could not be read: {message}")]
    Deserialize {
        /// What `serde` said.
        message: String,
    },
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_refused_proposal_is_counted_and_a_carried_one_is_not() {
        let mut item = ItemReport {
            id: ItemId::new("i"),
            name: "An item".to_owned(),
            tags: Vec::new(),
            fingerprint: ItemFingerprint::default(),
            samples: vec![sample(1, "A", false)],
        };
        // Proposed and executed: the model read it right.
        item.samples[0].acts_proposed = 1;
        item.samples[0].acts_refused = 0;
        item.samples[0].commands_journaled = 1;
        assert_eq!(item.refused_proposals(), 0);

        // Refused: the structure turned the reading down. The sample may still
        // pass — nothing happened, which is often what the item asked — and
        // that is exactly why this is counted separately.
        item.samples[0].acts_refused = 1;
        item.samples[0].commands_journaled = 0;
        assert_eq!(item.refused_proposals(), 1);

        // The two that journal nothing and are NOT refusals, which is the whole
        // reason this is not counted off the command total: an act valid on a
        // record already in the requested state, and one waiting for a person
        // to confirm it. Counting either reports the design working as the
        // model failing.
        item.samples[0].acts_refused = 0;
        assert_eq!(item.refused_proposals(), 0);

        // And the other way round: a plan that refused one act while another
        // one wrote. The command count hides this entirely.
        item.samples[0].acts_proposed = 2;
        item.samples[0].acts_refused = 1;
        item.samples[0].commands_journaled = 1;
        assert_eq!(item.refused_proposals(), 1);

        // Proposed nothing: an answer, not a refusal.
        item.samples[0].acts_proposed = 0;
        item.samples[0].acts_refused = 0;
        assert_eq!(item.refused_proposals(), 0);

        // Never ran: the harness could not build the world, so there was no
        // reading to be right or wrong about.
        item.samples[0].acts_proposed = 1;
        item.samples[0].acts_refused = 1;
        item.samples[0].harness_error = Some("no world".to_owned());
        assert_eq!(item.refused_proposals(), 0);
    }
    use super::*;
    use crate::judge::{JudgeVerdict, JudgeVote};

    fn sample(index: u32, signature: &str, failing: bool) -> SampleReport {
        SampleReport {
            sample: index,
            failures: if failing {
                vec![AssertionFailure::new(
                    ExpectationName::Commands,
                    "[a]",
                    "[b]",
                )]
            } else {
                Vec::new()
            },
            harness_error: None,
            signature: signature.to_owned(),
            judge: Vec::new(),
            acts_proposed: 0,
            acts_refused: 0,
            commands_journaled: 0,
            provider_failures: 0,
            cards_created: 0,
            abandoned: false,
            discarded_answers: Vec::new(),
            answer: String::new(),
            tasks: Default::default(),
        }
    }

    fn report(samples: Vec<SampleReport>) -> EvalReport {
        EvalReport::new(
            "s",
            DateTime::from_timestamp(0, 0).unwrap_or_default(),
            EvalConfig::default(),
            vec![ItemReport {
                id: ItemId::new("i"),
                name: "An item".to_owned(),
                tags: Vec::new(),
                fingerprint: ItemFingerprint::default(),
                samples,
            }],
        )
    }

    /// The third number reaches the summary, and says which code it was.
    ///
    /// The scenario is the one it exists for: every sample PASSED. An answer
    /// was thrown away and the repair round recovered, so the effects are right
    /// and the pass rate is 100% — and a report that stopped there would
    /// describe a turn that struggled as a turn that did not.
    #[test]
    fn a_discarded_answer_is_reported_even_when_every_sample_passed() {
        let mut passing = sample(1, "A", false);
        passing.discarded_answers = vec!["evidence".to_owned()];
        let mut clean = sample(2, "A", false);
        clean.discarded_answers.clear();
        let report = report(vec![passing, clean]);

        assert_eq!(report.items[0].samples_with_discards(), 1);
        assert!(
            (report.deterministic_pass_rate() - 1.0).abs() < 1e-9,
            "every sample passed, which is the whole point of the number"
        );

        let summary = report.summary();
        assert!(
            summary.contains("discarded answers 50% (1/2 measured samples)"),
            "the run-wide line states the rate: {summary}"
        );
        assert!(
            summary.contains("evidence×1"),
            "and which code it was, because the answer differs by code: {summary}"
        );
        assert!(
            summary.contains("1/2 sample(s) had an answer discarded whole: evidence"),
            "the item says it next to its own numbers too: {summary}"
        );
    }

    /// Counted per sample, not per answer.
    ///
    /// A turn that lost three answers in a row is one turn that struggled. The
    /// per-answer count is still there in the codes, which is where it belongs:
    /// it says what went wrong, not how many turns went wrong.
    #[test]
    fn a_sample_that_lost_three_answers_is_one_sample() {
        let mut struggled = sample(1, "A", false);
        struggled.discarded_answers = vec![
            "evidence".to_owned(),
            "evidence".to_owned(),
            "schema_violation".to_owned(),
        ];
        let report = report(vec![struggled, sample(2, "A", false)]);

        assert_eq!(report.items[0].samples_with_discards(), 1);
        assert_eq!(report.discard_codes(), "evidence×2, schema_violation×1");
    }

    /// A sample that never ran discarded nothing.
    ///
    /// Same rule as the refused proposals above it: the harness could not build
    /// the world, so there was no answer to throw away.
    #[test]
    fn an_unmeasured_sample_discards_nothing() {
        let mut never_ran = sample(1, "A", false);
        never_ran.discarded_answers = vec!["evidence".to_owned()];
        never_ran.harness_error = Some("no world".to_owned());
        let report = report(vec![never_ran]);
        assert_eq!(report.items[0].samples_with_discards(), 0);
    }

    #[test]
    fn variance_counts_distinct_behaviours_not_failures() {
        let report = report(vec![
            sample(1, "A", false),
            sample(2, "B", true),
            sample(3, "A", false),
        ]);
        let variance = report.items[0].variance();
        assert_eq!(variance.distinct_behaviours, 2);
        assert!((variance.pass_rate - 2.0 / 3.0).abs() < 1e-9);
        assert!(variance.pass_variance > 0.0);
        assert_eq!(variance.behaviours[0].count, 2);
        assert!(report.items[0].is_flaky());
    }

    #[test]
    fn a_side_effect_failure_is_not_averaged_into_a_language_score() {
        let mut failing = sample(1, "A", true);
        failing.judge = vec![CriterionOutcome {
            criterion: JudgeCriterion::Tone,
            votes: vec![JudgeVote {
                vote: 1,
                verdict: Some(JudgeVerdict {
                    score: 5,
                    reason: "fine".to_owned(),
                }),
                error: None,
            }],
        }];
        let report = report(vec![failing]);
        let reliability = report.reliability();
        assert_eq!(reliability.side_effect_integrity.failing_samples, 1);
        assert_eq!(reliability.user_experience[0].mean_score, Some(5.0));
        assert!((report.deterministic_pass_rate() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn the_default_gate_refuses_a_side_effect_failure() {
        let outcome = report(vec![sample(1, "A", true)]).gate(&GateThresholds::default());
        assert!(!outcome.passed);
        assert!(
            outcome
                .violations
                .iter()
                .any(|v| v.rule == "forbid_side_effect_failures"),
            "{:?}",
            outcome.violations
        );
    }

    #[test]
    fn a_report_round_trips_through_its_machine_readable_form() {
        let report = report(vec![sample(1, "A", false)]);
        let json = report.to_json().unwrap();
        assert_eq!(EvalReport::from_json(&json).unwrap(), report);
    }
}
