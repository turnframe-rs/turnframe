//! «The name is Lisbon for March», split after «Lisbon»: the second part's act finds no
//! value in its words, so the copied value that ends where the part begins is read again,
//! told those words. Read with them, the value takes them and the empty part asks nothing;
//! read without them, both readings stand.
mod support;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActAction, ActStatus, ArgumentValue};

fn split() -> serde_json::Value {
    json!({"analysis": "A name, then a month.", "units": [
        {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"},
        {"kind": "request", "words": {"from": 5, "to": 6}, "workflow": "trip"}
    ]})
}

async fn read(tail: serde_json::Value) -> Vec<(String, ActStatus, Option<String>)> {
    // [1]The [2]name [3]is [4]Lisbon [5]for [6]March
    let script = script()
        .answer("turn/segment", split())
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(SET_DATE))
        .answer("u1/extract", json!({"arguments": {"value": words(4, 4)}}))
        .answer(
            "u1/extract.at_part_end",
            json!({"arguments": {"value": words(4, 4)}}),
        )
        .answer(
            "u2/extract",
            json!({"arguments": {"date": {"kind": "not_given"}}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "u1/extract.after_tail",
            json!({"arguments": {"value": tail}}),
        )
        .answer(
            "u1/verify.after_tail",
            confirmed(json!({"value": "stated"})),
        );
    let run = understand(script, &turn("The name is Lisbon for March")).await;
    run.understanding
        .acts
        .iter()
        .map(|act| {
            let operation = match &act.action {
                ActAction::Apply { operation } => operation.to_string(),
                ActAction::Start { workflow } => workflow.to_string(),
            };
            let value = act.arguments.values().find_map(|given| match &given.value {
                ArgumentValue::Json(value) => value.as_str().map(ToOwned::to_owned),
                _ => None,
            });
            (operation, act.status.clone(), value)
        })
        .collect()
}

#[tokio::test]
async fn a_part_that_gives_its_act_nothing_may_be_the_tail_of_the_value_before_it() {
    let acts = read(words(4, 6)).await;
    assert_eq!(
        acts,
        [(
            SET_NAME.to_owned(),
            ActStatus::Ready,
            Some("Lisbon for March".to_owned())
        )]
    );
}

#[tokio::test]
async fn a_value_read_again_without_those_words_leaves_both_readings() {
    let acts = read(words(4, 4)).await;
    assert_eq!(acts.len(), 2, "{acts:?}");
    assert_eq!(acts[0].2.as_deref(), Some("Lisbon"));
    assert!(
        matches!(acts[1].1, ActStatus::NeedsValue { .. }),
        "{acts:?}"
    );
}
