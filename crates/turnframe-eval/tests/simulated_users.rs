//! Conversations held by simulated users over the travel desk, against a real model: opt-in
//! and never in continuous integration, like the live corpus. It needs the live key and
//! `TURNFRAME_EVAL_SIMULATE`, so a live corpus run never starts one by accident.
//!
//! The variables that pick the vendor, the models, the goals and the report file are in
//! `docs/evaluation.md`. Only the goals' own fixtures leave the machine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod support;

use support::{DEFAULT_VENDOR, SampleHarness, VENDORS, load_dotenv, provider_for};
use turnframe_eval::runner::{EvalHarness, SampleIndex};
use turnframe_eval::simulate::{Goal, ModelUser, Simulation};
use turnframe_provider::secret::ApiKey;

/// The variable that allows a paid run at all.
const KEY_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_KEY";

/// The variable that asks for this run in particular.
const SIMULATE_VARIABLE: &str = "TURNFRAME_EVAL_SIMULATE";

/// Which vendor answers, for the runtime and the simulated user alike.
const VENDOR_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_VENDOR";

/// The model under test.
const MODEL_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_MODEL";

/// The model playing the user, the one under test when unset.
const SIMULATOR_VARIABLE: &str = "TURNFRAME_EVAL_SIMULATOR_MODEL";

/// Goal ids to run, comma separated.
const GOALS_VARIABLE: &str = "TURNFRAME_EVAL_SIMULATE_GOALS";

/// How many times each goal and manner is held, one when unset.
const SAMPLES_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_SAMPLES";

/// How many conversations run at once, one when unset.
const CONCURRENCY_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_CONCURRENCY";

/// Where to write the machine-readable report.
const REPORT_VARIABLE: &str = "TURNFRAME_EVAL_SIMULATE_REPORT";

/// The day the goals are held on: they are written for the autumn of 2026, and a user who
/// names a date without its year means that year.
fn goals_day() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-09-30T09:00:00Z")
        .expect("a valid instant")
        .into()
}

fn goals_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("simulated_users")
}

/// The goals run only on demand, so their files are checked on every build instead.
#[tokio::test]
async fn the_goals_load_and_every_world_prepares() {
    let goals = Goal::load_dir(goals_dir()).expect("the goals load");
    assert!(goals.len() >= 5, "{} goals", goals.len());
    let harness = SampleHarness::new(|_item, _sample, _turn| {
        support::Scripted::narrated_by(
            turnframe_test::providers::ScriptedProvider::builder("unused", "unused").build_shared(),
        )
    });
    for goal in &goals {
        if let Err(error) = harness.prepare(&goal.item(), SampleIndex(0)).await {
            panic!("{}: {error}", goal.id);
        }
    }
}

#[tokio::test]
async fn simulated_users_talk_to_a_real_model() {
    load_dotenv();
    let (Ok(key), Ok(_)) = (
        std::env::var(KEY_VARIABLE),
        std::env::var(SIMULATE_VARIABLE),
    ) else {
        println!(
            "SKIPPED simulated_users_talk_to_a_real_model: {KEY_VARIABLE} and \
             {SIMULATE_VARIABLE} are not both set, so no model was called"
        );
        return;
    };
    if key.trim().is_empty() || std::env::var("CI").is_ok() {
        println!("SKIPPED simulated_users_talk_to_a_real_model: no key, or in CI");
        return;
    }
    let vendor = std::env::var(VENDOR_VARIABLE).unwrap_or_else(|_| DEFAULT_VENDOR.to_owned());
    let Some((_, fallback_model)) = VENDORS.iter().find(|(name, _)| *name == vendor) else {
        panic!("{VENDOR_VARIABLE} is {vendor:?}, not one of the three");
    };
    let model = std::env::var(MODEL_VARIABLE).unwrap_or_else(|_| (*fallback_model).to_owned());
    let simulator = std::env::var(SIMULATOR_VARIABLE).unwrap_or_else(|_| model.clone());
    let mut goals = Goal::load_dir(goals_dir()).expect("the goals load");
    if let Ok(only) = std::env::var(GOALS_VARIABLE) {
        let wanted: Vec<&str> = only.split(',').map(str::trim).collect();
        goals.retain(|goal| wanted.contains(&goal.id.as_str()));
    }
    let count = |variable: &str| -> u32 {
        std::env::var(variable).map_or(1, |value| {
            value
                .parse()
                .unwrap_or_else(|error| panic!("{variable}: {error}"))
        })
    };
    let (samples, concurrency) = (count(SAMPLES_VARIABLE), count(CONCURRENCY_VARIABLE));
    let conversations: usize = goals.iter().map(|goal| goal.manners.len()).sum();
    println!(
        "holding {} conversation(s) with {vendor}/{model}, the user played by {simulator}",
        conversations * samples as usize
    );

    let key = ApiKey::new(key);
    let user = ModelUser::new(provider_for(&vendor, &simulator, &key).expect("a simulator"));
    let harness = SampleHarness::with_provider(move |_item, _sample, _turn| {
        provider_for(&vendor, &model, &key).expect("a provider for the live run")
    })
    .at(goals_day());
    let report = Simulation::new()
        .with_samples(samples)
        .with_concurrency(concurrency as usize)
        .run(&goals, &harness, &user)
        .await;

    println!("{}", report.summary());
    if let Ok(path) = std::env::var(REPORT_VARIABLE) {
        std::fs::write(&path, report.to_json().expect("the report serializes"))
            .expect("the report is written");
        println!("report written to {path}");
    }
    // A model may miss a goal, which the rates measure; a run that measured nothing may not.
    assert!(report.measured_count() > 0, "{}", report.summary());
}
