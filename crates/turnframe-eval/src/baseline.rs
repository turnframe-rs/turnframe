//! Comparing a run against a previous one — and refusing to, when the two runs
//! stopped being the same experiment.
//!
//! Two things look like "the evaluation got worse" and must not be treated the
//! same. A **deterministic regression** is an item that used to satisfy its
//! assertions and no longer does: something the agent *does* changed, it is
//! reproducible, and it can be a merge blocker. A **judge drift** is the same
//! behaviour graded differently: a signal about the measurement, and gating a
//! merge on it is how a team learns to ignore its own evaluation. [`ChangeKind`]
//! keeps them in separate variants.
//!
//! The third thing is an item that changed because the code under test generates
//! part of it, which silently unpairs the comparison. Exclusion is then as loud
//! as the score, there is no headline to read past
//! [`ComparisonPolicy::max_excluded_share`], and the three kinds of item change
//! have three different names. [`NoiseFloor`] makes a difference news only when
//! it is bigger than the one the unchanged system produces.
//!
//! The reasoning, and what each of the three names means, is in
//! [`docs/evaluation.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/evaluation.md).
//!

use serde::{Deserialize, Serialize};

use crate::assertions::ExpectationName;
use crate::control::NoiseFloor;
use crate::corpus::{ItemId, ItemPart, PartProvenance};
use crate::judge::{JudgeCriterion, ratio};
use crate::report::{EvalReport, ItemReport, ReliabilityCategory};

/// How much movement is noise rather than news.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DriftTolerance {
    /// Change in deterministic pass rate below which nothing is reported. Zero
    /// by default: a deterministic assertion either holds or it does not, and
    /// pretending a drop of one sample in twenty is noise is how a regression
    /// gets shipped.
    pub pass_rate_epsilon: f64,
    /// Change in mean judge score below which nothing is reported. A judge
    /// moving by a tenth of a point is the judge breathing.
    pub judge_score_epsilon: f64,
}

impl Default for DriftTolerance {
    fn default() -> Self {
        Self {
            pass_rate_epsilon: 0.0,
            judge_score_epsilon: 0.25,
        }
    }
}

/// Everything [`compare`] needs beyond the two reports.
///
/// ```
/// use turnframe_eval::baseline::ComparisonPolicy;
///
/// // A tenth of the corpus may go unpaired before a headline is refused.
/// assert!((ComparisonPolicy::default().max_excluded_share - 0.1).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ComparisonPolicy {
    /// How much movement is noise rather than news.
    pub tolerance: DriftTolerance,
    /// The fraction of the paired corpus that may be excluded before the
    /// comparison refuses to produce a headline number.
    ///
    /// A tenth by default, and deliberately low. The number exists because a
    /// report over a third of a corpus reads exactly like a report over all of
    /// it, and nothing in the shape of the output would ever tell a reader
    /// otherwise. Raise it only when you have looked at the exclusions and
    /// decided they are acceptable.
    pub max_excluded_share: f64,
    /// What the same code produced against itself, when a
    /// [control run](crate::control) measured it.
    ///
    /// `None` leaves every change [`NoiseVerdict::Unmeasured`]: without a
    /// control run there is no honest way to say whether a difference is signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noise_floor: Option<NoiseFloor>,
}

impl Default for ComparisonPolicy {
    fn default() -> Self {
        Self {
            tolerance: DriftTolerance::default(),
            max_excluded_share: 0.1,
            noise_floor: None,
        }
    }
}

impl ComparisonPolicy {
    /// A policy with this tolerance and the default exclusion ceiling.
    #[must_use]
    pub fn new(tolerance: DriftTolerance) -> Self {
        Self {
            tolerance,
            ..Self::default()
        }
    }

    /// Sets how much of the paired corpus may be excluded before the headline is
    /// withheld.
    #[must_use]
    pub const fn with_max_excluded_share(mut self, share: f64) -> Self {
        self.max_excluded_share = share;
        self
    }

    /// Attaches the floor a [control run](crate::control) measured.
    #[must_use]
    pub fn with_noise_floor(mut self, floor: NoiseFloor) -> Self {
        self.noise_floor = Some(floor);
        self
    }
}

/// What changed between two runs of the same suite.
///
/// The field order is the reading order, and it is deliberate: the headline (or
/// the refusal to produce one) and the exclusions come before the changes, in
/// the machine-readable form as much as in [`Comparison::summary`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    /// The suite the baseline reported on.
    pub baseline_suite: String,
    /// The suite the current run reported on.
    pub current_suite: String,
    /// The figures, or the reason there are none.
    pub headline: Headline,
    /// Items that appeared in both runs and could not be paired, with why.
    pub excluded: Vec<ExcludedItem>,
    /// Every change on the items that stayed paired, in current-run item order
    /// followed by removals.
    pub changes: Vec<Change>,
}

