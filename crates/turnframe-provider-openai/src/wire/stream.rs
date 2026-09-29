//! Server-sent events → normalized stream events.
//!
//! The chat-completions stream is a sequence of `data:` lines, each a JSON
//! chunk, terminated by the literal `data: [DONE]`. Text arrives as
//! `delta.content` fragments; a tool call arrives as `delta.tool_calls`
//! fragments **identified by index**, with the id and the name usually — but
//! not always — on the first fragment for that index and the arguments spread
//! over the rest as a JSON string sliced at arbitrary points.
//!
//! [`StreamDecoder`] turns that into the vocabulary
//! [`StreamAccumulator`](turnframe_provider::stream::StreamAccumulator)
//! consumes, and the contract it must keep is strict: reassembling the events
//! must produce the *same* [`ModelResponse`](turnframe_provider::response::ModelResponse)
//! the non-streamed call returns for the same exchange (spec §20.8). Three
//! decisions follow from that:
//!
//! * a tool call is announced with [`StreamEvent::ToolCallStart`] only once its
//!   **name** is known, and argument fragments that arrived earlier are
//!   buffered and flushed immediately after — the accumulator rejects a
//!   fragment for a call it has not seen start, and rightly so;
//! * every open call is closed with [`StreamEvent::ToolCallEnd`] *before*
//!   [`StreamEvent::Finish`], and usage is emitted **after** the finish, which
//!   is the order the endpoint itself uses;
//! * a stream that ends without `[DONE]` and without a `finish_reason` emits no
//!   finish at all, so the accumulator reports it as a truncated stream. A
//!   silent `Stop` there would turn a dropped connection into a short answer.

use std::collections::VecDeque;
use std::pin::Pin;

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::stream::{Stream, StreamExt};
use serde::Deserialize;
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelRef};
use turnframe_provider::response::{FinishReason, ResponseWarning};
use turnframe_provider::secret::Redactor;
use turnframe_provider::stream::{ModelStream, StreamEvent, StreamItem};

use crate::error::{ApiError, ApiErrorEnvelope};
use crate::wire::response::{Usage, WireText, finish_reason};

/// The sentinel that closes a chat-completions stream.
pub(crate) const DONE_SENTINEL: &str = "[DONE]";

/// One streamed chunk.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ChatChunk {
    /// The response identifier, repeated on every chunk. Reported once, so the
    /// rebuilt response carries the same `raw_id` the whole answer does.
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub(crate) usage: Option<Usage>,
    /// Several gateways report a mid-stream failure as an error chunk rather
    /// than by closing the connection.
    #[serde(default)]
    pub(crate) error: Option<ApiError>,
}

/// One choice inside a chunk.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ChunkChoice {
    #[serde(default)]
    pub(crate) delta: Option<Delta>,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

/// The incremental part of a choice.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Delta {
    #[serde(default)]
    pub(crate) content: Option<WireText>,
    #[serde(default)]
    pub(crate) refusal: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<ToolCallDelta>,
}

/// One tool-call fragment, identified by its index within the response.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ToolCallDelta {
    #[serde(default)]
    pub(crate) index: usize,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) function: Option<FunctionDelta>,
}

/// The function half of a tool-call fragment.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FunctionDelta {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) arguments: Option<String>,
}

/// A tool call being assembled from fragments.
#[derive(Debug, Clone)]
struct OpenCall {
    index: usize,
    id: Option<String>,
    name: Option<String>,
    /// Argument bytes that arrived before the call could be announced.
    buffered: String,
    started: bool,
    closed: bool,
}

impl OpenCall {
    /// The id to announce: the endpoint's own, or one derived from the index.
    fn call_id(&self) -> CallId {
        match self.id.as_deref().filter(|id| !id.is_empty()) {
            Some(id) => CallId::new(id),
            None => CallId::new(format!("call_{}", self.index)),
        }
    }
}

/// Rebuilds normalized stream events from chat-completion chunks.
#[derive(Debug, Default)]
pub(crate) struct StreamDecoder {
    calls: Vec<OpenCall>,
    saw_refusal: bool,
    finished: bool,
    announced_id: bool,
}

