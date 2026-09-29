//! Votes agree by the task's own notion of agreement; no majority is handed back to ask.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{PickColour, answer, colours, provider, router, scope};
use turnframe_core::replay::TaskVerdict;
use turnframe_tasks::{TaskCall, TaskEngine, TaskId, TaskKind, TaskOutcome, TaskProfiles};

fn voting_engine(answers: &[serde_json::Value]) -> TaskEngine {
    let profiles = TaskProfiles::new().adjust(TaskKind::Route, |profile| {
        profile.with_votes(3).with_repairs(0)
    });
    TaskEngine::builder(router(vec![(provider("p", answers), &[])]))
        .profiles(profiles)
        .build()
}

#[tokio::test]
async fn two_of_three_win_and_the_third_is_marked_outvoted() {
    let engine = voting_engine(&[answer("red"), answer("blue"), answer("red")]);
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

    assert_eq!(outcome.accepted().unwrap().colour, "red");
    let verdicts: Vec<TaskVerdict> = scope.records().into_iter().map(|r| r.verdict).collect();
    assert_eq!(
        verdicts
            .iter()
            .filter(|v| **v == TaskVerdict::Accepted)
            .count(),
        2
    );
    assert_eq!(
        verdicts
            .iter()
            .filter(|v| **v == TaskVerdict::Outvoted)
            .count(),
        1
    );
}

#[tokio::test]
async fn no_majority_hands_the_answers_back_so_the_user_can_be_asked() {
    let engine = voting_engine(&[answer("red"), answer("blue"), answer("green")]);
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

    let TaskOutcome::Disagreed { answers, .. } = outcome else {
        panic!("one red, one blue and one invalid answer have no majority");
    };
    let mut colours: Vec<String> = answers.into_iter().map(|a| a.colour).collect();
    colours.sort();
    assert_eq!(colours, vec!["blue", "red"]);
}
