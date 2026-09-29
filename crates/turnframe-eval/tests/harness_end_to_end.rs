//! The harness, end to end, against scripted understandings and the sample
//! trip domain of `turnframe-test`.
//!
//! Every test here runs a real [`Orchestrator`](turnframe_runtime::orchestrator::Orchestrator)
//! over the in-memory stores. None of them touches a network or a real model:
//! each turn's understanding is scripted, the narrator is a `ScriptedProvider`
//! that fails loudly on a call nobody scripted, and the judge is a
//! `StaticProvider` answering a fixed queue of verdicts. An evaluation harness
//! whose own tests needed a model would be untestable where it matters.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use support::{ConcurrencyGauge, SampleHarness, Scripted, scripted, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::{Understanding, UnitKind};
use turnframe_eval::assertions::ExpectationName;
use turnframe_eval::baseline::{
    ChangeKind, ComparisonPolicy, ExclusionReason, NoiseVerdict, compare,
};
use turnframe_eval::config::EvalConfig;
use turnframe_eval::corpus::{EvalItem, ItemId, ItemPart, PartProvenance, Suite};
use turnframe_eval::judge::{Judge, JudgeCriterion};
use turnframe_eval::report::{EvalReport, GateThresholds};
use turnframe_eval::runner::{Runner, SampleIndex};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::testing::StaticProvider;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::operations;

// ---------------------------------------------------------------------------
// Corpus fixtures
// ---------------------------------------------------------------------------

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn invalid_corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus_invalid")
}

fn set_name_item() -> EvalItem {
    Suite::load_item(corpus_dir().join("set_name.toml")).expect("the item file loads")
}

// ---------------------------------------------------------------------------
// Scripted understandings: what the agent under test makes of the turn
// ---------------------------------------------------------------------------

/// The understanding that sets the name, read out of the item's own text.
fn name_understanding(turn_id: TurnId, text: &str) -> Understanding {
    // Quoting the whole text and taking its last word lets one script serve an
    // item in any language.
    let value = text
        .rsplit(|c: char| c.is_whitespace())
        .find(|word| !word.is_empty())
        .unwrap_or("Lisbon");
    UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({ "value": value }),
            text,
        )
        .build()
        .expect("a valid understanding")
}

/// The understanding that sets the travel date instead: the same shape, the wrong command.
fn travel_date_understanding(turn_id: TurnId, text: &str) -> Understanding {
    UnderstandingBuilder::of(text)
        .apply(
            operations::SET_TRAVEL_DATE,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "2026-10-31"}),
            text,
        )
        .build()
        .expect("a valid understanding")
}

/// The understanding that asks for a change and withdraws it in the same turn.
fn correction_understanding(turn_id: TurnId, text: &str) -> Understanding {
    let mut understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .superseded_by_next()
        .chitchat("actually leave it alone")
        .build()
        .expect("a valid understanding");
    // The builder has no verb for a withdrawal, so the superseding unit is made one.
    if let Some(unit) = understanding.units.last_mut() {
        unit.kind = UnitKind::Cancel;
    }
    understanding
}

/// A provider that answers nothing at all, so any model call is a loud failure.
fn silent() -> Arc<ScriptedProvider> {
    ScriptedProvider::builder("scripted", "model-1").build_shared()
}

/// What an item's turn deserves: an understanding when there is text, and a provider
/// that answers nothing when the turn is only a click.
fn script_for(item: &EvalItem, turn_id: TurnId) -> Scripted {
    let Some(text) = item.turn.text.clone() else {
        return Scripted::narrated_by(silent());
    };
    let understanding = if text.contains("leave it alone") {
        correction_understanding(turn_id, &text)
    } else {
        name_understanding(turn_id, &text)
    };
    scripted(understanding)
}

/// A harness whose model always sets the name.
fn name_harness() -> SampleHarness {
    SampleHarness::new(|item, _sample, turn_id| script_for(item, turn_id))
}

/// A judge that returns the same score for every vote.
fn steady_judge(score: u8) -> Arc<Judge> {
    let provider = Arc::new(
        StaticProvider::new("judge", "j-1")
            .answering_json(serde_json::json!({"score": score, "reason": "steady"})),
    );
    Arc::new(Judge::new(provider))
}

// ---------------------------------------------------------------------------
// 1. A correct scenario passes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_correct_scenario_passes() {
    let suite = Suite::new("trip", vec![set_name_item()]).unwrap();
    let report = Runner::new(EvalConfig::default())
        .run(&suite, &name_harness())
        .await;

    let item = &report.items[0];
    assert!(item.samples[0].failures.is_empty(), "{}", report.summary());
    assert!((report.deterministic_pass_rate() - 1.0).abs() < 1e-9);
    assert!(!item.is_flaky());

    let gate = report.gate(&GateThresholds::default());
    assert!(gate.passed, "{:?}", gate.violations);
}

