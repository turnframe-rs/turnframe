//! A call the budget does not allow is refused before it reaches a provider.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{PickColour, answer, colours, provider, router};
use turnframe_core::locale::Locale;
use turnframe_tasks::{
    Budget, BudgetBound, TaskCall, TaskEngine, TaskFailure, TaskId, TaskOutcome, TaskScope,
};

#[tokio::test]
async fn the_repair_that_would_exceed_the_budget_is_not_sent() {
    let model = provider("p", &[answer("green"), answer("red")]);
    let engine = TaskEngine::builder(router(vec![(model.clone(), &[])])).build();
    let mut budget = Budget::understanding();
    budget.max_model_calls = Some(1);
    let scope = TaskScope::new(budget, Locale::from("en-GB"));
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

    assert!(matches!(
        outcome,
        TaskOutcome::Failed {
            failure: TaskFailure::Budget(BudgetBound::ModelCalls),
            ..
        }
    ));
    assert_eq!(
        model.call_count(),
        1,
        "only the call the budget allowed was sent"
    );
    assert_eq!(
        scope.budget_report().exhausted.as_deref(),
        Some("model_calls")
    );
}
