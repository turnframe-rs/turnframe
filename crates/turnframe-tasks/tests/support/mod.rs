//! A toy task and a scripted provider pool for the engine's tests.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use turnframe_core::locale::Locale;
use turnframe_provider::capabilities::{ProviderCapabilities, StructuredOutputCapability};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::request::Message;
use turnframe_provider::router::{PolicyRouter, ProviderPool};
use turnframe_provider::testing::StaticProvider;
use turnframe_tasks::{Budget, ModelTask, StructuralError, TaskKind, TaskScope};

/// Picks the colour the user named, from a list the schema leaves open and `check` closes.
pub struct PickColour;

pub struct Colours {
    pub allowed: Vec<&'static str>,
    pub text: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colour {
    pub colour: String,
}

impl ModelTask for PickColour {
    type Input = Colours;
    type Output = Colour;

    fn kind(&self) -> TaskKind {
        TaskKind::Route
    }

    fn prompt_name(&self) -> &str {
        "test.pick_colour"
    }

    fn instructions(&self) -> &str {
        "Pick the colour the user names."
    }

    fn schema(&self, _input: &Colours) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["colour"],
            "properties": {"colour": {"type": "string"}}
        })
    }

    fn render(&self, input: &Colours) -> Vec<Message> {
        vec![Message::user(input.text)]
    }

    fn check(&self, input: &Colours, output: &Colour) -> Result<(), StructuralError> {
        if input.allowed.contains(&output.colour.as_str()) {
            return Ok(());
        }
        Err(StructuralError::new(
            "unknown_colour",
            format!(
                "`colour` must be one of: {}; `{}` is not",
                input.allowed.join(", "),
                output.colour
            ),
        ))
    }
}

pub fn colours() -> Colours {
    Colours {
        allowed: vec!["red", "blue"],
        text: "the red one",
    }
}

pub fn answer(colour: &str) -> serde_json::Value {
    json!({ "colour": colour })
}

/// A provider answering each call with the next of `answers`.
pub fn provider(name: &str, answers: &[serde_json::Value]) -> Arc<StaticProvider> {
    let mut provider = StaticProvider::new(name, "m").with_capabilities(
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
            .with_temperature(true),
    );
    for value in answers {
        provider = provider.replying_once_json(value.clone());
    }
    Arc::new(provider)
}

/// A router over `providers`, each registered with its tags.
pub fn router(providers: Vec<(Arc<StaticProvider>, &[&str])>) -> Arc<PolicyRouter> {
    let mut pool = ProviderPool::builder();
    for (provider, tags) in providers {
        pool = pool.provider_tagged(provider as Arc<dyn ModelProvider>, tags.iter().copied());
    }
    Arc::new(PolicyRouter::new(Arc::new(pool.build().unwrap())))
}

pub fn scope() -> TaskScope {
    TaskScope::new(Budget::understanding(), Locale::from("en-GB"))
}