impl Comparison {
    /// Returns `true` when nothing changed and nothing was excluded.
    #[must_use]
    pub fn is_unchanged(&self) -> bool {
        self.changes.is_empty() && self.excluded.is_empty()
    }

    /// The deterministic regressions — behaviour that got worse.
    #[must_use]
    pub fn deterministic_regressions(&self) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|change| matches!(change.kind, ChangeKind::DeterministicRegression { .. }))
            .collect()
    }

    /// The deterministic regressions that a [control run](crate::control) says
    /// are larger than the harness's own variation.
    ///
    /// Without a noise floor this is every regression: an
    /// [`Unmeasured`](NoiseVerdict::Unmeasured) movement is not known to be
    /// noise, and treating the unknown as harmless is how a regression ships.
    #[must_use]
    pub fn signal_regressions(&self) -> Vec<&Change> {
        self.deterministic_regressions()
            .into_iter()
            .filter(|change| change.against_noise != NoiseVerdict::WithinNoise)
            .collect()
    }

    /// The judge drifts — the same behaviour, graded differently.
    #[must_use]
    pub fn judge_drifts(&self) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|change| matches!(change.kind, ChangeKind::JudgeDrift { .. }))
            .collect()
    }

    /// The items whose declared-derived parts changed: the intended effect of
    /// the change being measured, and not a broken pairing.
    #[must_use]
    pub fn derived_input_changes(&self) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|change| matches!(change.kind, ChangeKind::DerivedInput { .. }))
            .collect()
    }

    /// Returns `true` when at least one item's deterministic assertions got
    /// worse.
    #[must_use]
    pub fn has_deterministic_regression(&self) -> bool {
        !self.deterministic_regressions().is_empty()
    }

    /// Returns `true` when at least one regression exceeds the measured noise
    /// floor. This is the question a merge gate should ask.
    #[must_use]
    pub fn has_signal_regression(&self) -> bool {
        !self.signal_regressions().is_empty()
    }

    /// Returns `true` when at least one judge score moved beyond the tolerance.
    /// This is a question about the measurement, not about the agent.
    #[must_use]
    pub fn has_judge_drift(&self) -> bool {
        !self.judge_drifts().is_empty()
    }

    /// The excluded items whose `recorded` parts changed.
    ///
    /// These are not a result about the model and must never be read as one:
    /// something regenerated testimony about what the system actually emitted,
    /// which is a defect in the corpus or in the tooling that touched it. They
    /// are excluded from every figure, and this is where they are named.
    #[must_use]
    pub fn corpus_defects(&self) -> Vec<&ExcludedItem> {
        self.excluded
            .iter()
            .filter(|excluded| excluded.is_corpus_defect())
            .collect()
    }

    /// Returns `true` when a recorded part changed anywhere in the corpus.
    #[must_use]
    pub fn has_corpus_defect(&self) -> bool {
        !self.corpus_defects().is_empty()
    }

    /// The fraction of the paired corpus that could not be compared.
    #[must_use]
    pub fn excluded_share(&self) -> f64 {
        self.headline.excluded_share()
    }

    /// Returns `true` when there is no headline figure to read, because too much
    /// of the corpus went unpaired.
    #[must_use]
    pub fn is_withheld(&self) -> bool {
        matches!(self.headline, Headline::Withheld(_))
    }

    /// A readable rendering.
    ///
    /// The first line is the pairing: how much of the corpus was compared and
    /// how much was dropped. The second is the headline, or the refusal. Only
    /// then come the individual changes, regressions first.
    #[must_use]
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let counts = self.headline.counts();
        let _ = writeln!(
            out,
            "{} → {}: {} of {} paired item(s) compared, {} excluded ({:.0}% of the paired corpus)",
            self.baseline_suite,
            self.current_suite,
            counts.compared,
            counts.compared + counts.excluded,
            counts.excluded,
            self.excluded_share() * 100.0
        );
        if counts.unverified > 0 {
            let _ = writeln!(
                out,
                "  {} item(s) carried no fingerprint on one side, so their pairing was not checked",
                counts.unverified
            );
        }
        let defects = self.corpus_defects();
        if !defects.is_empty() {
            // Said before any figure, because it is not a figure: something
            // rewrote testimony, and no number in this report answers that.
            let _ = writeln!(
                out,
                "CORPUS DEFECT in {} item(s): a `recorded` part changed. Testimony about what the \
                 system emitted must never be regenerated — this is a problem with the corpus or \
                 the tooling, not a result about the model.",
                defects.len()
            );
        }
        match &self.headline {
            Headline::Measured(figures) => {
                let _ = writeln!(
                    out,
                    "deterministic pass rate {:.2} → {:.2} ({:+.2}) over the compared items",
                    figures.baseline_pass_rate, figures.current_pass_rate, figures.pass_rate_delta
                );
            }
            Headline::Withheld(withheld) => {
                let _ = writeln!(out, "NO HEADLINE FIGURE: {}", withheld.reason);
            }
        }
        let _ = writeln!(
            out,
            "{} change(s), {} deterministic regression(s) of which {} exceed the noise floor, \
             {} judge drift(s), {} item(s) changed as the projection intended",
            self.changes.len(),
            self.deterministic_regressions().len(),
            self.signal_regressions().len(),
            self.judge_drifts().len(),
            self.derived_input_changes().len()
        );
        for excluded in &self.excluded {
            let _ = writeln!(out, "  EXCLUDED {excluded}");
        }
        let mut ordered: Vec<&Change> = self.deterministic_regressions();
        ordered.extend(
            self.changes.iter().filter(|change| {
                !matches!(change.kind, ChangeKind::DeterministicRegression { .. })
            }),
        );
        for change in ordered {
            let _ = writeln!(out, "  {change}");
        }
        out
    }
}