impl StreamDecoder {
    /// A decoder for one stream.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Absorbs one chunk and returns the events it produced, in order.
    pub(crate) fn push(&mut self, chunk: &ChatChunk) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if !self.announced_id
            && let Some(id) = chunk.id.as_deref().filter(|id| !id.is_empty())
        {
            self.announced_id = true;
            events.push(StreamEvent::response_id(id));
        }
        if let Some(choice) = chunk.choices.first() {
            if let Some(delta) = &choice.delta {
                self.push_delta(delta, &mut events);
            }
            if let Some(reported) = choice.finish_reason.as_deref().filter(|r| !r.is_empty()) {
                self.close(Some(reported), &mut events);
            }
        }
        if let Some(usage) = &chunk.usage {
            events.push(StreamEvent::Usage {
                usage: usage.normalize(),
            });
        }
        events
    }

    /// Absorbs the incremental part of a choice.
    fn push_delta(&mut self, delta: &Delta, events: &mut Vec<StreamEvent>) {
        if let Some(refusal) = delta.refusal.as_deref().filter(|text| !text.is_empty()) {
            self.saw_refusal = true;
            events.push(StreamEvent::text(refusal));
        }
        if let Some(text) = delta.content.as_ref().map(WireText::flatten)
            && !text.is_empty()
        {
            events.push(StreamEvent::text(text));
        }
        for fragment in &delta.tool_calls {
            self.push_tool_fragment(fragment, events);
        }
    }

    /// Absorbs one tool-call fragment, announcing the call as soon as it can.
    fn push_tool_fragment(&mut self, fragment: &ToolCallDelta, events: &mut Vec<StreamEvent>) {
        let slot = match self
            .calls
            .iter()
            .position(|call| call.index == fragment.index)
        {
            Some(position) => position,
            None => {
                self.calls.push(OpenCall {
                    index: fragment.index,
                    id: None,
                    name: None,
                    buffered: String::new(),
                    started: false,
                    closed: false,
                });
                self.calls.len() - 1
            }
        };
        let arguments = fragment
            .function
            .as_ref()
            .and_then(|function| function.arguments.clone());
        let call = &mut self.calls[slot];
        if let Some(id) = fragment.id.as_deref().filter(|id| !id.is_empty()) {
            call.id = Some(id.to_owned());
        }
        if let Some(name) = fragment
            .function
            .as_ref()
            .and_then(|function| function.name.as_deref())
            .filter(|name| !name.is_empty())
        {
            call.name = Some(name.to_owned());
        }
        if call.started {
            // The call is announced; a fragment is a fragment.
            let id = call.call_id();
            if let Some(arguments) = arguments.filter(|arguments| !arguments.is_empty()) {
                events.push(StreamEvent::tool_call_delta(id, arguments));
            }
            return;
        }
        // Not announced yet: hold the bytes until the name arrives, then let
        // `announce` flush them as one delta after the start event.
        if let Some(arguments) = &arguments {
            call.buffered.push_str(arguments);
        }
        self.announce(slot, events);
    }

    /// Emits the start event once the call has a name, flushing whatever
    /// arguments arrived before it.
    fn announce(&mut self, slot: usize, events: &mut Vec<StreamEvent>) {
        let call = &mut self.calls[slot];
        if call.started || call.name.is_none() {
            return;
        }
        call.started = true;
        let id = call.call_id();
        let name = call.name.clone().unwrap_or_default();
        let buffered = std::mem::take(&mut call.buffered);
        events.push(StreamEvent::tool_call_start(id.clone(), name));
        if !buffered.is_empty() {
            events.push(StreamEvent::tool_call_delta(id, buffered));
        }
    }

    /// Closes every open call and emits the finish event.
    fn close(&mut self, reported: Option<&str>, events: &mut Vec<StreamEvent>) {
        if self.finished {
            return;
        }
        for slot in 0..self.calls.len() {
            // A call that never learned its name is announced now with an
            // empty one rather than dropped: losing a call the model made is
            // worse than reporting it unnamed, and the caller can see it.
            if !self.calls[slot].started {
                self.calls[slot].name.get_or_insert_with(String::new);
                self.announce(slot, events);
            }
            if !self.calls[slot].closed {
                self.calls[slot].closed = true;
                events.push(StreamEvent::tool_call_end(self.calls[slot].call_id()));
            }
        }
        // Warnings have no place in a stream; an unknown label is reported as
        // `Other`, which a structured stage refuses.
        let mut warnings = Vec::new();
        let reason = if self.saw_refusal {
            FinishReason::Refusal
        } else {
            finish_reason(reported, &mut warnings)
        };
        self.finished = true;
        events.push(StreamEvent::Finish { reason });
    }

    /// Ends the stream.
    ///
    /// `saw_done` says whether the endpoint sent its `[DONE]` sentinel. Only
    /// then may a missing `finish_reason` be read as a clean stop; otherwise
    /// nothing is emitted and the accumulator reports the truncation.
    pub(crate) fn end(&mut self, saw_done: bool) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if saw_done && !self.finished {
            self.close(None, &mut events);
        }
        events
    }
}

