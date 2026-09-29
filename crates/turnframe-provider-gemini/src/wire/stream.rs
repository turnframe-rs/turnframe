//! Server-sent events → normalized stream events.
//!
//! `streamGenerateContent?alt=sse` answers with a sequence of `data:` lines,
//! each one a whole `GenerateContentResponse` carrying the *next slice* of the
//! same candidate. There is no `[DONE]` sentinel: the stream ends when the
//! connection closes, and the last chunk is the one that carries a
//! `finishReason` and the complete `usageMetadata`.
//!
//! [`StreamDecoder`] turns that into the vocabulary
//! [`StreamAccumulator`](turnframe_provider::stream::StreamAccumulator)
//! consumes, and the contract it must keep is strict: reassembling the events
//! must produce the *same* [`ModelResponse`](turnframe_provider::response::ModelResponse)
//! the non-streamed call returns for the same exchange (spec §20.8). Four
//! decisions follow from that.
//!
//! **A function call arrives whole.** Gemini does not slice `args` across
//! chunks the way a JSON-string argument list gets sliced elsewhere: a
//! `functionCall` part appears complete inside one chunk. So a call is
//! announced, filled and closed in one go, in the order the parts arrived —
//! which is the order the non-streamed path reads them in.
//!
//! **Every open call is closed before the finish event**, and usage is emitted
//! after it, which is the order the service itself uses.
//!
//! **Thought parts are dropped, not streamed.** The model's own reasoning is
//! not its answer, and spec §18.5 streams only model-authored prose.
//!
//! **A stream that ends without a `finishReason` emits no finish at all**, so
//! the accumulator reports the truncation. A silent `Stop` there would turn a
//! dropped connection into a short answer — and since Gemini has no sentinel,
//! the `finishReason` is the *only* evidence the answer is complete.

use std::collections::VecDeque;
use std::pin::Pin;

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::stream::{Stream, StreamExt};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelRef};
use turnframe_provider::response::ResponseWarning;
use turnframe_provider::secret::Redactor;
use turnframe_provider::stream::{ModelStream, StreamEvent, StreamItem};

use crate::error::{ApiErrorEnvelope, ResponseHints, classify};
use crate::wire::response::{Candidate, GenerateContentResponse, finish_reason};

/// Rebuilds normalized stream events from `GenerateContentResponse` chunks.
#[derive(Debug, Default)]
pub(crate) struct StreamDecoder {
    /// How many calls have been announced, which is also the next synthesized
    /// id — the same numbering the non-streamed path uses.
    calls: usize,
    finished: bool,
    /// Whether the service's `responseId` has been reported already. It repeats
    /// on every chunk and the rebuilt response only needs it once.
    announced_id: bool,
}

impl StreamDecoder {
    /// A decoder for one stream.
    pub(crate) const fn new() -> Self {
        Self {
            calls: 0,
            finished: false,
            announced_id: false,
        }
    }

    /// Absorbs one chunk and returns the events it produced, in order.
    ///
    /// A chunk whose candidate was blocked, or whose prompt feedback reports a
    /// block, ends the stream with a
    /// [`ContentFilter`](turnframe_provider::error::ProviderErrorKind::ContentFilter)
    /// failure rather than a finish event: a safety stop mid-stream is the same
    /// outcome it is at the end of a whole call.
    pub(crate) fn push(&mut self, chunk: &GenerateContentResponse) -> Vec<StreamItem> {
        let mut events = Vec::new();
        if let Some(reason) = chunk
            .prompt_feedback
            .as_ref()
            .and_then(|feedback| feedback.block_reason.as_deref())
            .filter(|reason| !reason.is_empty() && *reason != "BLOCK_REASON_UNSPECIFIED")
        {
            return vec![Err(ProviderError::content_filter().with_code(reason))];
        }
        if !self.announced_id
            && let Some(id) = chunk.response_id.as_deref().filter(|id| !id.is_empty())
        {
            self.announced_id = true;
            events.push(StreamEvent::response_id(id));
        }
        if let Some(candidate) = chunk.candidates.first() {
            self.push_candidate(candidate, &mut events);
        }
        let mut items: Vec<StreamItem> = events.into_iter().map(Ok).collect();
        if let Some(usage) = &chunk.usage_metadata {
            // The service repeats usage on every chunk with growing counts, and
            // the accumulator keeps the last one — which is the complete one.
            items.push(Ok(StreamEvent::Usage {
                usage: usage.normalize(),
            }));
        }
        items
    }