/// The figures of a comparison, or the reason there are none.
///
/// There is no accessor anywhere that produces a pass-rate delta outside
/// [`HeadlineFigures`], which is what makes the refusal a refusal rather than a
/// warning a caller can step over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Headline {
    /// Enough of the corpus stayed paired for the figures to mean something.
    Measured(HeadlineFigures),
    /// Too much of the corpus went unpaired. No figure is produced.
    Withheld(WithheldHeadline),
}

/// The counts every headline carries, whatever it decided.
struct HeadlineCounts {
    compared: usize,
    excluded: usize,
    unverified: usize,
}

impl Headline {
    /// The figures, when there are any.
    #[must_use]
    pub const fn figures(&self) -> Option<&HeadlineFigures> {
        match self {
            Self::Measured(figures) => Some(figures),
            Self::Withheld(_) => None,
        }
    }

    /// The refusal, when the headline was withheld.
    #[must_use]
    pub const fn withheld(&self) -> Option<&WithheldHeadline> {
        match self {
            Self::Withheld(reason) => Some(reason),
            Self::Measured(_) => None,
        }
    }

    /// The fraction of the paired corpus that was excluded — reported whether or
    /// not there is a figure beside it.
    #[must_use]
    pub const fn excluded_share(&self) -> f64 {
        match self {
            Self::Measured(figures) => figures.excluded_share,
            Self::Withheld(withheld) => withheld.excluded_share,
        }
    }

    const fn counts(&self) -> HeadlineCounts {
        match self {
            Self::Measured(figures) => HeadlineCounts {
                compared: figures.items_compared,
                excluded: figures.items_excluded,
                unverified: figures.unverified_pairings,
            },
            Self::Withheld(withheld) => HeadlineCounts {
                compared: withheld.items_compared,
                excluded: withheld.items_excluded,
                unverified: withheld.unverified_pairings,
            },
        }
    }
}

/// The figures of a comparison that stood.
///
/// Every rate is over the **compared** items only. An excluded item contributes
/// nothing to either side, because a measurement it is not part of is not a
/// measurement of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadlineFigures {
    /// Deterministic pass rate of the baseline over the compared items.
    pub baseline_pass_rate: f64,
    /// Deterministic pass rate of the current run over the compared items.
    pub current_pass_rate: f64,
    /// Current minus baseline.
    pub pass_rate_delta: f64,
    /// How many items were compared.
    pub items_compared: usize,
    /// How many appeared in both runs and could not be paired.
    pub items_excluded: usize,
    /// Excluded over compared plus excluded.
    pub excluded_share: f64,
    /// How many items carried no fingerprint on one side, so their pairing could
    /// not be checked at all.
    pub unverified_pairings: usize,
}

/// Why a comparison produced no figure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WithheldHeadline {
    /// Excluded over compared plus excluded.
    pub excluded_share: f64,
    /// The ceiling it crossed.
    pub max_excluded_share: f64,
    /// How many items were still comparable.
    pub items_compared: usize,
    /// How many appeared in both runs and could not be paired.
    pub items_excluded: usize,
    /// How many items carried no fingerprint on one side.
    pub unverified_pairings: usize,
    /// One sentence a person reads instead of a number.
    pub reason: String,
}

/// One item that appeared in both runs and could not be paired.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExcludedItem {
    /// Which item.
    pub item: ItemId,
    /// Why its pairing did not survive. An item can hit more than one reason at
    /// once — an edited fixture *and* rewritten testimony — and both are said.
    pub reasons: Vec<ExclusionReason>,
}

