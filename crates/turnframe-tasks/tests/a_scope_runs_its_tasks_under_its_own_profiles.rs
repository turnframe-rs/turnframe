//! A scope that carries profiles runs its tasks under them, over the engine's own: one
//! engine serves turns of every effort.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{PickColour, answer, colours, provider, router, scope};
use turnframe_core::effort::Effort;
use turnframe_tasks::{TaskCall, TaskEngine, TaskId, TaskKind, TaskProfiles};

#[tokio::test]
async fn a_scope_runs_its_tasks_under_its_own_profiles() {
    let engine = TaskEngine::builder(router(vec![(
        provider("p", &[answer("red"), answer("red"), answer("blue")]),
        &[],
    )]))
    .build();
    let voting = TaskProfiles::new().adjust(TaskKind::Route, |p| p.with_votes(3));
    let scope = scope().with_profiles(voting).with_effort(Effort::High);
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

    assert_eq!(outcome.accepted().unwrap().colour, "red");
    assert_eq!(
        scope.records().len(),
        3,
        "three votes, from the scope's profile"
    );
    assert_eq!(engine.profile(&scope, TaskKind::Route).votes, 3);
    assert_eq!(
        engine.profiles().get(TaskKind::Route).votes,
        1,
        "the engine's own is untouched"
    );
    assert_eq!(scope.effort(), Some(Effort::High));
}
