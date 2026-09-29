//! A verdict that finds fault is voted on again when the settings ask for it: the majority
//! of all the verdicts decides, so one misjudged reading is not read again on its word.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_understand::Settings;

fn doubted(second: serde_json::Value) -> ScriptedTasks {
    // [1]name [2]Lisbon
    script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer(
            "u1/verify",
            json!({"reason": "Extra words.", "arguments": {"value": "too_much"}, "overall": "confirmed"}),
        )
        .answer("u1/verify.doubt1", confirmed(json!({"value": "stated"})))
        .answer("u1/verify.doubt2", second)
}

#[tokio::test]
async fn a_verdict_finding_fault_is_voted_on_again() {
    let script = doubted(confirmed(json!({"value": "stated"})));
    let input = turn("name Lisbon").with_settings(Settings::conservative().with_doubt_votes(2));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    assert!(
        !run.was_called("u1/extract.after_verify"),
        "{:?}",
        run.called()
    );
}

#[tokio::test]
async fn a_fault_most_verdicts_find_stands() {
    let script = doubted(json!({"reason": "Extra words.", "arguments": {"value": "too_much"}, "overall": "confirmed"}))
        .answer("u1/extract.after_verify", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify.after_repair", confirmed(json!({"value": "stated"})));
    let input = turn("name Lisbon").with_settings(Settings::conservative().with_doubt_votes(2));
    let run = understand(script, &input).await;

    assert!(
        run.was_called("u1/extract.after_verify"),
        "{:?}",
        run.called()
    );
}

#[tokio::test]
async fn without_doubt_votes_the_first_verdict_decides() {
    let script = doubted(confirmed(json!({"value": "stated"})))
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": words(2, 2)}}),
        )
        .answer(
            "u1/verify.after_repair",
            confirmed(json!({"value": "stated"})),
        );
    let run = understand(script, &turn("name Lisbon")).await;

    assert!(!run.was_called("u1/verify.doubt1"), "{:?}", run.called());
}

#[tokio::test]
async fn the_verdict_after_a_repair_is_voted_on_again_too() {
    let fault = json!({"reason": "Extra words.", "arguments": {"value": "too_much"}, "overall": "confirmed"});
    let script = doubted(fault.clone())
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": words(2, 2)}}),
        )
        .answer("u1/verify.after_repair", fault)
        .answer(
            "u1/verify.after_repair.doubt1",
            confirmed(json!({"value": "stated"})),
        )
        .answer(
            "u1/verify.after_repair.doubt2",
            confirmed(json!({"value": "stated"})),
        );
    let input = turn("name Lisbon").with_settings(Settings::conservative().with_doubt_votes(2));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{:?} {act:?}", run.called());
}