impl ExcludedItem {
    /// Returns `true` when one of the reasons is evidence that something
    /// rewrote a recorded part.
    #[must_use]
    pub fn is_corpus_defect(&self) -> bool {
        self.reasons
            .iter()
            .any(|reason| matches!(reason, ExclusionReason::RecordedPartChanged { .. }))
    }
}

impl std::fmt::Display for ExcludedItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: ", self.item)?;
        for (index, reason) in self.reasons.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{reason}")?;
        }
        Ok(())
    }
}

/// Why an item could not be paired between two runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExclusionReason {
    /// Authored parts of the item changed, so the two runs measured two
    /// different scenarios under one identifier. Nothing about this item is
    /// compared.
    PairingBroken {
        /// The parts a person wrote that are no longer the same.
        parts: Vec<ItemPart>,
    },
    /// Parts declared `recorded` changed.
    ///
    /// Testimony about what the system actually emitted does not change on its
    /// own, so something regenerated it — and regenerating a recording destroys
    /// the only property that made it worth keeping. This is a defect in the
    /// corpus or in the tooling that touched it, and it is deliberately *not*
    /// a result about the model: the item is excluded from every figure and
    /// reported on its own terms.
    RecordedPartChanged {
        /// The parts that were supposed to be evidence.
        parts: Vec<ItemPart>,
    },
}

impl std::fmt::Display for ExclusionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PairingBroken { parts } => write!(
                f,
                "pairing broken — authored `{}` changed, so the two runs did not measure the same \
                 scenario",
                joined(parts)
            ),
            Self::RecordedPartChanged { parts } => write!(
                f,
                "CORPUS DEFECT — recorded `{}` changed; testimony must never be regenerated, so \
                 fix the corpus or the tooling rather than reading this as a result",
                joined(parts)
            ),
        }
    }
}

fn joined(parts: &[ItemPart]) -> String {
    parts
        .iter()
        .map(|part| part.as_str())
        .collect::<Vec<&str>>()
        .join("`, `")
}

/// One item's change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// Which item.
    pub item: ItemId,
    /// What changed about it.
    pub kind: ChangeKind,
    /// How the movement sits against a measured noise floor.
    #[serde(default)]
    pub against_noise: NoiseVerdict,
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.item, self.kind)?;
        match self.against_noise {
            NoiseVerdict::Unmeasured => Ok(()),
            NoiseVerdict::WithinNoise => f.write_str(" [within the measured noise floor]"),
            NoiseVerdict::ExceedsNoise => f.write_str(" [exceeds the measured noise floor]"),
        }
    }
}

/// How a movement compares with what the unchanged system produced against
/// itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NoiseVerdict {
    /// No control run was supplied, so nothing can be said. The default, because
    /// assuming a difference is noise without measuring one is the habit this
    /// vocabulary exists to break.
    #[default]
    Unmeasured,
    /// No larger than the movement the same code produced against itself.
    WithinNoise,
    /// Larger than the movement the same code produced against itself.
    ExceedsNoise,
}

/// The kinds of change a comparison reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangeKind {
    /// The item is new in the current run.
    Added {
        /// Its deterministic pass rate.
        pass_rate: f64,
    },
    /// The item was in the baseline and is not in the current run.
    Removed {
        /// Its deterministic pass rate in the baseline.
        pass_rate: f64,
    },
    /// The item's content changed, in parts the corpus declares are written by
    /// the code under test.
    ///
    /// This is the intended effect of the change being measured, not a broken
    /// pairing: the item is still compared, and whatever deterministic or judge
    /// change it also produced is reported beside this one. It is the difference
    /// between "the state block changed because we changed the projector" and
    /// "somebody edited the fixture".
    DerivedInput {
        /// The declared parts whose content changed.
        parts: Vec<ItemPart>,
    },
    /// Deterministic assertions that used to hold no longer do. Behaviour
    /// changed.
    DeterministicRegression {
        /// Pass rate before.
        before: f64,
        /// Pass rate now.
        after: f64,
        /// Expectations that fail now and did not before.
        newly_failing: Vec<ExpectationName>,
        /// The reliability categories those expectations belong to (§26.3).
        categories: Vec<ReliabilityCategory>,
    },
    /// Deterministic assertions that used to fail now hold.
    DeterministicImprovement {
        /// Pass rate before.
        before: f64,
        /// Pass rate now.
        after: f64,
    },
    /// The same behaviour, graded differently. Not a regression.
    JudgeDrift {
        /// Which criterion moved.
        criterion: JudgeCriterion,
        /// Mean score before.
        before: f64,
        /// Mean score now.
        after: f64,
    },
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added { pass_rate } => write!(f, "added, pass rate {pass_rate:.2}"),
            Self::Removed { pass_rate } => write!(f, "removed, was {pass_rate:.2}"),
            Self::DerivedInput { parts } => {
                let names: Vec<&str> = parts.iter().map(|part| part.as_str()).collect();
                write!(
                    f,
                    "derived input changed as intended (`{}`)",
                    names.join("`, `")
                )
            }
            Self::DeterministicRegression {
                before,
                after,
                newly_failing,
                ..
            } => {
                let names: Vec<&str> = newly_failing
                    .iter()
                    .map(|expectation| expectation.as_str())
                    .collect();
                write!(
                    f,
                    "DETERMINISTIC REGRESSION {before:.2} → {after:.2} ({})",
                    names.join(", ")
                )
            }
            Self::DeterministicImprovement { before, after } => {
                write!(f, "improved {before:.2} → {after:.2}")
            }
            Self::JudgeDrift {
                criterion,
                before,
                after,
            } => write!(f, "judge drift on {criterion}: {before:.2} → {after:.2}"),
        }
    }
}