// ---------------------------------------------------------------------------
// 2. A scenario whose command is wrong fails, naming both commands.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_wrong_command_fails_naming_the_expected_and_the_actual_command() {
    let mut item = set_name_item();
    item.expect.commands = Some(vec!["trip.set_travel_date".to_owned()]);
    item.expect.events = None;
    item.expect.acts = None;
    item.expect.case_revision.clear();
    let suite = Suite::new("trip", vec![item]).unwrap();

    let report = Runner::new(EvalConfig::default())
        .run(&suite, &name_harness())
        .await;

    let failures = report.items[0].failures();
    let command_failure = failures
        .iter()
        .find(|failure| failure.expectation == ExpectationName::Commands)
        .expect("the command assertion failed");
    let message = command_failure.to_string();
    assert!(
        message.contains("trip.set_travel_date"),
        "the message must name the expected command: {message}"
    );
    assert!(
        message.contains("trip.set_name"),
        "the message must name the actual command: {message}"
    );
    assert!((report.deterministic_pass_rate() - 0.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 3. A forbidden effect that appears fails.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_forbidden_effect_that_appears_fails() {
    let mut item = set_name_item();
    item.expect.commands = None;
    item.expect.events = None;
    item.expect.acts = None;
    item.expect.case_revision.clear();
    item.expect.forbid.commands = vec!["trip.set_name".to_owned()];
    item.expect.forbid.events = vec!["trip.name_set".to_owned()];
    let suite = Suite::new("trip", vec![item]).unwrap();

    let report = Runner::new(EvalConfig::default())
        .run(&suite, &name_harness())
        .await;

    let failures = report.items[0].failures();
    assert!(
        failures
            .iter()
            .any(|failure| failure.expectation == ExpectationName::ForbiddenCommand),
        "{}",
        report.summary()
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.expectation == ExpectationName::ForbiddenEvent),
        "{}",
        report.summary()
    );
    let message = failures[0].to_string();
    assert!(message.contains("never to appear"), "{message}");

    // A forbidden effect is a side-effect integrity failure, never a language
    // score (spec §26.3).
    let reliability = report.reliability();
    assert_eq!(reliability.side_effect_integrity.failing_samples, 1);
    assert!(reliability.user_experience.is_empty());
}

// ---------------------------------------------------------------------------
// 4. Sampling reports variance when the model answers differently.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sampling_reports_variance_when_the_model_answers_differently() {
    // The harness scripts a different understanding on odd samples. Nothing else
    // in the world changes, so any variance the report shows is the model's.
    let harness = SampleHarness::new(|item, sample: SampleIndex, turn_id| {
        let text = item.turn.text.clone().unwrap_or_default();
        scripted(if sample.index().is_multiple_of(2) {
            name_understanding(turn_id, &text)
        } else {
            travel_date_understanding(turn_id, &text)
        })
    });

    let mut item = set_name_item();
    item.expect.events = None;
    item.expect.acts = None;
    item.expect.case_revision.clear();
    let suite = Suite::new("trip", vec![item]).unwrap();

    let report = Runner::new(EvalConfig::default().with_samples_per_item(4))
        .run(&suite, &harness)
        .await;

    let item = &report.items[0];
    let variance = item.variance();
    assert_eq!(variance.samples, 4);
    assert_eq!(
        variance.distinct_behaviours,
        2,
        "two scripted answers must show as two behaviours: {}",
        report.summary()
    );
    assert!((variance.pass_rate - 0.5).abs() < 1e-9);
    assert!((variance.pass_variance - 0.25).abs() < 1e-9);
    assert!(item.is_flaky(), "a flaky item is a result, not an error");
    assert_eq!(item.samples_passed(), 2);

    // Votes could never have found this: it took running the item again.
    assert_eq!(report.config.judging.votes_per_sample, 1);
}

