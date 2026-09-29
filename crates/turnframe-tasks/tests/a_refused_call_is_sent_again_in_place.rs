//! A call a provider refused for a reason another try can change is sent again, in
//! place, before the task fails; one it found invalid is not.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{PickColour, answer, colours, router, scope};
use turnframe_core::replay::TaskVerdict;
use turnframe_provider::capabilities::{ProviderCapabilities, StructuredOutputCapability};
use turnframe_provider::error::ProviderError;
use turnframe_provider::testing::StaticProvider;
use turnframe_tasks::{TaskCall, TaskEngine, TaskId};

fn failing_first(error: ProviderError) -> Arc<StaticProvider> {
    Arc::new(
        StaticProvider::new("p", "m")
            .with_capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
                    .with_temperature(true),
            )
            .failing_once(error)
            .replying_once_json(answer("red")),
    )
}

async fn run(model: &Arc<StaticProvider>) -> (Option<String>, Vec<TaskVerdict>) {
    let engine = TaskEngine::builder(router(vec![(Arc::clone(model), &[])])).build();
    let scope = scope();
    let id = TaskId::new("u1").child("route");
    let call = TaskCall {
        id: &id,
        parent: None,
        depth: 1,
    };
    let outcome = engine.run(&scope, call, &PickColour, &colours()).await;
    let verdicts = scope
        .records()
        .into_iter()
        .map(|record| record.verdict)
        .collect();
    (outcome.accepted().map(|picked| picked.colour), verdicts)
}

#[tokio::test]
async fn a_refusal_is_retried_and_the_second_answer_is_used() {
    let model = failing_first(ProviderError::refusal());
    let (picked, verdicts) = run(&model).await;
    assert_eq!(picked.as_deref(), Some("red"));
    assert!(
        matches!(verdicts[0], TaskVerdict::Failed { .. }),
        "{verdicts:?}"
    );
    assert_eq!(verdicts[1], TaskVerdict::Accepted);
}

#[tokio::test]
async fn an_invalid_request_is_not_retried() {
    let model = failing_first(ProviderError::invalid_request("bad_schema"));
    let (picked, verdicts) = run(&model).await;
    assert_eq!(picked, None);
    assert_eq!(verdicts.len(), 1, "the same call would fail the same way");
}