    /// Absorbs one candidate slice.
    fn push_candidate(&mut self, candidate: &Candidate, events: &mut Vec<StreamEvent>) {
        let mut text = String::new();
        if let Some(content) = candidate.content.as_ref() {
            for part in &content.parts {
                if let Some(call) = part.function_call.as_ref() {
                    // A function call arrives complete, so it is announced,
                    // filled and closed at once.
                    let id = match call.id.as_deref().filter(|id| !id.is_empty()) {
                        Some(id) => CallId::new(id),
                        None => CallId::new(format!("call_{}", self.calls)),
                    };
                    self.calls += 1;
                    let arguments = call
                        .args
                        .clone()
                        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                    events.push(StreamEvent::tool_call_start(
                        id.clone(),
                        call.name.clone().unwrap_or_default(),
                    ));
                    events.push(StreamEvent::tool_call_delta(
                        id.clone(),
                        arguments.to_string(),
                    ));
                    events.push(StreamEvent::tool_call_end(id));
                    continue;
                }
                if part.thought {
                    continue;
                }
                if let Some(fragment) = part.text.as_deref() {
                    text.push_str(fragment);
                }
            }
        }
        if !text.is_empty() {
            // One delta per chunk, in arrival order: the accumulator
            // concatenates them into the single text part the non-streamed path
            // builds.
            events.insert(0, StreamEvent::text(text));
        }
        if let Some(reported) = candidate
            .finish_reason
            .as_deref()
            .filter(|label| !label.trim().is_empty())
        {
            self.close(Some(reported), events);
        }
    }

    /// Emits the finish event, once.
    fn close(&mut self, reported: Option<&str>, events: &mut Vec<StreamEvent>) {
        if self.finished {
            return;
        }
        // Warnings have no place in a stream; an unknown label is reported as
        // `Other`, which a structured stage refuses.
        let mut warnings: Vec<ResponseWarning> = Vec::new();
        let reason = finish_reason(reported, &mut warnings);
        self.finished = true;
        events.push(StreamEvent::Finish { reason });
    }

    /// Ends the stream.
    ///
    /// Gemini sends no sentinel, so a connection that closes without a
    /// `finishReason` is indistinguishable from one that was cut — and is
    /// treated as cut. Nothing is emitted, and the accumulator reports the
    /// truncation.
    pub(crate) const fn end(&self) -> Vec<StreamEvent> {
        Vec::new()
    }
}

/// Reads an HTTP response body as a normalized stream.
///
/// Every failure inside the stream becomes an item, never a panic: a chunk that
/// is not JSON, an error chunk, a safety block or a connection that drops all
/// end the stream with a typed [`ProviderError`].
pub(crate) fn model_stream(
    response: reqwest::Response,
    reference: ModelRef,
    redactor: Box<dyn Redactor>,
    warnings: Vec<ResponseWarning>,
) -> ModelStream {
    let events = Box::pin(response.bytes_stream().eventsource());
    let state = SseState {
        events,
        decoder: StreamDecoder::new(),
        // What the request conversion gave up leads the stream, so the streamed
        // path reports exactly what the whole path reports.
        pending: warnings
            .into_iter()
            .map(|warning| Ok(StreamEvent::warning(warning)))
            .collect(),
        done: false,
        reference,
        redactor,
    };
    ModelStream::new(futures::stream::unfold(state, next_item))
}

/// The byte stream, already framed into server-sent events.
type EventSource =
    Pin<Box<dyn Stream<Item = Result<Event, EventStreamError<reqwest::Error>>> + Send>>;

/// Everything the unfolded stream carries between polls.
struct SseState {
    events: EventSource,
    decoder: StreamDecoder,
    pending: VecDeque<StreamItem>,
    done: bool,
    reference: ModelRef,
    redactor: Box<dyn Redactor>,
}

impl SseState {
    /// Queues items, labelling every failure with the provider-model pair.
    fn queue(&mut self, items: Vec<StreamItem>) {
        for item in items {
            match item {
                Ok(event) => self.pending.push_back(Ok(event)),
                Err(error) => {
                    self.done = true;
                    self.pending
                        .push_back(Err(error.with_model(&self.reference)));
                    return;
                }
            }
        }
    }