/// Reads an HTTP response body as a normalized stream.
///
/// Every failure inside the stream becomes an item, never a panic: a chunk that
/// is not JSON, an error chunk, or a connection that drops all end the stream
/// with a typed [`ProviderError`].
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
        // path reports exactly what the whole path reports instead of dropping
        // it on the floor.
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
    /// Queues events, labelling nothing with anything from the wire.
    fn queue(&mut self, events: Vec<StreamEvent>) {
        self.pending.extend(events.into_iter().map(Ok));
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
                let events = state.decoder.end(false);
                state.queue(events);
            }
            Some(Err(_transport)) => {
                state.fail(ProviderError::transport("stream_read_failed"));
            }
            Some(Ok(event)) => {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }
                if data == DONE_SENTINEL {
                    state.done = true;
                    let events = state.decoder.end(true);
                    state.queue(events);
                    continue;
                }
                match serde_json::from_str::<ChatChunk>(data) {
                    Ok(chunk) => {
                        if let Some(reported) = chunk.error.clone() {
                            let envelope = ApiErrorEnvelope {
                                error: Some(reported),
                                ..ApiErrorEnvelope::default()
                            };
                            let error = crate::error::classify(
                                500,
                                None,
                                &envelope,
                                state.redactor.as_ref(),
                            );
                            state.fail(error);
                            continue;
                        }
                        let events = state.decoder.push(&chunk);
                        state.queue(events);
                    }
                    Err(_) => state.fail(ProviderError::malformed("stream_chunk_not_json")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turnframe_provider::ids::RequestId;
    use turnframe_provider::response::TokenUsage;
    use turnframe_provider::stream::{StreamAccumulator, reconstruct};

    fn chunk(value: serde_json::Value) -> ChatChunk {
        serde_json::from_value(value).expect("decodes")
    }

    fn drain(chunks: Vec<serde_json::Value>, saw_done: bool) -> Vec<StreamEvent> {
        let mut decoder = StreamDecoder::new();
        let mut events = Vec::new();
        for value in chunks {
            events.extend(decoder.push(&chunk(value)));
        }
        events.extend(decoder.end(saw_done));
        events
    }

    #[test]
    fn text_deltas_become_text_events_and_the_finish_arrives_once() {
        let events = drain(
            vec![
                json!({"choices": [{"delta": {"role": "assistant", "content": "Ho "}}]}),
                json!({"choices": [{"delta": {"content": "preparato."}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
                json!({"choices": [], "usage": {"prompt_tokens": 42, "completion_tokens": 7}}),
            ],
            true,
        );
        assert_eq!(
            events,
            vec![
                StreamEvent::text("Ho "),
                StreamEvent::text("preparato."),
                StreamEvent::Finish {
                    reason: FinishReason::Stop
                },
                StreamEvent::Usage {
                    usage: TokenUsage::new(42, 7)
                },
            ]
        );
    }

    #[test]
    fn tool_fragments_are_reassembled_by_index() {
        let events = drain(
            vec![
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "id": "call_a", "type": "function",
                     "function": {"name": "read_case", "arguments": ""}}
                ]}}]}),
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "function": {"arguments": "{\"id\":"}}
                ]}}]}),
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "function": {"arguments": "\"c1\"}"}}
                ]}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
            ],
            true,
        );
        assert_eq!(
            events,
            vec![
                StreamEvent::tool_call_start("call_a", "read_case"),
                StreamEvent::tool_call_delta("call_a", "{\"id\":"),
                StreamEvent::tool_call_delta("call_a", "\"c1\"}"),
                StreamEvent::tool_call_end("call_a"),
                StreamEvent::Finish {
                    reason: FinishReason::ToolCalls
                },
            ]
        );
    }

    #[tokio::test]
    async fn two_parallel_calls_keep_their_own_fragments() {
        let events = drain(
            vec![
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "id": "a", "function": {"name": "first", "arguments": "{\"x\""}},
                    {"index": 1, "id": "b", "function": {"name": "second", "arguments": "{\"y\""}}
                ]}}]}),
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 1, "function": {"arguments": ":2}"}},
                    {"index": 0, "function": {"arguments": ":1}"}}
                ]}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
            ],
            true,
        );
        let response = reconstruct(
            ModelStream::from_events(events),
            StreamAccumulator::new(RequestId::nil(), "openai", "gpt-4o"),
        )
        .await
        .expect("reassembles");
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_str(), "a");
        assert_eq!(calls[0].arguments, json!({"x": 1}));
        assert_eq!(calls[1].id.as_str(), "b");
        assert_eq!(calls[1].arguments, json!({"y": 2}));
    }

    #[test]
    fn a_name_that_arrives_after_its_arguments_still_announces_first() {
        let events = drain(
            vec![
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "id": "late", "function": {"arguments": "{\"a\":1}"}}
                ]}}]}),
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 0, "function": {"name": "read_case"}}
                ]}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
            ],
            true,
        );
        assert_eq!(
            events,
            vec![
                StreamEvent::tool_call_start("late", "read_case"),
                StreamEvent::tool_call_delta("late", "{\"a\":1}"),
                StreamEvent::tool_call_end("late"),
                StreamEvent::Finish {
                    reason: FinishReason::ToolCalls
                },
            ]
        );
    }

    #[test]
    fn a_call_without_an_id_gets_one_derived_from_its_index() {
        let events = drain(
            vec![
                json!({"choices": [{"delta": {"tool_calls": [
                    {"index": 3, "function": {"name": "n", "arguments": "{}"}}
                ]}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
            ],
            true,
        );
        assert_eq!(events[0], StreamEvent::tool_call_start("call_3", "n"));
    }

    #[test]
    fn a_refusal_delta_overrides_the_finish_reason() {
        let events = drain(
            vec![
                json!({"choices": [{"delta": {"refusal": "I cannot help with that."}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            ],
            true,
        );
        assert_eq!(
            events,
            vec![
                StreamEvent::text("I cannot help with that."),
                StreamEvent::Finish {
                    reason: FinishReason::Refusal
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_stream_that_stops_without_a_finish_is_a_truncation() {
        let events = drain(
            vec![json!({"choices": [{"delta": {"content": "meta"}}]})],
            false,
        );
        assert_eq!(events, vec![StreamEvent::text("meta")]);
        let error = reconstruct(
            ModelStream::from_events(events),
            StreamAccumulator::new(RequestId::nil(), "openai", "gpt-4o"),
        )
        .await
        .expect_err("a truncated stream is not a short answer");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("stream_ended_without_finish".to_owned())
        );
    }

    #[test]
    fn a_done_sentinel_closes_a_stream_the_endpoint_never_finished() {
        let events = drain(
            vec![json!({"choices": [{"delta": {"content": "tutto"}}]})],
            true,
        );
        assert_eq!(
            events,
            vec![
                StreamEvent::text("tutto"),
                StreamEvent::Finish {
                    reason: FinishReason::Stop
                },
            ]
        );
    }

    #[test]
    fn a_second_finish_reason_is_ignored_rather_than_duplicated() {
        let events = drain(
            vec![
                json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            ],
            true,
        );
        assert_eq!(
            events,
            vec![StreamEvent::Finish {
                reason: FinishReason::Stop
            }]
        );
    }
}
