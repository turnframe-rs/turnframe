//! Every call a provider makes, as it went over the wire, for a person debugging a
//! turn: the request with its prompts and schema, and what came back.
//!
//! [`TracedProvider`] wraps any provider and reports each call to a [`CallTrace`]. It
//! is opt-in and meant for local debugging: a trace holds the users' words and every
//! prompt, so it is written where the deployment decides and nowhere else. The API
//! key never reaches it; adapters add credentials below the request.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::StreamExt;

use crate::capabilities::{
    CapabilityMismatch, CapabilityRequirements, ModelProfile, ProviderCapabilities,
};
use crate::error::ProviderError;
use crate::ids::{ModelKey, ModelRef, ProviderKey};
use crate::provider::ModelProvider;
use crate::request::ModelRequest;
use crate::response::{FinishReason, ModelResponse, TokenUsage};
use crate::stream::{ModelStream, StreamEvent};

/// What came back from one traced call.
#[derive(Debug)]
#[non_exhaustive]
pub enum TracedOutcome<'a> {
    /// A whole answer.
    Response(&'a ModelResponse),
    /// A streamed answer, as it was put together.
    Streamed {
        /// The text, every fragment in order.
        text: &'a str,
        /// Why it ended, when the stream said.
        finish: Option<FinishReason>,
        /// Tokens, when the stream reported them.
        usage: Option<TokenUsage>,
    },
    /// No answer.
    Failed(&'a ProviderError),
}

/// One call, as the trace receives it.
#[derive(Debug)]
#[non_exhaustive]
pub struct TracedCall<'a> {
    /// Who answered.
    pub model: &'a ModelRef,
    /// The request, verbatim.
    pub request: &'a ModelRequest,
    /// What came back.
    pub outcome: TracedOutcome<'a>,
    /// From the call to its answer, or to the end of its stream.
    pub latency: Duration,
}

/// Where a [`TracedProvider`] reports its calls. It must not block.
pub trait CallTrace: Send + Sync {
    /// Records one call.
    fn call(&self, call: &TracedCall<'_>);
}

/// A provider that reports every call to a [`CallTrace`], and is otherwise the one it
/// wraps.
#[derive(Clone)]
pub struct TracedProvider {
    inner: Arc<dyn ModelProvider>,
    trace: Arc<dyn CallTrace>,
}

impl std::fmt::Debug for TracedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TracedProvider")
            .field("model", &self.inner.reference())
            .finish_non_exhaustive()
    }
}

impl TracedProvider {
    /// `inner`, reporting to `trace`.
    #[must_use]
    pub fn new(inner: Arc<dyn ModelProvider>, trace: Arc<dyn CallTrace>) -> Self {
        Self { inner, trace }
    }
}

#[async_trait]
impl ModelProvider for TracedProvider {
    fn provider_key(&self) -> ProviderKey {
        self.inner.provider_key()
    }

