//! Each call's record names its instructions, its settings and a digest of what was sent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{PickColour, answer, colours, provider, router, scope};
use turnframe_tasks::instructions::BUILT_IN_VERSION;
use turnframe_tasks::{RecordPolicy, TaskCall, TaskEngine, TaskId};

#[tokio::test]
async fn a_record_carries_the_prompt_the_settings_and_the_digest_and_keeps_prompts_only_on_request()
{
    let id = TaskId::new("u0").child("route");
    let call = TaskCall {
        id: &id,
        parent: None,
        depth: 1,
    };

    let engine = TaskEngine::builder(router(vec![(provider("p", &[answer("red")]), &[])])).build();
    let default_scope = scope();
    engine
        .run(&default_scope, call, &PickColour, &colours())
        .await;
    let record = &default_scope.records()[0];
    let prompt = record.prompt_ref.as_ref().unwrap();
    assert_eq!(prompt.name.as_str(), "test.pick_colour");
    assert_eq!(prompt.version.as_str(), BUILT_IN_VERSION);
    assert_eq!(record.params.temperature, Some(0.0));
    assert_eq!(record.params.reasoning_effort.as_deref(), Some("minimal"));
    assert_eq!(record.params.max_output_tokens, Some(150));
    assert!(record.input_digest.is_some());
    assert!(
        record.rendered.is_none(),
        "prompts carry the user's words and are kept on request"
    );
    assert_eq!(record.parsed, Some(serde_json::json!({"colour": "red"})));

    let mut policy = RecordPolicy::default();
    policy.keep_prompts = true;
    let keeping = TaskEngine::builder(router(vec![(provider("p", &[answer("red")]), &[])]))
        .records(policy)
        .build();
    let keeping_scope = scope();
    keeping
        .run(&keeping_scope, call, &PickColour, &colours())
        .await;
    assert!(keeping_scope.records()[0].rendered.is_some());
}
