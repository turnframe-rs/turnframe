//! A part asking for one operation twice, whose two readings both took the same words, is
//! read again on both sides, each reminded which occurrence it is: nothing tells which of
//! the two took the other's words, and the same words read twice would leave one.
mod support;

use serde_json::json;
use support::{ADD_EXTRA, confirmed, one_request, script, turn, understand, words};

fn extra(description: (usize, usize), amount: (usize, usize), euros: &str) -> serde_json::Value {
    json!({"arguments": {
        "description": words(description.0, description.1),
        "amount": {"kind": "money", "message": "current", "from": amount.0, "to": amount.1,
                   "amount": euros, "currency": "EUR"}
    }})
}

#[tokio::test]
async fn two_readings_of_one_operation_on_the_same_words_are_each_read_again() {
    // [1]add [2]the [3]hotel [4]at [5]80 [6]euro [7]and [8]a [9]lounge [10]pass [11]at [12]15 [13]euro
    let stated = confirmed(json!({"description": "stated", "amount": "stated"}));
    let script = script()
        .answer("turn/segment", one_request(1, 13))
        .answer("u1/route", json!({"operations": [ADD_EXTRA, ADD_EXTRA]}))
        .answer("u1/extract", extra((9, 10), (12, 13), "15"))
        .answer("u1.a2/extract", extra((8, 10), (12, 13), "15"))
        .answer("u1/verify", stated.clone())
        .answer("u1.a2/verify", stated.clone())
        .answer("u1/extract.after_overlap", extra((3, 3), (5, 6), "80"))
        .answer(
            "u1.a2/extract.after_overlap",
            extra((9, 10), (12, 13), "15"),
        )
        .answer("u1/verify.after_overlap", stated.clone())
        .answer("u1.a2/verify.after_overlap", stated);
    let run = understand(
        script,
        &turn("add the hotel at 80 euro and a lounge pass at 15 euro"),
    )
    .await;

    let descriptions: Vec<String> = run
        .understanding
        .acts
        .iter()
        .filter_map(|act| match &act.arguments.get("description")?.value {
            turnframe_core::understanding::ArgumentValue::Json(value) => {
                value.as_str().map(ToOwned::to_owned)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        descriptions,
        ["hotel", "lounge pass"],
        "{:?} {:?}",
        run.understanding,
        run.called()
    );
}
