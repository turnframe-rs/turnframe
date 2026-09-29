//! The corpus, run against a real model: the one test here that measures a model rather
//! than the harness. It is opt-in and never runs in continuous integration; with
//! `TURNFRAME_EVAL_LIVE_KEY` unset it skips with a printed note, so a green suite with no
//! credentials is never mistaken for a measurement.
//!
//! The variables that pick the vendor, the model, the report file, the items and tracing
//! are in `docs/evaluation.md`. Only the corpus's own fixtures leave the machine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod support;

use std::sync::Arc;

use support::SampleHarness;
use turnframe_eval::config::EvalConfig;
use turnframe_eval::corpus::Suite;
use turnframe_eval::runner::Runner;
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::secret::ApiKey;
use turnframe_provider_anthropic::AnthropicProvider;
use turnframe_provider_gemini::GeminiProvider;
use turnframe_provider_openai::OpenAiProvider;

/// The variable that turns this test on. Its absence is the normal case.
const KEY_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_KEY";

/// Which vendor answers. The three adapters carry the schema differently, so
/// this is part of what a report measures rather than an implementation detail.
const VENDOR_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_VENDOR";

/// Which model answers. Named rather than defaulted silently, because a report
/// that does not say which model produced it is not a measurement.
const MODEL_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_MODEL";

/// Where to write the machine-readable report, when the caller wants one.
const REPORT_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_REPORT";

/// Item ids to run, comma separated, when the caller wants only those.
const ITEMS_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_ITEMS";

/// The effort every turn of the run is forced to: `low`, `medium` (when unset) or `high`.
const EFFORT_VARIABLE: &str = "TURNFRAME_EVAL_EFFORT";

/// How many times each item runs, one when unset: what measures an item's pass rate.
const SAMPLES_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_SAMPLES";

/// How many samples may run at once, one when unset.
const CONCURRENCY_VARIABLE: &str = "TURNFRAME_EVAL_LIVE_CONCURRENCY";

/// The three adapters this can run against, with the model each falls back to.
///
/// The defaults are small, cheap and current: the point is to exercise the
/// pipeline end to end against a real model, not to benchmark a frontier one.
const VENDORS: [(&str, &str); 3] = [
    ("openai", "gpt-4o-mini"),
    ("anthropic", "claude-haiku-4-5-20251001"),
    ("gemini", "gemini-2.5-flash"),
];

/// The vendor used when the caller names none.
const DEFAULT_VENDOR: &str = "openai";

/// Builds the provider for `vendor`, or `None` when the name is not one of the
/// three.
fn provider_for(vendor: &str, model: &str, key: &ApiKey) -> Option<Arc<dyn ModelProvider>> {
    match vendor {
        "openai" => OpenAiProvider::openai()
            .api_key(key.clone())
            .model(model)
            .build()
            .ok()
            .map(|built| Arc::new(built) as Arc<dyn ModelProvider>),
        "anthropic" => AnthropicProvider::anthropic()
            .api_key(key.clone())
            .model(model)
            .build()
            .ok()
            .map(|built| Arc::new(built) as Arc<dyn ModelProvider>),
        "gemini" => GeminiProvider::gemini()
            .api_key(key.clone())
            .model(model)
            .build()
            .ok()
            .map(|built| Arc::new(built) as Arc<dyn ModelProvider>),
        _ => None,
    }
}

fn corpus_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("live_corpus")
}

/// The live corpus is only run against a model on demand, so its files are checked on every
/// build instead: an item that no longer parses or validates fails here, with no model.
#[tokio::test]
async fn the_live_corpus_loads_and_every_setup_prepares() {
    use turnframe_eval::runner::{EvalHarness, SampleIndex};
    let suite = Suite::load_dir("live", corpus_dir()).expect("the live corpus loads");
    assert!(suite.items.len() >= 40, "{} items", suite.items.len());
    let harness = SampleHarness::new(|_item, _sample, _turn| {
        support::Scripted::narrated_by(
            turnframe_test::providers::ScriptedProvider::builder("unused", "unused").build_shared(),
        )
    });
    for item in &suite.items {
        if let Err(error) = harness.prepare(item, SampleIndex(0)).await {
            panic!("{}: {error}", item.id.0);
        }
    }
}

