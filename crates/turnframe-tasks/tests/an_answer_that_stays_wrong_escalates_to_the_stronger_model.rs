//! An answer still wrong after its repairs is asked of the profile's escalation model.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{PickColour, answer, colours, provider, router, scope};
use turnframe_tasks::{TaskCall, TaskEngine, TaskId, TaskKind, TaskProfiles};

#[tokio::test]
async fn the_escalation_model_answers_what_the_small_one_could_not() {
    let small = provider("small", &[answer("green"), answer("green")]);
    let large = provider("large", &[answer("blue")]);
    let profiles = TaskProfiles::new().adjust(TaskKind::Route, |profile| {
        profile.on_model("small").escalating_to("large")
    });
    let engine = TaskEngine::builder(router(vec![
        (small.clone(), &["small"]),
        (large.clone(), &["large"]),
    ]))
    .profiles(profiles)
    .build();
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

    assert_eq!(outcome.accepted().unwrap().colour, "blue");
    assert_eq!(small.call_count(), 2, "the first call and its repair");
    assert_eq!(large.call_count(), 1);
    let ids: Vec<String> = scope.records().into_iter().map(|r| r.task_id).collect();
    assert_eq!(
        ids,
        vec!["u0/route", "u0/route#repair1", "u0/route#escalation"]
    );
}