/// Compares a current run against a baseline.
///
/// Items are joined by identifier, and then by fingerprint: an item whose
/// undeclared content changed between the two runs is **excluded**, not
/// compared, because the two runs did not measure the same scenario. An item
/// whose *declared derived* content changed is compared and reports a
/// [`ChangeKind::DerivedInput`] beside whatever else it produced.
///
/// An item that changed both its behaviour and its judge score produces two
/// changes, one of each kind, because they are two different pieces of news.
///
/// The headline is withheld — no pass-rate figure at all — when the excluded
/// share crosses [`ComparisonPolicy::max_excluded_share`], or when nothing is
/// left to compare.
#[must_use]
pub fn compare(
    baseline: &EvalReport,
    current: &EvalReport,
    policy: &ComparisonPolicy,
) -> Comparison {
    let mut changes = Vec::new();
    let mut excluded = Vec::new();
    let mut compared: Vec<ItemId> = Vec::new();
    let mut unverified = 0_usize;

    for item in &current.items {
        let Some(before) = baseline.item(&item.id) else {
            changes.push(change(
                item.id.clone(),
                ChangeKind::Added {
                    pass_rate: item.deterministic_pass_rate(),
                },
                policy,
            ));
            continue;
        };
        if before.fingerprint.is_unknown() || item.fingerprint.is_unknown() {
            unverified += 1;
        }

        let sorted = sort_by_provenance(
            &before.fingerprint.differing_parts(&item.fingerprint),
            before,
            item,
        );
        let mut reasons = Vec::new();
        if !sorted.recorded.is_empty() {
            reasons.push(ExclusionReason::RecordedPartChanged {
                parts: sorted.recorded,
            });
        }
        if !sorted.authored.is_empty() {
            reasons.push(ExclusionReason::PairingBroken {
                parts: sorted.authored,
            });
        }
        if !reasons.is_empty() {
            excluded.push(ExcludedItem {
                item: item.id.clone(),
                reasons,
            });
            continue;
        }

        compared.push(item.id.clone());
        if !sorted.derived.is_empty() {
            changes.push(change(
                item.id.clone(),
                ChangeKind::DerivedInput {
                    parts: sorted.derived,
                },
                policy,
            ));
        }
        compare_deterministic(before, item, policy, &mut changes);
        compare_judge(before, item, policy, &mut changes);
    }

    for item in &baseline.items {
        if current.item(&item.id).is_none() {
            changes.push(change(
                item.id.clone(),
                ChangeKind::Removed {
                    pass_rate: item.deterministic_pass_rate(),
                },
                policy,
            ));
        }
    }

    Comparison {
        baseline_suite: baseline.suite.clone(),
        current_suite: current.suite.clone(),
        headline: headline(baseline, current, &compared, &excluded, unverified, policy),
        excluded,
        changes,
    }
}

/// The changed parts of one item, split by what the two corpora said they were.
struct SortedParts {
    /// Declared `recorded` on at least one side: evidence that moved.
    recorded: Vec<ItemPart>,
    /// Declared `derived` on both sides: the intended effect of the change.
    derived: Vec<ItemPart>,
    /// Everything else, which is a person's fixture that no longer matches.
    authored: Vec<ItemPart>,
}

/// Splits the parts that differ by the provenance the two runs recorded.
///
/// `recorded` wins over everything, and takes the vote of **either** side: a
/// part one corpus calls testimony is testimony, and the reading that raises a
/// defect is the one worth being wrong about. `derived` needs **both** sides,
/// so adding the declaration cannot retroactively excuse a difference against a
/// baseline that never knew about it. Everything left over is authored.
fn sort_by_provenance(parts: &[ItemPart], before: &ItemReport, after: &ItemReport) -> SortedParts {
    let mut sorted = SortedParts {
        recorded: Vec::new(),
        derived: Vec::new(),
        authored: Vec::new(),
    };
    for part in parts {
        let was = before.fingerprint.provenance_of(*part);
        let now = after.fingerprint.provenance_of(*part);
        if was == PartProvenance::Recorded || now == PartProvenance::Recorded {
            sorted.recorded.push(*part);
        } else if was == PartProvenance::Derived && now == PartProvenance::Derived {
            sorted.derived.push(*part);
        } else {
            sorted.authored.push(*part);
        }
    }
    sorted
}