    fn model_key(&self) -> ModelKey {
        self.inner.model_key()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn profile(&self) -> ModelProfile {
        self.inner.profile()
    }

    fn supports(&self, requirements: &CapabilityRequirements) -> Result<(), CapabilityMismatch> {
        self.inner.supports(requirements)
    }

    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        let started = Instant::now();
        let answer = self.inner.generate(request.clone()).await;
        let outcome = match &answer {
            Ok(response) => TracedOutcome::Response(response),
            Err(error) => TracedOutcome::Failed(error),
        };
        self.trace.call(&TracedCall {
            model: &self.inner.reference(),
            request: &request,
            outcome,
            latency: started.elapsed(),
        });
        answer
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        let started = Instant::now();
        let model = self.inner.reference();
        let stream = match self.inner.stream(request.clone()).await {
            Ok(stream) => stream,
            Err(error) => {
                self.trace.call(&TracedCall {
                    model: &model,
                    request: &request,
                    outcome: TracedOutcome::Failed(&error),
                    latency: started.elapsed(),
                });
                return Err(error);
            }
        };
        let seen = Arc::new(Mutex::new(Seen::default()));
        let recorder = Arc::clone(&seen);
        let trace = Arc::clone(&self.trace);
        let reported = stream.inspect(move |item| {
            let mut seen = recorder.lock().unwrap_or_else(PoisonError::into_inner);
            match item {
                Ok(StreamEvent::TextDelta { text }) => seen.text.push_str(text),
                Ok(StreamEvent::Usage { usage }) => seen.usage = Some(*usage),
                Ok(StreamEvent::Finish { reason }) => seen.finish = Some(*reason),
                Ok(_) => {}
                Err(error) => seen.error = Some(error.clone()),
            }
        });
        let ended = futures::stream::once(async move {
            let seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
            let outcome = match &seen.error {
                Some(error) => TracedOutcome::Failed(error),
                None => TracedOutcome::Streamed {
                    text: &seen.text,
                    finish: seen.finish,
                    usage: seen.usage,
                },
            };
            trace.call(&TracedCall {
                model: &model,
                request: &request,
                outcome,
                latency: started.elapsed(),
            });
        })
        .filter_map(|()| async { None });
        Ok(ModelStream::new(reported.chain(ended)))
    }
}

/// What a traced stream has carried so far.
#[derive(Default)]
struct Seen {
    text: String,
    finish: Option<FinishReason>,
    usage: Option<TokenUsage>,
    error: Option<ProviderError>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::purpose::ModelPurpose;
    use crate::request::Message;
    use crate::testing::StaticProvider;

    #[derive(Default)]
    struct Recorded(Mutex<Vec<(String, String)>>);

    impl CallTrace for Recorded {
        fn call(&self, call: &TracedCall<'_>) {
            let said = match &call.outcome {
                TracedOutcome::Response(response) => response.text(),
                TracedOutcome::Streamed { text, .. } => (*text).to_owned(),
                TracedOutcome::Failed(error) => format!("failed: {}", error.kind().as_str()),
            };
            let asked = call.request.messages[0].text();
            self.0.lock().unwrap().push((asked, said));
        }
    }

    #[tokio::test]
    async fn a_call_is_traced_with_its_request_and_its_answer() {
        let recorded = Arc::new(Recorded::default());
        let inner: Arc<dyn ModelProvider> =
            Arc::new(StaticProvider::new("static", "model-1").answering_text("ok"));
        let traced = TracedProvider::new(inner, Arc::clone(&recorded) as Arc<dyn CallTrace>);
        let request =
            ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("hi"));
        let answer = traced.generate(request).await.unwrap();
        assert_eq!(answer.text(), "ok");
        assert_eq!(
            *recorded.0.lock().unwrap(),
            vec![("hi".to_owned(), "ok".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_streamed_call_is_traced_once_it_ends_with_the_whole_text() {
        use crate::capabilities::ProviderCapabilities;
        let recorded = Arc::new(Recorded::default());
        let inner: Arc<dyn ModelProvider> = Arc::new(
            StaticProvider::new("static", "model-1")
                .with_capabilities(ProviderCapabilities::minimal().with_streaming(true))
                .answering_stream(vec![
                    StreamEvent::text("ciao "),
                    StreamEvent::text("mondo"),
                    StreamEvent::Finish {
                        reason: FinishReason::Stop,
                    },
                ]),
        );
        let traced = TracedProvider::new(inner, Arc::clone(&recorded) as Arc<dyn CallTrace>);
        let request =
            ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("hi"));
        let stream = traced.stream(request).await.unwrap();
        assert!(
            recorded.0.lock().unwrap().is_empty(),
            "nothing before the stream ends"
        );
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 3, "the trace adds no event to the stream");
        assert_eq!(
            *recorded.0.lock().unwrap(),
            vec![("hi".to_owned(), "ciao mondo".to_owned())]
        );
    }
}
