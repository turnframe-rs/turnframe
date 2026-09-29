//! An act most verdicts found was not asked for, repaired to the same values, is still not
//! asked for: a repair re-reads values, and a second verification is not asked to overturn it.
mod support;

use serde_json::json;
use support::{SET_NAME, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::NotUnderstoodReason;
use turnframe_understand::Settings;

#[tokio::test]
async fn an_act_not_asked_for_stays_so_when_its_repair_reads_the_same() {
    let not_requested = json!({"reason": "The user asked what the name is.",
        "arguments": {"value": "different"}, "overall": "not_requested"});
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 4)}}))
        .answer("u1/verify", not_requested.clone())
        .answer("u1/verify.doubt1", not_requested.clone())
        .answer("u1/verify.doubt2", not_requested)
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": words(3, 4)}}),
        )
        .answer(
            "u1/verify.after_repair",
            json!({"reason": "The name is given.", "arguments": {"value": "stated"},
                "overall": "confirmed"}),
        );
    let input =
        turn("what is the name").with_settings(Settings::conservative().with_doubt_votes(2));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert_eq!(
        understanding.not_understood.first().map(|n| &n.reason),
        Some(&NotUnderstoodReason::NotRequested),
        "{understanding:?}"
    );
    assert!(
        !run.was_called("u1/verify.after_repair"),
        "{:?}",
        run.called()
    );
}