// ---------------------------------------------------------------------------
// 5. Judge votes are aggregated by majority.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn judge_votes_are_aggregated_by_majority() {
    // Three votes about one sample: 5, 5, 2. The majority is 5, and the spread
    // of 3 is the judge disagreeing with itself — not the agent varying.
    let judge_provider = Arc::new(
        StaticProvider::new("judge", "j-1")
            .replying_once_json(serde_json::json!({"score": 5, "reason": "fluent"}))
            .replying_once_json(serde_json::json!({"score": 5, "reason": "fluent"}))
            .replying_once_json(serde_json::json!({"score": 2, "reason": "clumsy"})),
    );

    let mut item = set_name_item();
    item.judge = vec![JudgeCriterion::Tone];
    let suite = Suite::new("trip", vec![item]).unwrap();

    let report = Runner::new(EvalConfig::default().with_votes_per_sample(3))
        .with_judge(Arc::new(Judge::new(judge_provider)))
        .run(&suite, &name_harness())
        .await;

    let outcome = &report.items[0].samples[0].judge[0];
    assert_eq!(outcome.criterion, JudgeCriterion::Tone);
    assert_eq!(outcome.votes.len(), 3);
    assert_eq!(outcome.scores(), vec![5, 5, 2]);
    assert_eq!(outcome.majority_score(), Some(5));
    assert_eq!(outcome.spread(), 3);

    let summary = &report.reliability().user_experience[0];
    assert_eq!(summary.mean_score, Some(5.0));
    assert!((summary.mean_vote_spread - 3.0).abs() < 1e-9);

    // And the judge never touched the deterministic verdict.
    assert!((report.deterministic_pass_rate() - 1.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 6. A baseline comparison tells a regression from a drift.
// ---------------------------------------------------------------------------

/// Two items: one whose behaviour changes, one whose grade changes.
fn comparison_suite() -> Suite {
    let base = set_name_item();

    let mut regressing = base.clone();
    regressing.id = ItemId::new("trip.regression");
    regressing.expect.events = None;
    regressing.expect.acts = None;
    regressing.expect.case_revision.clear();
    regressing.judge = Vec::new();

    let mut drifting = base;
    drifting.id = ItemId::new("trip.drift");
    drifting.expect.events = None;
    drifting.expect.acts = None;
    drifting.expect.case_revision.clear();
    drifting.judge = vec![JudgeCriterion::Tone];

    Suite::new("trip", vec![regressing, drifting]).unwrap()
}

async fn run_comparison_side(suite: &Suite, regress: bool, judge_score: u8) -> EvalReport {
    // Only the item named `trip.regression` changes behaviour between the
    // two runs; the drifting item behaves identically and is only graded
    // differently.
    let harness = SampleHarness::new(move |item, _sample, turn_id| {
        let text = item.turn.text.clone().unwrap_or_default();
        scripted(if regress && item.id.as_str() == "trip.regression" {
            travel_date_understanding(turn_id, &text)
        } else {
            name_understanding(turn_id, &text)
        })
    });
    Runner::new(EvalConfig::default())
        .with_judge(steady_judge(judge_score))
        .run(suite, &harness)
        .await
}

#[tokio::test]
async fn a_baseline_comparison_tells_a_deterministic_regression_from_a_judge_drift() {
    let suite = comparison_suite();
    let baseline = run_comparison_side(&suite, false, 5).await;
    let current = run_comparison_side(&suite, true, 3).await;

    let comparison = compare(&baseline, &current, &ComparisonPolicy::default());

    assert!(comparison.has_deterministic_regression());
    assert!(comparison.has_judge_drift());

    let regressions = comparison.deterministic_regressions();
    assert_eq!(regressions.len(), 1);
    assert_eq!(regressions[0].item, ItemId::new("trip.regression"));
    let ChangeKind::DeterministicRegression {
        before,
        after,
        newly_failing,
        categories,
    } = &regressions[0].kind
    else {
        panic!("expected a regression, got {:?}", regressions[0].kind);
    };
    assert!((*before - 1.0).abs() < 1e-9);
    assert!((*after - 0.0).abs() < 1e-9);
    assert!(newly_failing.contains(&ExpectationName::Commands));
    assert!(categories.contains(&turnframe_eval::report::ReliabilityCategory::SideEffectIntegrity));

    let drifts = comparison.judge_drifts();
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].item, ItemId::new("trip.drift"));
    let ChangeKind::JudgeDrift {
        criterion,
        before,
        after,
    } = &drifts[0].kind
    else {
        panic!("expected a drift, got {:?}", drifts[0].kind);
    };
    assert_eq!(*criterion, JudgeCriterion::Tone);
    assert!((*before - 5.0).abs() < 1e-9);
    assert!((*after - 3.0).abs() < 1e-9);

    // The drifting item's behaviour did not change, so it produced no
    // regression; the regressing item was not judged, so it produced no drift.
    let summary = comparison.summary();
    assert!(summary.contains("DETERMINISTIC REGRESSION"), "{summary}");
    assert!(summary.contains("judge drift"), "{summary}");
}

// ---------------------------------------------------------------------------
// A long ledger is paged to the end, not cut at a private cap.
// ---------------------------------------------------------------------------

/// More events than the cap the observation used to read a case under.
///
/// The number is the point of the test: at 512 the read stopped, and everything
/// this turn committed sat beyond it.
const HISTORY_BEYOND_THE_OLD_CAP: usize = 640;

#[tokio::test]
async fn a_history_longer_than_the_old_cap_still_shows_the_turns_own_events() {
    // The case already carries 640 events from turns nobody is measuring, and
    // the turn under test appends its own after all of them.
    let harness = SampleHarness::new(|item, _sample, turn_id| script_for(item, turn_id))
        .with_prior_events(HISTORY_BEYOND_THE_OLD_CAP);

    let suite = Suite::new("trip", vec![set_name_item()]).unwrap();
    let report = Runner::new(EvalConfig::default())
        .run(&suite, &harness)
        .await;

    // The item asserts `events = ["trip.name_set"]`. Reading the case
    // under a cap of 512 would have handed the observation 512 rows of history
    // and none of this turn's, so the list would have been empty.
    assert!(
        (report.deterministic_pass_rate() - 1.0).abs() < 1e-9,
        "{}",
        report.summary()
    );

    // And the history itself is still not the turn's: only what this turn
    // committed is observed, however much came before it.
    let signature = &report.items[0].samples[0].signature;
    assert!(
        signature.contains("events=trip.name_set\n"),
        "the turn's own event, and only it: {signature}"
    );
    assert!(
        !signature.contains("trip.note_added"),
        "the seeded history is not this turn's ledger: {signature}"
    );
}

#[tokio::test]
async fn a_forbidden_event_hiding_beyond_the_old_cap_now_fails_the_item() {
    // This is the failure mode that mattered: under a silent cap the safety row
    // went green because the event it forbids was never read.
    let mut item = set_name_item();
    item.expect.commands = None;
    item.expect.events = None;
    item.expect.acts = None;
    item.expect.case_revision.clear();
    item.expect.forbid.events = vec!["trip.name_set".to_owned()];
    let suite = Suite::new("trip", vec![item]).unwrap();

    let harness = SampleHarness::new(|item, _sample, turn_id| script_for(item, turn_id))
        .with_prior_events(HISTORY_BEYOND_THE_OLD_CAP);
    let report = Runner::new(EvalConfig::default())
        .run(&suite, &harness)
        .await;

    let failures = report.items[0].failures();
    assert!(
        failures
            .iter()
            .any(|failure| failure.expectation == ExpectationName::ForbiddenEvent),
        "a forbidden event past the old cap must still be seen:\n{}",
        report.summary()
    );
}

