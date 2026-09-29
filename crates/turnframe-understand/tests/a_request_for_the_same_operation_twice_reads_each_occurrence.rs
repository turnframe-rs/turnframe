//! A request asking for the same operation twice («add lounge at 80 and meals at 15»)
//! is two acts, and each extraction is told which occurrence it reads, counting in the order
//! the message says them.
mod support;

use serde_json::json;
use support::{ADD_EXTRA, confirmed, one_request, script, turn, understand};

fn line(description: (usize, usize), amount: (usize, usize), euros: &str) -> serde_json::Value {
    json!({"arguments": {
        "description": {"kind": "words", "text": "", "message": "current",
                        "from": description.0, "to": description.1},
        "amount": {"kind": "money", "message": "current", "from": amount.0, "to": amount.1,
                   "amount": euros, "currency": "EUR"}
    }})
}

#[tokio::test]
async fn a_request_for_the_same_operation_twice_reads_each_occurrence() {
    // [1]add [2]lounge [3]at [4]80 [5]euros [6]and [7]meals [8]at [9]15 [10]euros
    let script = script()
        .answer("turn/segment", one_request(1, 10))
        .answer("u1/route", json!({"operations": [ADD_EXTRA, ADD_EXTRA]}))
        .answer("u1/extract", line((2, 2), (4, 5), "80"))
        .answer("u1.a2/extract", line((7, 7), (9, 10), "15"))
        .answer(
            "u1/verify",
            confirmed(json!({"description": "stated", "amount": "stated"})),
        )
        .answer(
            "u1.a2/verify",
            confirmed(json!({"description": "stated", "amount": "stated"})),
        );
    let run = understand(
        script,
        &turn("add lounge at 80 euros and meals at 15 euros"),
    )
    .await;

    assert_eq!(run.understanding.acts.len(), 2, "{:?}", run.understanding);
    let told = |task: &str| {
        run.provider
            .calls()
            .iter()
            .find(|call| format!("{:?}", call.metadata).contains(&format!("\"{task}\"")))
            .map(|call| format!("{:?}", call.messages))
            .unwrap_or_default()
    };
    assert!(
        told("u1/extract").contains("occurrence 1 of 2"),
        "{}",
        told("u1/extract")
    );
    assert!(
        told("u1.a2/extract").contains("occurrence 2 of 2"),
        "{}",
        told("u1.a2/extract")
    );
}
