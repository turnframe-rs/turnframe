//! «name: offsite Lisbon, date 30/11/2026», cut into «name: offsite» and «Lisbon» by the
//! segmentation, both parts routed to naming the trip: each reading takes half the name, the
//! verifier finds each incomplete, and the repair reads the whole name across both parts.
//! That is one value cut in two, not another part's value: the trip gets one name.
mod support;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActAction, ArgumentValue};

#[tokio::test]
async fn a_value_cut_in_two_by_the_segmentation_is_read_whole() {
    // [1]name: [2]offsite [3]Lisbon, [4]date [5]30/11/2026
    let incomplete = json!({"reason": "The name is «offsite Lisbon».",
        "arguments": {"value": "incomplete"}, "overall": "confirmed"});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Three parts.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 3, "to": 3}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 4, "to": 5}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(SET_NAME))
        .answer("u3/route", routed(SET_DATE))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer(
            "u1/extract.at_part_end",
            json!({"arguments": {"value": words(2, 2)}}),
        )
        .answer("u2/extract", json!({"arguments": {"value": words(3, 3)}}))
        .answer(
            "u2/extract.at_part_end",
            json!({"arguments": {"value": words(3, 3)}}),
        )
        .answer(
            "u3/extract",
            json!({"arguments": {"date": {
                "kind": "date", "message": "current", "from": 5, "to": 5,
                "date": {"kind": "absolute", "day": 30, "month": 11, "year": 2026}
            }}}),
        )
        .answer("u1/verify", incomplete.clone())
        .answer("u2/verify", incomplete)
        .answer("u3/verify", confirmed(json!({"date": "stated"})))
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": words(2, 3)}}),
        )
        .answer(
            "u2/extract.after_verify",
            json!({"arguments": {"value": words(2, 3)}}),
        )
        .answer(
            "u1/verify.after_repair",
            confirmed(json!({"value": "stated"})),
        )
        .answer(
            "u2/verify.after_repair",
            confirmed(json!({"value": "stated"})),
        );
    let run = understand(script, &turn("name: offsite Lisbon, date 30/11/2026")).await;

    let names: Vec<String> = run
        .understanding
        .acts
        .iter()
        .filter(|act| matches!(&act.action, ActAction::Apply { operation } if operation.as_str() == SET_NAME))
        .filter_map(|act| match &act.arguments.get("value")?.value {
            ArgumentValue::Json(value) => value.as_str().map(ToOwned::to_owned),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        ["offsite Lisbon"],
        "{:?} {:?}",
        run.understanding,
        run.called()
    );
}