/// Loads the repository's `.env` once per test binary, before any test reads a
/// variable or opens a connection. A variable already set in the shell wins.
fn load_dotenv() {
    static LOADED: std::sync::Once = std::sync::Once::new();
    LOADED.call_once(|| {
        let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    });
}

#[tokio::test]
async fn the_corpus_runs_against_a_real_model() {
    load_dotenv();
    let Ok(key) = std::env::var(KEY_VARIABLE) else {
        println!(
            "SKIPPED the_corpus_runs_against_a_real_model: {KEY_VARIABLE} is not set, so no model \
             was called and nothing was measured"
        );
        return;
    };
    if key.trim().is_empty() {
        println!("SKIPPED the_corpus_runs_against_a_real_model: {KEY_VARIABLE} is empty");
        return;
    }
    if std::env::var("CI").is_ok() {
        println!("SKIPPED the_corpus_runs_against_a_real_model: refuses to run in CI");
        return;
    }

    let vendor = std::env::var(VENDOR_VARIABLE).unwrap_or_else(|_| DEFAULT_VENDOR.to_owned());
    let Some((_, fallback_model)) = VENDORS.iter().find(|(name, _)| *name == vendor) else {
        panic!(
            "{VENDOR_VARIABLE} is {vendor:?}, which is not one of {:?}",
            VENDORS.map(|(name, _)| name)
        );
    };
    let model = std::env::var(MODEL_VARIABLE).unwrap_or_else(|_| (*fallback_model).to_owned());
    let effort: turnframe_core::effort::Effort = std::env::var(EFFORT_VARIABLE)
        .map_or(Ok(turnframe_core::effort::Effort::Medium), |level| {
            level.parse()
        })
        .unwrap_or_else(|error| panic!("{EFFORT_VARIABLE}: {error}"));
    let mut suite = Suite::load_dir("live", corpus_dir()).expect("the corpus loads");
    if let Ok(only) = std::env::var(ITEMS_VARIABLE) {
        let wanted: Vec<&str> = only.split(',').map(str::trim).collect();
        suite
            .items
            .retain(|item| wanted.contains(&item.id.0.as_str()));
    }
    let count = |variable: &str| -> u32 {
        std::env::var(variable).map_or(1, |value| {
            value
                .parse()
                .unwrap_or_else(|error| panic!("{variable}: {error}"))
        })
    };
    let samples = count(SAMPLES_VARIABLE);
    let concurrency = count(CONCURRENCY_VARIABLE);
    println!(
        "running {} item(s) against {vendor}/{model} at {effort} effort, {samples} sample(s) each",
        suite.items.len()
    );
    if let Some(trace) = support::run_trace().expect("the trace TURNFRAME_TRACE asks for") {
        let path = std::fs::canonicalize(trace.path()).unwrap_or_else(|_| trace.path().into());
        println!("saving traces to {}", path.display());
    }

    let key = ApiKey::new(key);
    let harness = SampleHarness::with_provider(move |_item, _sample, _turn| {
        provider_for(&vendor, &model, &key).expect("a provider for the live run")
    })
    .with_effort(effort);

    let report = Runner::new(
        EvalConfig::default()
            .with_samples_per_item(samples)
            .with_sample_concurrency(concurrency),
    )
    .run(&suite, &harness)
    .await;

    println!("{}", report.summary());

    if let Ok(path) = std::env::var(REPORT_VARIABLE) {
        let json = report.to_json().expect("the report serializes");
        std::fs::write(&path, json).expect("the report is written");
        println!("report written to {path}");
    }

    // Deliberately weak: a model may be wrong, which the pass rate measures. What
    // is not allowed is a run that measured nothing and reports zeroes unnoticed.
    assert!(
        report.total_samples() > 0,
        "the run measured nothing: {}",
        report.summary()
    );
}