fn headline(
    baseline: &EvalReport,
    current: &EvalReport,
    compared: &[ItemId],
    excluded: &[ExcludedItem],
    unverified: usize,
    policy: &ComparisonPolicy,
) -> Headline {
    let paired = compared.len() + excluded.len();
    let share = ratio(excluded.len(), paired);
    if compared.is_empty() {
        return Headline::Withheld(WithheldHeadline {
            excluded_share: share,
            max_excluded_share: policy.max_excluded_share,
            items_compared: 0,
            items_excluded: excluded.len(),
            unverified_pairings: unverified,
            reason: if paired == 0 {
                "no item appeared in both runs, so there was nothing to compare".to_owned()
            } else {
                format!(
                    "all {} paired item(s) were excluded, so there was nothing left to compare",
                    excluded.len()
                )
            },
        });
    }
    if share > policy.max_excluded_share {
        return Headline::Withheld(WithheldHeadline {
            excluded_share: share,
            max_excluded_share: policy.max_excluded_share,
            items_compared: compared.len(),
            items_excluded: excluded.len(),
            unverified_pairings: unverified,
            reason: format!(
                "{:.0}% of the paired corpus was excluded, above the {:.0}% ceiling; a figure over \
                 the remaining {} of {} item(s) would read like a measurement of the suite and \
                 would not be one",
                share * 100.0,
                policy.max_excluded_share * 100.0,
                compared.len(),
                paired
            ),
        });
    }
    let before = pass_rate_over(baseline, compared);
    let after = pass_rate_over(current, compared);
    Headline::Measured(HeadlineFigures {
        baseline_pass_rate: before,
        current_pass_rate: after,
        pass_rate_delta: after - before,
        items_compared: compared.len(),
        items_excluded: excluded.len(),
        excluded_share: share,
        unverified_pairings: unverified,
    })
}

/// The deterministic pass rate of a report over a subset of its items.
fn pass_rate_over(report: &EvalReport, items: &[ItemId]) -> f64 {
    let (passed, total) = report
        .items
        .iter()
        .filter(|item| items.contains(&item.id))
        .fold((0_usize, 0_usize), |(passed, total), item| {
            (passed + item.samples_passed(), total + item.total_samples())
        });
    ratio(passed, total)
}

/// Builds a change, labelling it against the noise floor when there is one.
fn change(item: ItemId, kind: ChangeKind, policy: &ComparisonPolicy) -> Change {
    let against_noise = policy
        .noise_floor
        .as_ref()
        .map_or(NoiseVerdict::Unmeasured, |floor| match &kind {
            ChangeKind::DeterministicRegression { before, after, .. }
            | ChangeKind::DeterministicImprovement { before, after } => {
                if floor.covers_pass_rate(after - before) {
                    NoiseVerdict::WithinNoise
                } else {
                    NoiseVerdict::ExceedsNoise
                }
            }
            ChangeKind::JudgeDrift { before, after, .. } => {
                if floor.covers_judge_score(after - before) {
                    NoiseVerdict::WithinNoise
                } else {
                    NoiseVerdict::ExceedsNoise
                }
            }
            // An item that appeared, vanished or had its inputs rewritten has no
            // movement to weigh: it is not a smaller or larger difference than
            // the harness's own, it is a different kind of fact.
            _ => NoiseVerdict::Unmeasured,
        });
    Change {
        item,
        kind,
        against_noise,
    }
}

fn compare_deterministic(
    before: &ItemReport,
    after: &ItemReport,
    policy: &ComparisonPolicy,
    changes: &mut Vec<Change>,
) {
    let previous = before.deterministic_pass_rate();
    let now = after.deterministic_pass_rate();
    let delta = now - previous;
    if delta.abs() <= policy.tolerance.pass_rate_epsilon {
        return;
    }
    if delta > 0.0 {
        changes.push(change(
            after.id.clone(),
            ChangeKind::DeterministicImprovement {
                before: previous,
                after: now,
            },
            policy,
        ));
        return;
    }
    let newly_failing = newly_failing(before, after);
    let mut categories: Vec<ReliabilityCategory> = newly_failing
        .iter()
        .map(|expectation| ReliabilityCategory::of(*expectation))
        .collect();
    categories.sort_unstable();
    categories.dedup();
    changes.push(change(
        after.id.clone(),
        ChangeKind::DeterministicRegression {
            before: previous,
            after: now,
            newly_failing,
            categories,
        },
        policy,
    ));
}

