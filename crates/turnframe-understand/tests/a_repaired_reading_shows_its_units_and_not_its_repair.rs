//! A segmentation that needed a repair round is shown by its units: its analysis then
//! speaks of the repair, which is nothing the user said.
#![allow(clippy::expect_used)]

mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, routed, script, turn, understand, understand_profiled, words};
use turnframe_tasks::{TaskKind, TaskProfiles};
use turnframe_understand::Step;

fn analysis(run: &support::Run) -> String {
    run.steps
        .steps()
        .into_iter()
        .find_map(|step| match step {
            Step::Segmented { analysis, .. } => Some(analysis),
            _ => None,
        })
        .expect("the reading was published")
}

#[tokio::test]
async fn a_repaired_reading_shows_its_units_and_not_its_repair() {
    // [1]set [2]the [3]name [4]to [5]Lisbon
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Sets the name.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 9}, "workflow": "trip"}
            ]}),
        )
        .answer(
            "turn/segment#repair1",
            json!({"analysis": "The previous answer pointed past the message; fixed.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 5)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("set the name to Lisbon")).await;

    let analysis = analysis(&run);
    assert!(analysis.is_empty(), "{analysis}");
}

#[tokio::test]
async fn a_repaired_vote_shows_its_units_and_not_its_repair() {
    // [1]set [2]the [3]name [4]to [5]Lisbon
    let unit = json!({"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"});
    let script = script()
        .answer(
            "turn/segment#vote1",
            json!({"analysis": "Sets the name.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 9}, "workflow": "trip"}
            ]}),
        )
        .answer(
            "turn/segment#vote1#repair1",
            json!({"analysis": "The previous answer pointed past the message; fixed.", "units": [unit]}),
        )
        .answer(
            "turn/segment#vote2",
            json!({"analysis": "Sets the name.", "units": [unit]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 5)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let votes = TaskProfiles::new().adjust(TaskKind::Segment, |profile| profile.with_votes(2));
    let run = understand_profiled(script, &turn("set the name to Lisbon"), votes).await;

    let analysis = analysis(&run);
    assert!(!analysis.contains("fixed"), "{analysis}");
}
