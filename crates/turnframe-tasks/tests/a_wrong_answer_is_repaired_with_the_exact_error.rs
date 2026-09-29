//! A structurally wrong answer goes back to the same model once, with the exact error.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{PickColour, answer, colours, provider, router, scope};
use turnframe_core::replay::TaskVerdict;
use turnframe_provider::request::ContentPart;
use turnframe_tasks::{TaskCall, TaskEngine, TaskId};

#[tokio::test]
async fn the_repair_round_quotes_the_failed_check_and_the_second_answer_is_used() {
    let model = provider("p", &[answer("green"), answer("red")]);
    let engine = TaskEngine::builder(router(vec![(model.clone(), &[])])).build();
    let scope = scope();
    let id = TaskId::new("u0").child("route");

    let outcome = engine
        .run(
            &scope,
            TaskCall {
                id: &id,
                parent: None,
                depth: 1,
            },
            &PickColour,
            &colours(),
        )
        .await;

    assert_eq!(
        outcome.depth(),
        2,
        "the repair is one more call in the chain"
    );
    assert_eq!(outcome.accepted().unwrap().colour, "red");
    let repair = model.last_call().unwrap();
    let last = repair.messages.last().unwrap();
    let ContentPart::Text { text } = &last.content[0] else {
        panic!("the repair is text");
    };
    assert!(text.contains("`green` is not"), "{text}");

    let records = scope.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].task_id, "u0/route");
    assert!(
        matches!(&records[0].verdict, TaskVerdict::Rejected { code, .. } if code == "unknown_colour")
    );
    assert_eq!(records[1].task_id, "u0/route#repair1");
    assert_eq!(records[1].verdict, TaskVerdict::Accepted);
}
