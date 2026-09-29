//! A control run: the same corpus, twice, against the same code.
//!
//! # Why a harness needs one
//!
//! Every other number in this crate is a difference between two runs, and a
//! difference is only news if it is bigger than the difference the harness
//! produces when nothing has changed at all. Agents are sampled from a
//! distribution; judges are too. Without a control, "the pass rate fell from
//! 1.00 to 0.75" and "the pass rate wobbles by a quarter between any two runs"
//! are indistinguishable, and a team that cannot tell them apart eventually
//! learns to ignore its own evaluation.
//!
//! So the affordance is first class. [`Runner::run_control`](crate::runner::Runner::run_control)
//! runs a suite twice against the same harness and hands back a [`ControlRun`];
//! [`ControlRun::noise_floor`] turns the two reports into a [`NoiseFloor`],
//! which is the largest movement the *unchanged* system produced against
//! itself. A [`Comparison`](crate::baseline::Comparison) built with that floor
//! then labels every change it reports as
//! [`WithinNoise`](crate::baseline::NoiseVerdict::WithinNoise) or
//! [`ExceedsNoise`](crate::baseline::NoiseVerdict::ExceedsNoise), so a reader
//! never has to guess.
//!
//! # What a control run cannot check for you
//!
//! That both passes really saw the same code and the same corpus. Running them
//! back to back in one call is the strongest guarantee a library can offer;
//! deploying a change between the two passes would produce a "noise floor" that
//! is a measurement of the change, and nothing here can detect that. The
//! [`NoiseFloor::suite`] name and the fingerprints on both reports are what a
//! reviewer checks.
//!
//! # A floor is a ceiling on credulity, not a licence
//!
//! A large noise floor is itself the finding. A corpus whose control run moves
//! by half is not a corpus that tolerates movement of half; it is a corpus
//! whose items are too flaky to measure anything, and the honest next step is
//! more samples per item rather than a wider tolerance.

use serde::{Deserialize, Serialize};

use crate::corpus::ItemId;
use crate::judge::JudgeCriterion;
use crate::report::EvalReport;

/// Two runs of one suite against the same code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRun {
    /// The first pass.
    pub first: EvalReport,
    /// The second pass, over the same corpus and the same code.
    pub second: EvalReport,
}

impl ControlRun {
    /// Records two passes as a control run.
    #[must_use]
    pub const fn new(first: EvalReport, second: EvalReport) -> Self {
        Self { first, second }
    }

    /// The largest movement the unchanged system produced against itself.
    ///
    /// Items are joined by identifier; an item that appears in only one pass is
    /// counted in [`NoiseFloor::unpaired_items`] and contributes nothing to the
    /// floor, because a movement from "absent" to "present" is not a movement.
    #[must_use]
    pub fn noise_floor(&self) -> NoiseFloor {
        let mut moved = Vec::new();
        let mut unpaired = 0_usize;
        let mut compared = 0_usize;
        for after in &self.second.items {
            let Some(before) = self.first.item(&after.id) else {
                unpaired += 1;
                continue;
            };
            compared += 1;
            let pass_rate = after.deterministic_pass_rate() - before.deterministic_pass_rate();
            let judge = judge_movement(before, after);
            let judge_score = judge.as_ref().map_or(0.0, |moved| moved.delta);
            if pass_rate == 0.0 && judge_score == 0.0 {
                continue;
            }
            moved.push(ItemNoise {
                item: after.id.clone(),
                pass_rate_delta: pass_rate,
                judge_score_delta: judge_score,
                judge_criterion: judge.map(|moved| moved.criterion),
            });
        }
        unpaired += self
            .first
            .items
            .iter()
            .filter(|item| self.second.item(&item.id).is_none())
            .count();

        NoiseFloor {
            suite: self.second.suite.clone(),
            items_compared: compared,
            unpaired_items: unpaired,
            pass_rate: moved
                .iter()
                .map(|item| item.pass_rate_delta.abs())
                .fold(0.0_f64, f64::max),
            judge_score: moved
                .iter()
                .map(|item| item.judge_score_delta.abs())
                .fold(0.0_f64, f64::max),
            suite_pass_rate: (self.second.deterministic_pass_rate()
                - self.first.deterministic_pass_rate())
            .abs(),
            moved,
        }
    }
}

/// One item's largest judge movement, and which criterion it was on.
struct JudgeMovement {
    criterion: JudgeCriterion,
    delta: f64,
}

fn judge_movement(
    before: &crate::report::ItemReport,
    after: &crate::report::ItemReport,
) -> Option<JudgeMovement> {
    let previous = before.judge_summaries();
    let mut largest: Option<JudgeMovement> = None;
    for current in after.judge_summaries() {
        let Some(was) = previous
            .iter()
            .find(|summary| summary.criterion == current.criterion)
            .and_then(|summary| summary.mean_score)
        else {
            continue;
        };
        let Some(now) = current.mean_score else {
            continue;
        };
        let delta = now - was;
        if largest
            .as_ref()
            .is_none_or(|found| delta.abs() > found.delta.abs())
        {
            largest = Some(JudgeMovement {
                criterion: current.criterion,
                delta,
            });
        }
    }
    largest
}

