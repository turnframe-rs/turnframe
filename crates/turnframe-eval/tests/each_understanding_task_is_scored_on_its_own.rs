//! Each understanding task is scored on its own: a reading that reached the wrong
//! operation fails route, and extract with it, and keeps what segment and locate got
//! right.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{SampleHarness, scripted, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::Understanding;
use turnframe_eval::config::EvalConfig;
use turnframe_eval::corpus::{EvalItem, Suite};
use turnframe_eval::runner::Runner;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::operations;

const TEXT: &str = "Set the name to Lisbon";

fn corpus_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

/// The item that sets the name, saying what each task must make of it.
fn item() -> EvalItem {
    let mut item = Suite::load_item(corpus_dir().join("set_name.toml")).unwrap();
    item.expect.understanding = Some(
        toml::from_str(
            r#"
            [[units]]
            kind = "request"
            words = "Set the name to Lisbon"
            operation = "trip.set_name"
            record = "trip-1"
            arguments = { value = { equals = "Lisbon" } }
            "#,
        )
        .unwrap(),
    );
    item
}

fn reading(turn: TurnId, operation: &str, value: &str) -> Understanding {
    UnderstandingBuilder::of(TEXT)
        .apply(
            operation,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({ "value": value }),
            TEXT,
        )
        .build()
        .unwrap()
}

async fn accuracy(operation: &'static str, value: &'static str) -> String {
    let suite = Suite::new("trip", vec![item()]).unwrap();
    let harness =
        SampleHarness::new(move |_item, _sample, turn| scripted(reading(turn, operation, value)));
    let report = Runner::new(EvalConfig::default())
        .run(&suite, &harness)
        .await;
    report
        .task_accuracy()
        .expect("the item says what each task must do")
}

#[tokio::test]
async fn a_right_reading_passes_every_task() {
    assert_eq!(
        accuracy(operations::SET_NAME, "Lisbon").await,
        "segment 1/1 · route 1/1 · locate 1/1 · extract 1/1"
    );
}

#[tokio::test]
async fn the_wrong_operation_fails_route_and_nothing_segment_and_locate_got_right() {
    assert_eq!(
        accuracy(operations::SET_TRAVEL_DATE, "2026-10-31").await,
        "segment 1/1 · route 0/1 · locate 1/1 · extract 0/1"
    );
}