#[tokio::test]
async fn a_bound_that_bites_fails_the_item_instead_of_passing_it() {
    // A bound is allowed, and it is never silent: an item measured against half
    // a ledger fails, and says which half it read.
    let mut item = set_name_item();
    item.expect.commands = None;
    item.expect.events = None;
    item.expect.acts = None;
    item.expect.case_revision.clear();
    item.expect.forbid.events = vec!["trip.name_set".to_owned()];
    let suite = Suite::new("trip", vec![item]).unwrap();

    let report = Runner::new(EvalConfig::default().with_max_observed_events(0))
        .run(&suite, &name_harness())
        .await;

    let failures = report.items[0].failures();
    let truncation = failures
        .iter()
        .find(|failure| failure.expectation == ExpectationName::TruncatedLedger)
        .expect("a truncated ledger is reported, not absorbed");
    assert!(
        truncation.to_string().contains("max_observed_events"),
        "{truncation}"
    );
    // Reading nothing would otherwise have made the forbidden event look absent.
    assert!(
        !failures
            .iter()
            .any(|failure| failure.expectation == ExpectationName::ForbiddenEvent),
        "the forbidden event really was invisible: {}",
        report.summary()
    );
    assert!((report.deterministic_pass_rate() - 0.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Bounded parallelism across the samples of one item.
// ---------------------------------------------------------------------------

/// How many samples the concurrency tests run.
const SAMPLES: u32 = 8;

/// How many of them a parallel run is allowed to have in flight.
const IN_FLIGHT: u32 = 4;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raising_the_concurrency_changes_nothing_about_the_results() {
    // The same suite, twice: once strictly serial, once four samples at a time.
    // A concurrency setting that changed a verdict would not be a scheduling
    // knob, it would be a different measurement.
    let suite = Suite::load_dir("trip", corpus_dir()).expect("the corpus loads");
    let config = EvalConfig::default().with_samples_per_item(SAMPLES);

    let serial = Runner::new(config.clone())
        .run(&suite, &name_harness())
        .await;
    let parallel = Runner::new(config.with_sample_concurrency(IN_FLIGHT))
        .run(&suite, &name_harness())
        .await;

    assert_eq!(
        serial.items, parallel.items,
        "same items, same samples, same failures, same order"
    );
    assert!((serial.deterministic_pass_rate() - 1.0).abs() < 1e-9);

    // Said as a set too, since the promise is about the set of results and only
    // incidentally about their order.
    let behaviours = |report: &EvalReport| {
        let mut seen: Vec<String> = report
            .items
            .iter()
            .flat_map(|item| item.samples.iter().map(|sample| sample.signature.clone()))
            .collect();
        seen.sort();
        seen
    };
    assert_eq!(behaviours(&serial), behaviours(&parallel));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_concurrency_setting_is_the_number_of_samples_actually_in_flight() {
    let item = set_name_item();

    // One at a time is the default, and it really is one at a time.
    let gauge = Arc::new(ConcurrencyGauge::new());
    let serial = SampleHarness::new(|item, _sample, turn_id| script_for(item, turn_id))
        .watched_by(Arc::clone(&gauge));
    let suite = Suite::new("trip", vec![item.clone()]).unwrap();
    let report = Runner::new(EvalConfig::default().with_samples_per_item(SAMPLES))
        .run(&suite, &serial)
        .await;
    assert_eq!(report.items[0].samples.len(), SAMPLES as usize);
    assert_eq!(gauge.peak(), 1, "the default must stay strictly serial");

    // Four at a time is four at a time: neither one, nor eight.
    let gauge = Arc::new(ConcurrencyGauge::new());
    let parallel = SampleHarness::new(|item, _sample, turn_id| script_for(item, turn_id))
        .watched_by(Arc::clone(&gauge));
    let report = Runner::new(
        EvalConfig::default()
            .with_samples_per_item(SAMPLES)
            .with_sample_concurrency(IN_FLIGHT),
    )
    .run(&suite, &parallel)
    .await;

    assert_eq!(report.items[0].samples.len(), SAMPLES as usize);
    assert!(
        gauge.peak() > 1,
        "a raised setting must actually overlap samples, saw {}",
        gauge.peak()
    );
    assert!(
        gauge.peak() <= IN_FLIGHT as usize,
        "the bound is a bound, saw {}",
        gauge.peak()
    );
    // And the samples still come back in index order.
    let order: Vec<u32> = report.items[0]
        .samples
        .iter()
        .map(|sample| sample.sample)
        .collect();
    assert_eq!(order, (1..=SAMPLES).collect::<Vec<_>>());
}

// ---------------------------------------------------------------------------
// The corpus loader
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_whole_corpus_directory_runs() {
    let suite = Suite::load_dir("trip", corpus_dir()).expect("the corpus loads");
    assert_eq!(suite.items.len(), 4);

    let report = Runner::new(EvalConfig::default())
        .run(&suite, &name_harness())
        .await;

    assert_eq!(report.items.len(), 4);
    assert!(
        (report.deterministic_pass_rate() - 1.0).abs() < 1e-9,
        "{}",
        report.summary()
    );
    // The machine-readable form survives a round trip, which is what a
    // continuous integration gate archives and a baseline reads back.
    let json = report.to_json().unwrap();
    assert_eq!(EvalReport::from_json(&json).unwrap(), report);
}

#[test]
fn the_loader_refuses_an_item_it_does_not_fully_understand() {
    let error = Suite::load_item(invalid_corpus_dir().join("unknown_field.toml"))
        .expect_err("a misspelled key must be an error");
    let message = error.to_string();
    assert!(message.contains("forbbiden"), "{message}");
}

#[test]
fn a_tag_selects_a_subset_of_the_suite() {
    let suite = Suite::load_dir("trip", corpus_dir()).expect("the corpus loads");
    let config = EvalConfig::default().including_tag("safety");
    let selected = suite.select(&config.selection);
    assert_eq!(selected.len(), 1);
    assert_eq!(
        selected[0].id,
        ItemId::new("trip.correction_leaves_no_trace")
    );
}

#[tokio::test]
async fn a_harness_that_cannot_prepare_a_sample_is_unmeasured_not_failing() {
    // A workflow the harness never registered: the sample records that nothing
    // was measured rather than pretending the agent got it wrong.
    let mut item = set_name_item();
    item.setup.cases[0].workflow = "traveler".into();
    let suite = Suite::new("trip", vec![item]).unwrap();

    let report = Runner::new(EvalConfig::default())
        .run(&suite, &name_harness())
        .await;

    let sample = &report.items[0].samples[0];
    assert!(sample.failures.is_empty());
    assert!(sample.harness_error.is_some(), "{sample:?}");
    assert!(!sample.passed());
    assert!(sample.abandoned);
}

#[tokio::test]
async fn the_scripted_provider_was_the_only_model_involved() {
    // The provider fails loudly on a call nobody scripted, so a green run is
    // itself the assertion that no other model was reached — and there is no
    // network in any of this.
    let script = {
        let item = set_name_item();
        let text = item.turn.text.clone().unwrap_or_default();
        scripted(name_understanding(
            support::turn_id_for(SampleIndex(0)),
            &text,
        ))
    };
    let (provider, understander) = (
        Arc::clone(&script.provider),
        Arc::clone(&script.understander),
    );
    let harness = SampleHarness::new(move |_item, _sample, _turn_id| script.clone());

    let suite = Suite::new("trip", vec![set_name_item()]).unwrap();
    let report = Runner::new(EvalConfig::default())
        .run(&suite, &harness)
        .await;

    assert!(
        (report.deterministic_pass_rate() - 1.0).abs() < 1e-9,
        "{}",
        report.summary()
    );
    provider.verify().expect("the script was followed exactly");
    assert_eq!(
        understander.seen().len(),
        1,
        "the turn was understood exactly once"
    );
    assert_eq!(
        understander.remaining(),
        0,
        "and by the understanding scripted for it"
    );
    assert!(
        provider.calls().iter().all(|call| matches!(
            call.purpose(),
            ModelPurpose::Acknowledge | ModelPurpose::Review
        )),
        "the provider only narrated: {:?}",
        provider
            .calls()
            .iter()
            .map(|call| call.purpose())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn an_item_can_answer_a_card_without_naming_its_identifier() {
    // The item names the case; the runner finds its blocking card and reads the
    // revision off the card itself. Nothing is scripted to understand, so a turn
    // that tried to understand a click would be unreadable instead of sent.
    let item =
        Suite::load_item(corpus_dir().join("confirm_rebooking.toml")).expect("the item loads");
    let suite = Suite::new("trip", vec![item]).unwrap();
    let script = Scripted::narrated_by(
        ScriptedProvider::builder("scripted", "model-1")
            .acknowledging("Sent.")
            .build_shared(),
    );
    let (provider, understander) = (
        Arc::clone(&script.provider),
        Arc::clone(&script.understander),
    );
    let harness = SampleHarness::new(move |_item, _sample, _turn_id| script.clone());

    let report = Runner::new(EvalConfig::default())
        .run(&suite, &harness)
        .await;

    assert!(
        (report.deterministic_pass_rate() - 1.0).abs() < 1e-9,
        "{}",
        report.summary()
    );
    assert!(
        understander.seen().is_empty(),
        "a click costs no understanding: its meaning is the stored option"
    );
    provider.verify().expect("the script was followed exactly");
}

// ---------------------------------------------------------------------------
// Exclusion is as loud as the score, and past a ceiling there is no score.
// ---------------------------------------------------------------------------

/// One copy of the name item, under a new identifier and a new case label.
///
/// The label lives in `setup`, so changing it changes the item's fingerprint
/// without changing a single thing the turn does. That is exactly the shape of
/// the failure this guards against: an item that is textually different and
/// behaviourally identical, joined by identifier as though nothing happened.
fn labelled(id: &str, label: &str) -> EvalItem {
    let mut item = set_name_item();
    item.id = ItemId::new(id);
    item.setup.cases[0].label = label.to_owned();
    item
}

fn labelled_suite(labels: [&str; 3]) -> Suite {
    Suite::new(
        "trip",
        vec![
            labelled("trip.one", labels[0]),
            labelled("trip.two", labels[1]),
            labelled("trip.three", labels[2]),
        ],
    )
    .expect("three distinct items")
}

#[tokio::test]
async fn two_thirds_of_a_corpus_excluded_declines_to_summarize() {
    // The corpus is edited between the two runs: two items of three carry a
    // different seeded world, and nothing declares that world derived. Both
    // runs pass every assertion, so the naive comparison here is "no change,
    // 100%" over the one item that survived the join — a figure that reads like
    // a measurement of the suite and is a measurement of a third of it.
    let baseline = Runner::new(EvalConfig::default())
        .run(&labelled_suite(["A", "B", "C"]), &name_harness())
        .await;
    let current = Runner::new(EvalConfig::default())
        .run(
            &labelled_suite(["A", "B edited", "C edited"]),
            &name_harness(),
        )
        .await;

    let comparison = compare(&baseline, &current, &ComparisonPolicy::default());

    // There is no headline number to read, and no accessor that would give one.
    assert!(comparison.is_withheld());
    assert!(comparison.headline.figures().is_none());
    let withheld = comparison
        .headline
        .withheld()
        .expect("the headline was withheld");
    assert_eq!(withheld.items_compared, 1);
    assert_eq!(withheld.items_excluded, 2);
    assert!((withheld.excluded_share - 2.0 / 3.0).abs() < 1e-9);
    assert!(
        withheld.reason.contains("67%") && withheld.reason.contains("ceiling"),
        "the refusal must say why: {}",
        withheld.reason
    );

    // The readable summary leads with the exclusion, not with a figure.
    let summary = comparison.summary();
    let first_line = summary.lines().next().unwrap_or_default();
    assert!(
        first_line.contains("1 of 3 paired item(s) compared, 2 excluded (67%"),
        "{summary}"
    );
    assert!(summary.contains("NO HEADLINE FIGURE"), "{summary}");

    // And so does the machine-readable form: the headline and the exclusions are
    // both ahead of the changes a reader would otherwise scroll to.
    let json = serde_json::to_string_pretty(&comparison).expect("a comparison serializes");
    let headline_at = json.find("\"headline\"").expect("headline is present");
    let excluded_at = json.find("\"excluded\"").expect("exclusions are present");
    let changes_at = json.find("\"changes\"").expect("changes are present");
    assert!(
        headline_at < changes_at && excluded_at < changes_at,
        "{json}"
    );
    assert!(json.contains("\"excluded_share\""), "{json}");
    assert!(json.contains("\"withheld\""), "{json}");
    assert!(
        !json.contains("pass_rate_delta"),
        "no figure survives: {json}"
    );
}

#[tokio::test]
async fn the_refusal_is_the_threshold_and_nothing_else() {
    // The falsification of the test above: raise the ceiling past the observed
    // share and the very same comparison produces its figures. What withheld
    // them was the policy, not an unrelated accident of the data.
    let baseline = Runner::new(EvalConfig::default())
        .run(&labelled_suite(["A", "B", "C"]), &name_harness())
        .await;
    let current = Runner::new(EvalConfig::default())
        .run(
            &labelled_suite(["A", "B edited", "C edited"]),
            &name_harness(),
        )
        .await;

    let permissive = ComparisonPolicy::default().with_max_excluded_share(1.0);
    let comparison = compare(&baseline, &current, &permissive);

    let figures = comparison
        .headline
        .figures()
        .expect("the ceiling was raised above the observed share");
    assert_eq!(figures.items_compared, 1);
    assert_eq!(figures.items_excluded, 2);
    // Even when it stands, the headline still carries the share beside the rate.
    assert!((figures.excluded_share - 2.0 / 3.0).abs() < 1e-9);
    assert!((figures.pass_rate_delta - 0.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Three kinds of item change, and three names for them.
// ---------------------------------------------------------------------------

/// `provenance` cannot be written by editing a loaded item without also editing
/// the file, so these suites are built from source text: the declaration is
/// exercised through the loader, which is where a corpus really writes it.
fn declared_item(id: &str, label: &str, provenance: &str) -> EvalItem {
    let mut item = set_name_item();
    item.id = ItemId::new(id);
    item.setup.cases[0].label = label.to_owned();
    item.provenance = toml::from_str(provenance).expect("a provenance table");
    item.validate()
        .expect("the declaration applies to this item");
    item
}

fn provenance_suite(label: &str) -> Suite {
    Suite::new(
        "trip",
        vec![
            declared_item("trip.derived", label, "setup = \"derived\""),
            declared_item("trip.authored", label, ""),
            labelled("trip.untouched", "steady"),
        ],
    )
    .expect("three distinct items")
}

#[tokio::test]
async fn a_projection_change_and_an_unrelated_edit_get_different_names() {
    let baseline = Runner::new(EvalConfig::default())
        .run(&provenance_suite("before"), &name_harness())
        .await;
    let current = Runner::new(EvalConfig::default())
        .run(&provenance_suite("after"), &name_harness())
        .await;

    let policy = ComparisonPolicy::default().with_max_excluded_share(0.5);
    let comparison = compare(&baseline, &current, &policy);

    // The declared item changed because the projection changed. That is the
    // intended effect of the change being measured, so the item stays compared.
    let derived = comparison.derived_input_changes();
    assert_eq!(derived.len(), 1, "{}", comparison.summary());
    assert_eq!(derived[0].item, ItemId::new("trip.derived"));
    let ChangeKind::DerivedInput { parts } = &derived[0].kind else {
        panic!("expected a derived input change, got {:?}", derived[0].kind);
    };
    assert_eq!(parts, &[ItemPart::Setup]);

    // The undeclared item changed for a reason nothing accounts for, so the
    // pairing is gone and nothing about it is compared.
    assert_eq!(comparison.excluded.len(), 1);
    assert_eq!(comparison.excluded[0].item, ItemId::new("trip.authored"));
    assert!(matches!(
        comparison.excluded[0].reasons.as_slice(),
        [ExclusionReason::PairingBroken { parts }] if parts == &[ItemPart::Setup]
    ));
    assert!(!comparison.excluded[0].is_corpus_defect());

    // Neither is a regression: the agent did the same thing in both runs.
    assert!(!comparison.has_deterministic_regression());

    let summary = comparison.summary();
    assert!(
        summary.contains("derived input changed as intended"),
        "{summary}"
    );
    assert!(summary.contains("pairing broken"), "{summary}");
}

#[tokio::test]
async fn a_changed_recorded_part_is_a_corpus_defect_not_a_result() {
    // A part lifted from a production trace is testimony. It does not change on
    // its own, so a comparison that sees it change has found a defect in the
    // corpus or in whatever regenerated it — and must say so instead of folding
    // it into a score.
    let suite_of = |label: &str| {
        Suite::new(
            "trip",
            vec![
                declared_item("trip.recorded", label, "setup = \"recorded\""),
                labelled("trip.untouched", "steady"),
            ],
        )
        .expect("two distinct items")
    };

    let baseline = Runner::new(EvalConfig::default())
        .run(&suite_of("captured on 2026-06-01"), &name_harness())
        .await;
    let current = Runner::new(EvalConfig::default())
        .run(&suite_of("regenerated by tooling"), &name_harness())
        .await;

    let policy = ComparisonPolicy::default().with_max_excluded_share(0.5);
    let comparison = compare(&baseline, &current, &policy);

    assert!(comparison.has_corpus_defect());
    let defects = comparison.corpus_defects();
    assert_eq!(defects.len(), 1);
    assert_eq!(defects[0].item, ItemId::new("trip.recorded"));
    assert!(matches!(
        defects[0].reasons.as_slice(),
        [ExclusionReason::RecordedPartChanged { parts }] if parts == &[ItemPart::Setup]
    ));

    // It is not a regression, not an intended projection change, and not part of
    // any figure: the item is excluded and the defect is named on its own terms.
    assert!(!comparison.has_deterministic_regression());
    assert!(comparison.derived_input_changes().is_empty());
    assert!(
        comparison
            .headline
            .figures()
            .is_some_and(|figures| figures.items_compared == 1),
        "{}",
        comparison.summary()
    );

    let summary = comparison.summary();
    assert!(summary.contains("CORPUS DEFECT"), "{summary}");
    assert!(
        summary.contains("not a result about the model"),
        "the reader must be told what it is not: {summary}"
    );
}

#[test]
fn a_directory_declares_provenance_once_for_every_item_in_it() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus_provenance");
    let suite = Suite::load_dir("provenance", &dir).expect("the corpus loads");

    // `suite.toml` is the manifest, not a fourth item.
    assert_eq!(suite.items.len(), 2);
    for item in &suite.items {
        assert_eq!(
            item.provenance.setup,
            PartProvenance::Derived,
            "{} took the directory's declaration",
            item.id
        );
    }
    // And an item's own word beats the directory's.
    let own = suite
        .get(&ItemId::new("provenance.recorded"))
        .expect("the item is in the suite");
    assert_eq!(own.provenance.expect, PartProvenance::Recorded);
}

#[test]
fn a_directory_manifest_is_refused_when_it_names_something_unknown() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus_invalid_manifest");
    let error = Suite::load_dir("bad", &dir).expect_err("an unknown provenance must be an error");
    assert!(error.to_string().contains("transcribed"), "{error}");
}

// ---------------------------------------------------------------------------
// A control run makes the noise floor a number.
// ---------------------------------------------------------------------------

/// How many samples each pass of the noise tests runs.
const NOISE_SAMPLES: u32 = 4;

/// Whether each successive call to the harness gets the right understanding or
/// the wrong one, in call order, wrapping.
///
/// Four passes of four samples: 4/4, then 3/4, then 4/4, then 3/4. The first
/// two are the control run and give a noise floor of a quarter; the last two are
/// the before and after, and differ by exactly that quarter.
const VARYING: [bool; 16] = [
    true, true, true, true, // control, first pass
    true, true, true, false, // control, second pass
    true, true, true, true, // baseline
    true, true, true, false, // current
];

/// A harness whose scripted model varies from call to call rather than from
/// sample index, so two passes over the same corpus need not agree.
fn varying_harness() -> SampleHarness {
    let call = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    SampleHarness::new(move |item, _sample, turn_id| {
        let index = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let text = item.turn.text.clone().unwrap_or_default();
        scripted(if VARYING[index % VARYING.len()] {
            name_understanding(turn_id, &text)
        } else {
            travel_date_understanding(turn_id, &text)
        })
    })
}

/// The name item with only its command asserted, so a wrong understanding is a
/// plain failing sample rather than four of them.
fn noise_suite() -> Suite {
    let mut item = set_name_item();
    item.expect.events = None;
    item.expect.acts = None;
    item.expect.case_revision.clear();
    item.expect.blocks = None;
    item.expect.turn_phase = None;
    item.judge = Vec::new();
    Suite::new("trip", vec![item]).expect("one item")
}

#[tokio::test]
async fn a_control_run_over_a_repeatable_harness_measures_a_flat_floor() {
    // Nothing varies, so the floor is zero — and it is zero because it was
    // measured, which is the whole difference from assuming it.
    let control = Runner::new(EvalConfig::default().with_samples_per_item(NOISE_SAMPLES))
        .run_control(&noise_suite(), &name_harness())
        .await;

    let floor = control.noise_floor();
    assert_eq!(floor.items_compared, 1);
    assert_eq!(floor.unpaired_items, 0);
    assert!(floor.is_flat(), "{}", floor.summary());
    assert!((floor.pass_rate - 0.0).abs() < 1e-9);
    assert!(
        floor.summary().contains("nothing moved"),
        "{}",
        floor.summary()
    );
}

#[tokio::test]
async fn a_difference_inside_the_noise_floor_is_reported_as_noise_not_a_regression() {
    let runner = Runner::new(EvalConfig::default().with_samples_per_item(NOISE_SAMPLES));
    let harness = varying_harness();
    let suite = noise_suite();

    // Two passes against the same code, back to back: the floor is what the
    // unchanged system did to itself.
    let control = runner.run_control(&suite, &harness).await;
    let floor = control.noise_floor();
    assert!(
        (floor.pass_rate - 0.25).abs() < 1e-9,
        "the scripted provider must wobble by a quarter: {}",
        floor.summary()
    );
    assert!(!floor.is_flat());

    // And now a before and after that differ by exactly that quarter.
    let baseline = runner.run(&suite, &harness).await;
    let current = runner.run(&suite, &harness).await;
    assert!((baseline.deterministic_pass_rate() - 1.0).abs() < 1e-9);
    assert!((current.deterministic_pass_rate() - 0.75).abs() < 1e-9);

    let comparison = compare(
        &baseline,
        &current,
        &ComparisonPolicy::default().with_noise_floor(floor),
    );

    // The movement is real and is reported. What it is *not* is a signal.
    assert!(comparison.has_deterministic_regression());
    assert!(
        !comparison.has_signal_regression(),
        "a quarter is exactly what this harness does to itself: {}",
        comparison.summary()
    );
    assert!(comparison.signal_regressions().is_empty());
    assert_eq!(
        comparison.deterministic_regressions()[0].against_noise,
        NoiseVerdict::WithinNoise
    );
    assert!(
        comparison
            .summary()
            .contains("within the measured noise floor"),
        "{}",
        comparison.summary()
    );
}

#[tokio::test]
async fn without_a_control_run_the_same_difference_is_not_known_to_be_noise() {
    // The falsification of the test above. The data is identical; only the
    // control run is missing. An unmeasured movement is not excused, because
    // treating the unknown as harmless is how a regression ships.
    let runner = Runner::new(EvalConfig::default().with_samples_per_item(NOISE_SAMPLES));
    let harness = varying_harness();
    let suite = noise_suite();

    let _control = runner.run_control(&suite, &harness).await;
    let baseline = runner.run(&suite, &harness).await;
    let current = runner.run(&suite, &harness).await;

    let comparison = compare(&baseline, &current, &ComparisonPolicy::default());

    assert!(comparison.has_signal_regression());
    assert_eq!(
        comparison.deterministic_regressions()[0].against_noise,
        NoiseVerdict::Unmeasured
    );
    assert!(
        !comparison.summary().contains("noise floor]"),
        "{}",
        comparison.summary()
    );
}

#[tokio::test]
async fn a_movement_larger_than_the_floor_is_still_a_signal() {
    // The other half of the claim: a floor is not a licence. The control run
    // wobbles by a quarter and the change costs the whole item, so the
    // difference exceeds what the harness does to itself.
    let floor = Runner::new(EvalConfig::default().with_samples_per_item(NOISE_SAMPLES))
        .run_control(&noise_suite(), &varying_harness())
        .await
        .noise_floor();
    assert!((floor.pass_rate - 0.25).abs() < 1e-9);

    let suite = noise_suite();
    let runner = Runner::new(EvalConfig::default());
    let baseline = runner.run(&suite, &name_harness()).await;
    let current = runner
        .run(
            &suite,
            &SampleHarness::new(|item, _sample, turn_id| {
                let text = item.turn.text.clone().unwrap_or_default();
                scripted(travel_date_understanding(turn_id, &text))
            }),
        )
        .await;

    let comparison = compare(
        &baseline,
        &current,
        &ComparisonPolicy::default().with_noise_floor(floor),
    );
    assert!(comparison.has_signal_regression());
    assert_eq!(
        comparison.deterministic_regressions()[0].against_noise,
        NoiseVerdict::ExceedsNoise
    );
    assert!(
        comparison
            .summary()
            .contains("exceeds the measured noise floor"),
        "{}",
        comparison.summary()
    );
}

// ---------------------------------------------------------------------------
// The turn's own words, kept only when the run asked for them.
// ---------------------------------------------------------------------------

/// Curating a corpus means reading what the assistant said.
///
/// Assertions read storage and never need the prose, so it is off by default: the
/// reply can carry whatever a person typed, and a report gets attached to things.
/// But what the right reply would have been cannot be judged from a signature:
/// two runs with identical effects can differ in whether the needed question was
/// asked. Both halves are asserted: a flag always on, or never on, is no flag.
#[tokio::test]
async fn the_reply_is_kept_only_when_the_run_asked_for_it() {
    let suite = Suite::new("trip", vec![set_name_item()]).unwrap();

    let silent = Runner::new(EvalConfig::default())
        .run(&suite, &name_harness())
        .await;
    assert!(
        silent.items[0].samples[0].answer.is_empty(),
        "by default a report carries no model prose at all"
    );

    let curating = Runner::new(EvalConfig::default().recording_answers())
        .run(&suite, &name_harness())
        .await;
    assert!(
        !curating.items[0].samples[0].answer.is_empty(),
        "and with the flag on it carries the words a curator has to read: {}",
        curating.summary()
    );
}