/// How much the unchanged system moved when it was measured twice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NoiseFloor {
    /// The suite the control run measured.
    pub suite: String,
    /// How many items appeared in both passes.
    pub items_compared: usize,
    /// How many appeared in only one, and therefore said nothing.
    pub unpaired_items: usize,
    /// The largest absolute movement in any one item's deterministic pass rate.
    /// This is the number a pass-rate difference is judged against.
    pub pass_rate: f64,
    /// The largest absolute movement in any one item's mean judge score.
    pub judge_score: f64,
    /// The movement of the suite-wide pass rate, for a reader who wants the
    /// headline rather than the worst item.
    pub suite_pass_rate: f64,
    /// Every item that moved at all, worst first is not assumed — they are in
    /// suite order, so the list reads like the corpus.
    pub moved: Vec<ItemNoise>,
}

impl NoiseFloor {
    /// A floor of exactly zero, for a system asserted to be deterministic.
    ///
    /// Use it when a corpus is scripted end to end and any movement at all is
    /// news. It is not a default: assuming a floor of zero without measuring
    /// one is the assumption this module exists to replace.
    #[must_use]
    pub fn deterministic(suite: impl Into<String>) -> Self {
        Self {
            suite: suite.into(),
            ..Self::default()
        }
    }

    /// Returns `true` when nothing moved between the two passes.
    #[must_use]
    pub fn is_flat(&self) -> bool {
        self.moved.is_empty()
    }

    /// Returns `true` when a pass-rate difference is no larger than the
    /// movement the unchanged system produced.
    #[must_use]
    pub fn covers_pass_rate(&self, delta: f64) -> bool {
        delta.abs() <= self.pass_rate
    }

    /// Returns `true` when a judge-score difference is no larger than the
    /// movement the unchanged system produced.
    #[must_use]
    pub fn covers_judge_score(&self, delta: f64) -> bool {
        delta.abs() <= self.judge_score
    }

    /// A readable rendering.
    #[must_use]
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "noise floor for {} over {} paired item(s){}: pass rate ±{:.2}, judge score ±{:.2}, \
             suite pass rate ±{:.2}",
            self.suite,
            self.items_compared,
            if self.unpaired_items == 0 {
                String::new()
            } else {
                format!(" ({} unpaired, ignored)", self.unpaired_items)
            },
            self.pass_rate,
            self.judge_score,
            self.suite_pass_rate
        );
        if self.is_flat() {
            let _ = writeln!(
                out,
                "  nothing moved: every item repeated itself exactly across the two passes"
            );
        }
        for item in &self.moved {
            let _ = writeln!(out, "  {item}");
        }
        out
    }
}

/// How much one item moved between the two passes of a control run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemNoise {
    /// Which item.
    pub item: ItemId,
    /// Its deterministic pass rate in the second pass minus the first.
    pub pass_rate_delta: f64,
    /// Its largest mean judge score movement, zero when nothing was judged.
    pub judge_score_delta: f64,
    /// The criterion the judge movement was on, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_criterion: Option<JudgeCriterion>,
}

impl std::fmt::Display for ItemNoise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: pass rate {:+.2}", self.item, self.pass_rate_delta)?;
        if let Some(criterion) = self.judge_criterion {
            write!(f, ", {criterion} {:+.2}", self.judge_score_delta)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;
    use crate::assertions::{AssertionFailure, ExpectationName};
    use crate::config::EvalConfig;
    use crate::corpus::ItemFingerprint;
    use crate::report::{ItemReport, SampleReport};

    fn sample(index: u32, failing: bool) -> SampleReport {
        SampleReport {
            sample: index,
            failures: if failing {
                vec![AssertionFailure::new(
                    ExpectationName::Commands,
                    "[a]",
                    "[]",
                )]
            } else {
                Vec::new()
            },
            harness_error: None,
            signature: format!("sig{failing}"),
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

    fn report(failing: &[bool]) -> EvalReport {
        EvalReport::new(
            "s",
            DateTime::from_timestamp(0, 0).unwrap_or_default(),
            EvalConfig::default(),
            vec![ItemReport {
                id: ItemId::new("i"),
                name: "An item".to_owned(),
                tags: Vec::new(),
                fingerprint: ItemFingerprint::default(),
                samples: failing
                    .iter()
                    .enumerate()
                    .map(|(index, failing)| sample(u32::try_from(index).unwrap_or(0) + 1, *failing))
                    .collect(),
            }],
        )
    }

    #[test]
    fn a_system_that_repeats_itself_has_a_flat_floor() {
        let control = ControlRun::new(report(&[false, false]), report(&[false, false]));
        let floor = control.noise_floor();
        assert!(floor.is_flat());
        assert!((floor.pass_rate - 0.0).abs() < 1e-9);
        assert!(floor.summary().contains("nothing moved"));
    }

    #[test]
    fn a_system_that_wobbles_reports_the_wobble_as_the_floor() {
        // 4/4 in the first pass, 3/4 in the second: the unchanged system moves
        // by a quarter on its own.
        let control = ControlRun::new(
            report(&[false, false, false, false]),
            report(&[false, false, false, true]),
        );
        let floor = control.noise_floor();
        assert_eq!(floor.items_compared, 1);
        assert!((floor.pass_rate - 0.25).abs() < 1e-9);
        assert!(floor.covers_pass_rate(-0.25));
        assert!(!floor.covers_pass_rate(-0.5));
        assert_eq!(floor.moved.len(), 1);
    }

    #[test]
    fn an_item_present_in_only_one_pass_is_not_a_movement() {
        let mut second = report(&[false]);
        second.items[0].id = ItemId::new("other");
        let floor = ControlRun::new(report(&[false]), second).noise_floor();
        assert_eq!(floor.items_compared, 0);
        assert_eq!(floor.unpaired_items, 2);
        assert!(floor.is_flat());
    }
}