fn newly_failing(before: &ItemReport, after: &ItemReport) -> Vec<ExpectationName> {
    let was: Vec<ExpectationName> = before
        .failures()
        .into_iter()
        .map(|failure| failure.expectation)
        .collect();
    let mut now: Vec<ExpectationName> = after
        .failures()
        .into_iter()
        .map(|failure| failure.expectation)
        .filter(|expectation| !was.contains(expectation))
        .collect();
    now.sort_unstable();
    now.dedup();
    now
}

fn compare_judge(
    before: &ItemReport,
    after: &ItemReport,
    policy: &ComparisonPolicy,
    changes: &mut Vec<Change>,
) {
    for current in after.judge_summaries() {
        let Some(previous) = before
            .judge_summaries()
            .into_iter()
            .find(|summary| summary.criterion == current.criterion)
        else {
            continue;
        };
        let (Some(was), Some(now)) = (previous.mean_score, current.mean_score) else {
            continue;
        };
        if (now - was).abs() > policy.tolerance.judge_score_epsilon {
            changes.push(change(
                after.id.clone(),
                ChangeKind::JudgeDrift {
                    criterion: current.criterion,
                    before: was,
                    after: now,
                },
                policy,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;
    use crate::assertions::AssertionFailure;
    use crate::config::EvalConfig;
    use crate::corpus::{ItemFingerprint, PartDigest};
    use crate::judge::{CriterionOutcome, JudgeVerdict, JudgeVote};
    use crate::report::SampleReport;

    fn sample(failing: bool, score: Option<u8>) -> SampleReport {
        SampleReport {
            sample: 1,
            failures: if failing {
                vec![AssertionFailure::new(
                    ExpectationName::Commands,
                    "[trip.set_name]",
                    "[]",
                )]
            } else {
                Vec::new()
            },
            harness_error: None,
            signature: "sig".to_owned(),
            judge: score
                .map(|score| {
                    vec![CriterionOutcome {
                        criterion: JudgeCriterion::Tone,
                        votes: vec![JudgeVote {
                            vote: 1,
                            verdict: Some(JudgeVerdict {
                                score,
                                reason: "r".to_owned(),
                            }),
                            error: None,
                        }],
                    }]
                })
                .unwrap_or_default(),
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

    /// A fingerprint whose `setup` digest is `state`, with that provenance.
    fn fingerprint(state: &str, setup: PartProvenance) -> ItemFingerprint {
        ItemFingerprint {
            parts: ItemPart::ALL
                .into_iter()
                .map(|part| PartDigest {
                    part,
                    digest: if part == ItemPart::Setup {
                        state.to_owned()
                    } else {
                        "same".to_owned()
                    },
                    provenance: if part == ItemPart::Setup {
                        setup
                    } else {
                        PartProvenance::Authored
                    },
                })
                .collect(),
        }
    }

    fn item(id: &str, sample: SampleReport, fingerprint: ItemFingerprint) -> ItemReport {
        ItemReport {
            id: ItemId::new(id),
            name: "An item".to_owned(),
            tags: Vec::new(),
            fingerprint,
            samples: vec![sample],
        }
    }

    fn report(items: Vec<ItemReport>) -> EvalReport {
        EvalReport::new(
            "suite",
            DateTime::from_timestamp(0, 0).unwrap_or_default(),
            EvalConfig::default(),
            items,
        )
    }

    fn one(sample: SampleReport) -> EvalReport {
        report(vec![item(
            "i",
            sample,
            fingerprint("state", PartProvenance::Authored),
        )])
    }

    #[test]
    fn a_broken_assertion_is_a_regression_not_a_drift() {
        let comparison = compare(
            &one(sample(false, Some(5))),
            &one(sample(true, Some(5))),
            &ComparisonPolicy::default(),
        );
        assert!(comparison.has_deterministic_regression());
        assert!(!comparison.has_judge_drift());
        let regression = comparison.deterministic_regressions()[0];
        let ChangeKind::DeterministicRegression {
            newly_failing,
            categories,
            ..
        } = &regression.kind
        else {
            panic!("expected a regression, got {:?}", regression.kind);
        };
        assert_eq!(newly_failing, &[ExpectationName::Commands]);
        assert_eq!(categories, &[ReliabilityCategory::SideEffectIntegrity]);
    }

    #[test]
    fn a_moved_score_on_unchanged_behaviour_is_a_drift_not_a_regression() {
        let comparison = compare(
            &one(sample(false, Some(5))),
            &one(sample(false, Some(3))),
            &ComparisonPolicy::default(),
        );
        assert!(!comparison.has_deterministic_regression());
        assert!(comparison.has_judge_drift());
        assert!(comparison.summary().contains("judge drift"));
    }

    #[test]
    fn a_score_inside_the_tolerance_is_not_news() {
        let comparison = compare(
            &one(sample(false, Some(5))),
            &one(sample(false, Some(5))),
            &ComparisonPolicy::default(),
        );
        assert!(comparison.is_unchanged());
    }

    #[test]
    fn the_headline_reports_the_excluded_share_even_when_it_stands() {
        let comparison = compare(
            &one(sample(false, None)),
            &one(sample(false, None)),
            &ComparisonPolicy::default(),
        );
        let figures = comparison
            .headline
            .figures()
            .expect("nothing was excluded, so the headline stands");
        assert_eq!(figures.items_compared, 1);
        assert_eq!(figures.items_excluded, 0);
        assert!((figures.excluded_share - 0.0).abs() < 1e-9);
        assert!(comparison.summary().contains("0 excluded"));
    }

    /// Two reports of one item whose `setup` changed, with that provenance on
    /// each side.
    fn pair(before: PartProvenance, after: PartProvenance) -> (EvalReport, EvalReport) {
        (
            report(vec![item(
                "i",
                sample(false, None),
                fingerprint("before", before),
            )]),
            report(vec![item(
                "i",
                sample(true, None),
                fingerprint("after", after),
            )]),
        )
    }

    #[test]
    fn an_authored_item_change_is_excluded_and_produces_no_regression() {
        // The item's setup changed and a person wrote that setup. Under the old
        // rule this would have compared two different scenarios and reported the
        // difference as a regression.
        let (baseline, current) = pair(PartProvenance::Authored, PartProvenance::Authored);
        let comparison = compare(&baseline, &current, &ComparisonPolicy::default());

        assert!(!comparison.has_deterministic_regression());
        assert!(!comparison.has_corpus_defect());
        assert_eq!(comparison.excluded.len(), 1);
        assert!(matches!(
            comparison.excluded[0].reasons.as_slice(),
            [ExclusionReason::PairingBroken { parts }] if parts == &[ItemPart::Setup]
        ));
    }

    #[test]
    fn a_declared_derived_change_stays_compared_and_is_named_as_intended() {
        let (baseline, current) = pair(PartProvenance::Derived, PartProvenance::Derived);
        let comparison = compare(&baseline, &current, &ComparisonPolicy::default());

        assert!(comparison.excluded.is_empty());
        assert_eq!(comparison.derived_input_changes().len(), 1);
        assert!(comparison.has_deterministic_regression());
        assert!(
            comparison.summary().contains("derived input changed"),
            "{}",
            comparison.summary()
        );
    }

    #[test]
    fn a_recorded_change_is_a_corpus_defect_not_a_result() {
        let (baseline, current) = pair(PartProvenance::Recorded, PartProvenance::Recorded);
        let comparison = compare(&baseline, &current, &ComparisonPolicy::default());

        assert!(comparison.has_corpus_defect());
        assert_eq!(comparison.corpus_defects().len(), 1);
        // Never folded into a figure, and never a regression.
        assert!(!comparison.has_deterministic_regression());
        assert!(comparison.derived_input_changes().is_empty());
        assert!(matches!(
            comparison.excluded[0].reasons.as_slice(),
            [ExclusionReason::RecordedPartChanged { parts }] if parts == &[ItemPart::Setup]
        ));
        assert!(
            comparison.summary().contains("CORPUS DEFECT"),
            "{}",
            comparison.summary()
        );
    }

    #[test]
    fn one_side_calling_a_part_recorded_is_enough_to_raise_a_defect() {
        // Whichever run is right, something rewrote a recording, and the reading
        // that says so is the one worth being wrong about.
        let (baseline, current) = pair(PartProvenance::Derived, PartProvenance::Recorded);
        assert!(compare(&baseline, &current, &ComparisonPolicy::default()).has_corpus_defect());
    }

    #[test]
    fn a_derived_declaration_on_one_side_only_does_not_rescue_a_pairing() {
        let (baseline, current) = pair(PartProvenance::Authored, PartProvenance::Derived);
        let comparison = compare(&baseline, &current, &ComparisonPolicy::default());
        assert_eq!(comparison.excluded.len(), 1);
        assert!(!comparison.has_corpus_defect());
    }

    #[test]
    fn an_unknown_fingerprint_pairs_by_identifier_and_is_counted() {
        let baseline = one(sample(false, None));
        let current = report(vec![item(
            "i",
            sample(true, None),
            ItemFingerprint::default(),
        )]);
        let comparison = compare(&baseline, &current, &ComparisonPolicy::default());
        assert!(comparison.has_deterministic_regression());
        let figures = comparison.headline.figures().expect("still comparable");
        assert_eq!(figures.unverified_pairings, 1);
        assert!(
            comparison.summary().contains("pairing was not checked"),
            "{}",
            comparison.summary()
        );
    }
}
