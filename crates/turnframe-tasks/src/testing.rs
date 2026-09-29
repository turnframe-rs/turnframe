//! A provider that answers each task by its id, for testing anything built on tasks.
//!
//! Tasks run concurrently, so a queue answered in call order is a race. [`ScriptedTasks`]
//! reads the task id every request carries under [`TASK_LABEL`] and answers from that
//! task's own queue. An exact call id (`u1/extract#repair1`) is looked up before its task
//! (`u1/extract`), so a repair or a vote can be scripted apart from the first call.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use turnframe_provider::capabilities::{
    ModelProfile, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{ModelKey, ProviderKey};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::request::ModelRequest;
use turnframe_provider::response::{ModelResponse, TokenUsage};
use turnframe_provider::router::{PolicyRouter, ProviderPool};

use crate::engine::TASK_LABEL;

/// One queued reply: an answer, or a provider failure.
enum Scripted {
    Answer(serde_json::Value),
    Failure(ProviderError),
}

/// Answers each task from its own queue of JSON documents.
pub struct ScriptedTasks {
    profile: ModelProfile,
    answers: Mutex<BTreeMap<String, VecDeque<Scripted>>>,
    calls: Mutex<Vec<ModelRequest>>,
}

impl ScriptedTasks {
    /// A provider with native schema support and no answers yet.
    #[must_use]
    pub fn new(provider: impl Into<ProviderKey>, model: impl Into<ModelKey>) -> Self {
        let capabilities = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
            .with_temperature(true);
        Self {
            profile: ModelProfile::new(provider, model, capabilities),
            answers: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Queues `answer` for the task or call `task`.
    #[must_use]
    pub fn answer(self, task: &str, answer: serde_json::Value) -> Self {
        self.lock_answers()
            .entry(task.to_owned())
            .or_default()
            .push_back(Scripted::Answer(answer));
        self
    }

    /// Queues a provider failure for the task or call `task`.
    #[must_use]
    pub fn failing(self, task: &str, error: ProviderError) -> Self {
        self.lock_answers()
            .entry(task.to_owned())
            .or_default()
            .push_back(Scripted::Failure(error));
        self
    }

    /// Adds a tag to the profile, for routing by tag.
    #[must_use]
    pub fn tagged(mut self, tag: impl Into<String>) -> Self {
        self.profile.tags.push(tag.into());
        self
    }

    /// Every request received, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<ModelRequest> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The task ids called, in order.
    #[must_use]
    pub fn called(&self) -> Vec<String> {
        self.calls()
            .iter()
            .map(|request| {
                request
                    .metadata
                    .get(TASK_LABEL)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    /// Tasks with answers never asked for.
    #[must_use]
    pub fn unanswered(&self) -> Vec<String> {
        self.lock_answers()
            .iter()
            .filter(|(_, queue)| !queue.is_empty())
            .map(|(task, _)| task.clone())
            .collect()
    }

    /// A router over this provider alone.
    #[must_use]
    pub fn router(self: &Arc<Self>) -> Arc<PolicyRouter> {
        let provider: Arc<dyn ModelProvider> = self.clone();
        let pool = ProviderPool::builder()
            .provider(provider)
            .build()
            .unwrap_or_else(|error| unreachable!("a pool of one provider builds: {error}"));
        Arc::new(PolicyRouter::new(Arc::new(pool)))
    }

    fn lock_answers(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, VecDeque<Scripted>>> {
        self.answers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn take(&self, call: &str) -> Option<Scripted> {
        let mut answers = self.lock_answers();
        let task = call.split('#').next().unwrap_or(call);
        for key in [call, task] {
            if let Some(answer) = answers.get_mut(key).and_then(VecDeque::pop_front) {
                return Some(answer);
            }
        }
        None
    }
}

#[async_trait]
impl ModelProvider for ScriptedTasks {
    fn provider_key(&self) -> ProviderKey {
        self.profile.provider.clone()
    }

    fn model_key(&self) -> ModelKey {
        self.profile.model.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.profile.capabilities.clone()
    }

    fn profile(&self) -> ModelProfile {
        self.profile.clone()
    }

    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let Some(call) = request.metadata.get(TASK_LABEL) else {
            // Not a task: another provider in the pool may answer it.
            return Err(ProviderError::unsupported("untasked_request"));
        };
        let answer = match self.take(call) {
            Some(Scripted::Answer(answer)) => answer,
            Some(Scripted::Failure(error)) => return Err(error),
            None => {
                return Err(ProviderError::invalid_request("unscripted_task").with_detail(call));
            }
        };
        // Usage a budget can be tested against: about four characters a token.
        let text = answer.to_string();
        let prompt: usize = request
            .messages
            .iter()
            .map(|message| message.text().len())
            .sum();
        let prompt = prompt + request.system.as_ref().map_or(0, String::len);
        let tokens = |characters: usize| u64::try_from(characters / 4).unwrap_or(u64::MAX);
        let usage = TokenUsage::new(tokens(prompt), tokens(text.len()));
        Ok(ModelResponse::new(
            request.request_id,
            self.profile.provider.clone(),
            self.profile.model.clone(),
        )
        .with_text(text)
        .with_usage(usage))
    }
}

impl fmt::Debug for ScriptedTasks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedTasks")
            .field("model", &self.profile.reference().to_string())
            .field("unanswered", &self.unanswered())
            .finish_non_exhaustive()
    }
}
