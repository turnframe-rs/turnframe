//! The settings a turn carries win over the understander's own.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_understand::{Settings, VerifyPolicy};

#[tokio::test]
async fn a_turn_runs_under_its_own_settings() {
    // [1]name [2]Lisbon
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_verify(VerifyPolicy::Off));
    let run = understand(script, &input).await;

    assert_eq!(run.understanding.acts.len(), 1);
    assert!(
        !run.was_called("u1/verify"),
        "verification off for this turn"
    );
}
