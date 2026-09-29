//! A task re-run after a verifier's objection shows the model its answer and the objection.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Colour, PickColour, answer, colours, provider, router, scope};
use turnframe_provider::request::{ContentPart, Role};
use turnframe_tasks::{TaskCall, TaskEngine, TaskId};

#[tokio::test]
async fn the_previous_answer_and_the_objection_are_in_the_request() {
    let model = provider("p", &[answer("blue")]);
    let engine = TaskEngine::builder(router(vec![(model.clone(), &[])])).build();
    let scope = scope();
    let id = TaskId::new("u0").child("route");
    let previous = Colour {
        colour: "red".to_owned(),
    };

    let outcome = engine
        .run_with_feedback(
            &scope,
            TaskCall {
                id: &id,
                parent: None,
                depth: 3,
            },
            &PickColour,
            &colours(),
            &previous,
            "The user named blue, not red.",
        )
        .await;

    assert_eq!(outcome.accepted().unwrap().colour, "blue");
    let request = model.last_call().unwrap();
    let texts: Vec<(Role, String)> = request
        .messages
        .iter()
        .map(|message| {
            let ContentPart::Text { text } = &message.content[0] else {
                panic!("text messages only");
            };
            (message.role, text.clone())
        })
        .collect();
    assert!(
        texts
            .iter()
            .any(|(role, text)| *role == Role::Assistant && text.contains("\"red\""))
    );
    assert!(
        texts
            .iter()
            .any(|(role, text)| *role == Role::User && text.contains("named blue"))
    );
}
