//! Votes with no majority, under `Reread`, run the task once more shown the answers that
//! disagreed, and its answer stands. With no answer to show, nothing more is sent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{PickColour, answer, colours, provider, router, scope};
use turnframe_provider::testing::StaticProvider;
use turnframe_tasks::{
    Disagreement, TaskCall, TaskEngine, TaskId, TaskKind, TaskOutcome, TaskProfiles,
};

fn rereading(answers: &[serde_json::Value]) -> (TaskEngine, Arc<StaticProvider>) {
    let p = provider("p", answers);
    let profiles = TaskProfiles::new().adjust(TaskKind::Route, |profile| {
        profile
            .with_votes(3)
            .with_repairs(0)
            .on_disagreement(Disagreement::Reread)
    });
    let engine = TaskEngine::builder(router(vec![(Arc::clone(&p), &[])]))
        .profiles(profiles)
        .build();
    (engine, p)
}

#[tokio::test]
async fn a_split_vote_is_read_once_more() {
    // `colours()` allows red and blue: the third vote fails its check, leaving one each.
    let (engine, p) = rereading(&[
        answer("red"),
        answer("blue"),
        answer("violet"),
        answer("blue"),
    ]);
    let scope = scope();
    let id = TaskId::new("u0").child("route");
    let call = TaskCall {
        id: &id,
        parent: None,
        depth: 1,
    };

    let outcome = engine.run(&scope, call, &PickColour, &colours()).await;

    assert_eq!(outcome.accepted().unwrap().colour, "blue");
    let calls = p.calls();
    assert_eq!(calls.len(), 4);
    let shown = format!("{:?}", calls[3].messages);
    assert!(shown.contains("red") && shown.contains("blue"), "{shown}");
}

#[tokio::test]
async fn votes_that_all_failed_are_not_read_again() {
    let (engine, p) = rereading(&[answer("violet"), answer("violet"), answer("violet")]);
    let scope = scope();
    let id = TaskId::new("u0").child("route");
    let call = TaskCall {
        id: &id,
        parent: None,
        depth: 1,
    };

    let outcome = engine.run(&scope, call, &PickColour, &colours()).await;

    assert!(matches!(
        outcome,
        TaskOutcome::Failed { .. } | TaskOutcome::Disagreed { .. }
    ));
    assert_eq!(p.calls().len(), 3, "no fourth call");
}