    /// Queues a failure and closes the stream.
    fn fail(&mut self, error: ProviderError) {
        self.done = true;
        self.pending
            .push_back(Err(error.with_model(&self.reference)));
    }
}

/// Produces the next item, reading as many chunks as it takes to have one.
async fn next_item(mut state: SseState) -> Option<(StreamItem, SseState)> {
    loop {
        if let Some(item) = state.pending.pop_front() {
            return Some((item, state));
        }
        if state.done {
            return None;
        }
        match state.events.next().await {
            None => {
                state.done = true;
                let events = state.decoder.end();
                state.queue(events.into_iter().map(Ok).collect());
            }
            Some(Err(_transport)) => {
                state.fail(ProviderError::transport("stream_read_failed"));
            }
            Some(Ok(event)) => {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }
                match serde_json::from_str::<GenerateContentResponse>(data) {
                    Ok(chunk) => {
                        let items = state.decoder.push(&chunk);
                        state.queue(items);
                    }
                    Err(_) => {
                        // Google reports a mid-stream failure as an error
                        // object over the same 200, so a chunk that is not a
                        // response may still be a classifiable failure.
                        let envelope = ApiErrorEnvelope::decode(data);
                        if envelope.has_error() {
                            let error = classify(
                                0,
                                &ResponseHints::default(),
                                &envelope,
                                state.redactor.as_ref(),
                            );
                            state.fail(error);
                        } else {
                            state.fail(ProviderError::malformed("stream_chunk_not_json"));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use turnframe_provider::error::ProviderErrorKind;
    use turnframe_provider::ids::RequestId;
    use turnframe_provider::response::{FinishReason, TokenUsage};
    use turnframe_provider::stream::{StreamAccumulator, reconstruct};

    fn chunk(value: Value) -> GenerateContentResponse {
        serde_json::from_value(value).expect("decodes")
    }

    fn drain(chunks: Vec<Value>) -> Vec<StreamItem> {
        let mut decoder = StreamDecoder::new();
        let mut items = Vec::new();
        for value in chunks {
            items.extend(decoder.push(&chunk(value)));
        }
        items.extend(decoder.end().into_iter().map(Ok));
        items
    }

    fn events(chunks: Vec<Value>) -> Vec<StreamEvent> {
        drain(chunks)
            .into_iter()
            .map(|item| item.expect("no failure"))
            .collect()
    }

    fn slice(parts: Value, finish: Option<&str>) -> Value {
        let mut candidate = json!({"content": {"role": "model", "parts": parts}, "index": 0});
        if let Some(finish) = finish {
            candidate["finishReason"] = json!(finish);
        }
        json!({"candidates": [candidate], "modelVersion": "gemini-2.5-flash-001"})
    }

    async fn rebuild(
        chunks: Vec<Value>,
    ) -> Result<turnframe_provider::response::ModelResponse, ProviderError> {
        let stream = ModelStream::from_items(drain(chunks));
        let seed = StreamAccumulator::new(RequestId::nil(), "gemini", "gemini-2.5-flash-001");
        reconstruct(stream, seed).await
    }

    #[tokio::test]
    async fn text_slices_reassemble_into_one_part() {
        let response = rebuild(vec![
            slice(json!([{"text": "Ho preparato "}]), None),
            slice(json!([{"text": "la modifica."}]), None),
            json!({
                "candidates": [{"content": {"parts": [{"text": ""}]}, "finishReason": "STOP"}],
                "usageMetadata": {
                    "promptTokenCount": 42, "candidatesTokenCount": 7,
                    "cachedContentTokenCount": 30, "totalTokenCount": 49
                }
            }),
        ])
        .await
        .expect("reassembles");
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert_eq!(response.content.len(), 1);
        assert_eq!(response.finish, FinishReason::Stop);
        assert_eq!(response.usage, TokenUsage::new(42, 7).with_cached_input(30));
        assert!(response.warnings.contains(&ResponseWarning::Reconstructed));
    }

    #[tokio::test]
    async fn a_function_call_arrives_whole_and_is_closed_before_the_finish() {
        let emitted = events(vec![
            slice(
                json!([{"functionCall": {"name": "case.get", "args": {"target": "tok_1"}}}]),
                None,
            ),
            slice(json!([]), Some("STOP")),
        ]);
        let kinds: Vec<&str> = emitted.iter().map(StreamEvent::kind).collect();
        assert_eq!(
            kinds,
            vec![
                "tool_call_start",
                "tool_call_delta",
                "tool_call_end",
                "finish"
            ]
        );

        let response = rebuild(vec![
            slice(
                json!([{"functionCall": {"name": "case.get", "args": {"target": "tok_1"}}}]),
                None,
            ),
            slice(json!([]), Some("STOP")),
        ])
        .await
        .expect("reassembles");
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "call_0");
        assert_eq!(calls[0].name, "case.get");
        assert_eq!(calls[0].arguments, json!({"target": "tok_1"}));
    }

    #[tokio::test]
    async fn text_and_calls_come_back_in_the_same_shape_the_whole_answer_has() {
        let response = rebuild(vec![
            slice(json!([{"text": "controllo"}]), None),
            slice(
                json!([
                    {"functionCall": {"name": "a", "args": {"n": 1}}},
                    {"functionCall": {"name": "b", "args": {"n": 2}}}
                ]),
                None,
            ),
            slice(json!([{"text": " subito"}]), Some("STOP")),
        ])
        .await
        .expect("reassembles");
        // Text first, as one part, then the calls in order: exactly what
        // `build_response` produces for the same exchange.
        assert_eq!(response.content.len(), 3);
        assert_eq!(response.content[0].as_text(), Some("controllo subito"));
        let calls = response.tool_calls();
        assert_eq!(calls[0].id.as_str(), "call_0");
        assert_eq!(calls[1].id.as_str(), "call_1");
        assert_eq!(calls[1].arguments, json!({"n": 2}));
    }

    #[tokio::test]
    async fn a_stream_that_stops_without_a_finish_reason_is_a_truncation() {
        let error = rebuild(vec![slice(json!([{"text": "meta "}]), None)])
            .await
            .expect_err("no finish arrived");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("stream_ended_without_finish".to_owned())
        );
        assert!(matches!(error.kind(), ProviderErrorKind::Malformed));
    }

    #[test]
    fn a_safety_block_mid_stream_ends_it_with_a_content_filter() {
        let items = drain(vec![
            slice(json!([{"text": "Il tuo viaggiatore"}]), None),
            json!({
                "candidates": [{"finishReason": "SAFETY", "index": 0}],
                "usageMetadata": {"promptTokenCount": 8, "totalTokenCount": 8}
            }),
        ]);
        // The finish event still arrives, and it says `content_filter`, which
        // `is_complete()` refuses — so no structured stage parses the prefix.
        let finish = items
            .iter()
            .filter_map(|item| item.as_ref().ok())
            .find_map(|event| match event {
                StreamEvent::Finish { reason } => Some(*reason),
                _ => None,
            })
            .expect("a finish event");
        assert_eq!(finish, FinishReason::ContentFilter);
        assert!(!finish.is_complete());
    }

    #[test]
    fn a_blocked_prompt_reported_mid_stream_is_a_failure_item() {
        let items = drain(vec![json!({
            "promptFeedback": {"blockReason": "PROHIBITED_CONTENT"},
            "candidates": []
        })]);
        let error = items
            .into_iter()
            .find_map(Result::err)
            .expect("a failure item");
        assert!(matches!(error.kind(), ProviderErrorKind::ContentFilter));
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("PROHIBITED_CONTENT".to_owned())
        );
    }

    #[test]
    fn thought_slices_never_reach_the_reader() {
        let emitted = events(vec![
            slice(json!([{"text": "sto pensando", "thought": true}]), None),
            slice(json!([{"text": "ecco"}]), Some("STOP")),
        ]);
        let text: String = emitted
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "ecco");
    }

    #[test]
    fn a_second_finish_reason_never_produces_a_second_finish_event() {
        let emitted = events(vec![
            slice(json!([{"text": "a"}]), Some("STOP")),
            slice(json!([{"text": "b"}]), Some("STOP")),
        ]);
        let finishes = emitted
            .iter()
            .filter(|event| matches!(event, StreamEvent::Finish { .. }))
            .count();
        assert_eq!(finishes, 1);
    }
}
