//! A value the verifier finds unstated gets one repair; still unstated, it is asked for.
mod support;

use serde_json::json;
use support::{SET_NAME, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;

fn not_stated() -> serde_json::Value {
    json!({
        "reason": "Those words name the field, they are not a name.",
        "arguments": {"value": "not_stated"},
        "overall": "confirmed"
    })
}

#[tokio::test]
async fn a_value_the_verifier_rejects_twice_is_asked_for() {
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(4, 5)}}))
        .answer("u1/verify", not_stated())
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": words(4, 5)}}),
        )
        .answer("u1/verify.after_repair", not_stated());
    let run = understand(script, &turn("I want to set name")).await;

    let act = &run.understanding.acts[0];
    assert!(
        act.arguments.is_empty(),
        "a refused value is dropped: {act:?}"
    );
    assert_eq!(
        act.status,
        ActStatus::NeedsValue {
            arguments: vec!["value".to_owned()],
            reason: None
        }
    );
    let repair = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("u1/extract.after_verify"))
        .expect("the extraction is repaired");
    let last = repair.messages.last().unwrap();
    assert!(
        format!("{last:?}").contains("Those words name the field"),
        "the repair carries the verifier's reason"
    );
}
