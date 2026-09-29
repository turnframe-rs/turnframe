//! Server-sent events → normalized stream events.
//!
//! A Messages stream is a sequence of named events, each carrying a JSON object
//! whose `type` repeats the name:
//!
//! ```text
//! message_start        the message envelope, with the prompt half of the usage
//! content_block_start  one block opens, at an index — text, tool_use, thinking
//! content_block_delta  more of that block: text_delta, or input_json_delta
//! content_block_stop   that block closes
//! message_delta        the stop reason, and the completion half of the usage
//! message_stop         the stream ends
//! ping                 keep-alive
//! error                the call failed mid-answer
//! ```
//!
//! [`StreamDecoder`] turns that into the vocabulary
//! [`StreamAccumulator`](turnframe_provider::stream::StreamAccumulator)
//! consumes, and the contract it must keep is strict: reassembling the events
//! must produce the *same*
//! [`ModelResponse`](turnframe_provider::response::ModelResponse) the
//! non-streamed call returns for the same exchange (spec §20.8). Four decisions
//! follow from that:
//!
//! * a tool call is announced from `content_block_start`, which already carries
//!   both its id and its name — so, unlike the chat-completions format, nothing
//!   has to be buffered while waiting for a name;
//! * `input_json_delta` fragments are forwarded verbatim and the accumulator
//!   concatenates them per call; a fragment is rarely valid JSON on its own and
//!   is never parsed here;
//! * usage is **merged** across `message_start` and `message_delta` and emitted
//!   once, just before the finish, because the two halves separately would not
//!   equal what the non-streamed call reports;
//! * a stream that stops without `message_delta` and without `message_stop`
//!   emits no finish at all, so the accumulator reports the truncation. A
//!   silent `Stop` there would turn a dropped connection into a short answer.
//!
//! A `thinking` block is opened, ignored and closed without producing a single
//! event, exactly as the non-streamed path drops it: reasoning is not prose the
//! user may be shown (spec §18.5).

use std::pin::Pin;

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::stream::{Stream, StreamExt};
use serde::Deserialize;
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelRef};
use turnframe_provider::response::ResponseWarning;
use turnframe_provider::secret::Redactor;
use turnframe_provider::stream::{ModelStream, StreamEvent, StreamItem};

use crate::error::{ApiError, ApiErrorEnvelope, classify_stream};
use crate::wire::response::{Usage, finish_reason};

/// One frame of a Messages stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum StreamFrame {
    MessageStart {
        message: StreamMessage,
    },
    ContentBlockStart {
        index: usize,
        content_block: StartBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: BlockDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        #[serde(default)]
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Option<Usage>,
    },
    MessageStop,
    Ping,
    Error {
        #[serde(default)]
        error: ApiError,
    },
    /// A frame type this adapter does not model. Ignored, never guessed at.
    #[serde(other)]
    Unknown,
}

/// The envelope `message_start` carries.
///
/// The message id and the prompt half of the usage are read. The id used to be
/// left to the caller to seed; it arrives here, on the first frame, so the
/// rebuilt response carries the same `raw_id` the whole answer does without
/// anyone having to know it in advance.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct StreamMessage {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) usage: Option<Usage>,
}

/// The block `content_block_start` opens.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum StartBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    /// `thinking`, `redacted_thinking`, a server-tool block: opened and ignored.
    #[serde(other)]
    Unsupported,
}

/// The increment `content_block_delta` carries.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum BlockDelta {
    TextDelta {
        #[serde(default)]
        text: String,
    },
    InputJsonDelta {
        #[serde(default)]
        partial_json: String,
    },
    /// `thinking_delta`, `signature_delta`, `citations_delta`: ignored.
    #[serde(other)]
    Unsupported,
}

/// The `delta` object of `message_delta`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct MessageDeltaBody {
    #[serde(default)]
    pub(crate) stop_reason: Option<String>,
}

/// What an open block turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockKind {
    /// Prose: its deltas become text events.
    Text,
    /// A tool call, already announced under this id.
    Tool { id: CallId, closed: bool },
    /// A block this adapter drops whole.
    Ignored,
}

/// One block the stream opened, kept in arrival order.
#[derive(Debug, Clone)]
struct BlockSlot {
    index: usize,
    kind: BlockKind,
}

/// Rebuilds normalized stream events from Messages frames.
#[derive(Debug, Default)]
pub(crate) struct StreamDecoder {
    blocks: Vec<BlockSlot>,
    tool_ordinal: usize,
    usage: Usage,
    finished: bool,
}

impl StreamDecoder {
    /// A decoder for one stream.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Absorbs one frame and returns the events it produced, in order.
    ///
    /// # Errors
    ///
    /// Returns [`Malformed`](turnframe_provider::error::ProviderErrorKind::Malformed)
    /// for a frame that cannot be placed: a delta or a stop for a block that
    /// never opened, a text delta on a tool block, or a tool block opened
    /// without a name — the same failure the non-streamed path reports for the
    /// same body.
    pub(crate) fn push(&mut self, frame: &StreamFrame) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = Vec::new();
        match frame {
            StreamFrame::MessageStart { message } => {
                if let Some(id) = message.id.as_deref().filter(|id| !id.is_empty()) {
                    events.push(StreamEvent::response_id(id));
                }
                if let Some(usage) = &message.usage {
                    self.usage.merge(usage);
                }
            }
            StreamFrame::ContentBlockStart {
                index,
                content_block,
            } => self.open_block(*index, content_block, &mut events)?,
            StreamFrame::ContentBlockDelta { index, delta } => {
                self.push_delta(*index, delta, &mut events)?;
            }
            StreamFrame::ContentBlockStop { index } => self.close_block(*index, &mut events)?,
            StreamFrame::MessageDelta { delta, usage } => {
                if let Some(usage) = usage {
                    self.usage.merge(usage);
                }
                self.close(delta.stop_reason.as_deref(), &mut events);
            }
            // The vendor's own terminator: a message that ends without a stop
            // reason ended cleanly.
            StreamFrame::MessageStop => self.close(None, &mut events),
            // An `error` frame is classified by the reader, which holds the
            // redactor; `ping` and anything unmodelled produce nothing.
            StreamFrame::Error { .. } | StreamFrame::Ping | StreamFrame::Unknown => {}
        }
        Ok(events)
    }

    /// Opens a block, announcing a tool call as soon as it is known.
    fn open_block(
        &mut self,
        index: usize,
        block: &StartBlock,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let kind = match block {
            StartBlock::Text { text } => {
                if !text.is_empty() {
                    events.push(StreamEvent::text(text));
                }
                BlockKind::Text
            }
            StartBlock::ToolUse { id, name } => {
                let Some(name) = name.as_deref().filter(|name| !name.is_empty()) else {
                    return Err(ProviderError::malformed("tool_use_without_name"));
                };
                // The same synthesis rule as the non-streamed path, so the two
                // agree on the id when the endpoint sends none.
                let call_id = match id.as_deref().filter(|id| !id.is_empty()) {
                    Some(id) => CallId::new(id),
                    None => CallId::new(format!("call_{}", self.tool_ordinal)),
                };
                self.tool_ordinal += 1;
                events.push(StreamEvent::tool_call_start(call_id.clone(), name));
                BlockKind::Tool {
                    id: call_id,
                    closed: false,
                }
            }
            StartBlock::Unsupported => BlockKind::Ignored,
        };
        match self.slot(index) {
            // A restarted index is the vendor contradicting itself; the
            // accumulator would reject the second announcement anyway.
            Some(_) => return Err(ProviderError::malformed("content_block_started_twice")),
            None => self.blocks.push(BlockSlot { index, kind }),
        }
        Ok(())
    }

    /// Routes one delta to the block it belongs to.
    fn push_delta(
        &mut self,
        index: usize,
        delta: &BlockDelta,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let Some(slot) = self.slot(index) else {
            return Err(ProviderError::malformed("delta_without_block"));
        };
        match (&self.blocks[slot].kind, delta) {
            (BlockKind::Ignored, _) | (_, BlockDelta::Unsupported) => {}
            (BlockKind::Text, BlockDelta::TextDelta { text }) => {
                if !text.is_empty() {
                    events.push(StreamEvent::text(text));
                }
            }
            (BlockKind::Tool { id, .. }, BlockDelta::InputJsonDelta { partial_json }) => {
                if !partial_json.is_empty() {
                    events.push(StreamEvent::tool_call_delta(id.clone(), partial_json));
                }
            }
            (BlockKind::Text, BlockDelta::InputJsonDelta { .. }) => {
                return Err(ProviderError::malformed("input_json_delta_on_text_block"));
            }
            (BlockKind::Tool { .. }, BlockDelta::TextDelta { .. }) => {
                return Err(ProviderError::malformed("text_delta_on_tool_block"));
            }
        }
        Ok(())
    }

    /// Closes one block, ending its tool call if it had one.
    fn close_block(
        &mut self,
        index: usize,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let Some(slot) = self.slot(index) else {
            return Err(ProviderError::malformed("block_stop_without_start"));
        };
        if let BlockKind::Tool { id, closed } = &mut self.blocks[slot].kind
            && !*closed
        {
            *closed = true;
            events.push(StreamEvent::tool_call_end(id.clone()));
        }
        Ok(())
    }

    /// Closes every open call, reports the merged usage and finishes.
    fn close(&mut self, reported: Option<&str>, events: &mut Vec<StreamEvent>) {
        if self.finished {
            return;
        }
        for slot in &mut self.blocks {
            if let BlockKind::Tool { id, closed } = &mut slot.kind
                && !*closed
            {
                *closed = true;
                events.push(StreamEvent::tool_call_end(id.clone()));
            }
        }
        events.push(StreamEvent::Usage {
            usage: self.usage.normalize(),
        });
        // Warnings have no place in a stream; an unmodelled label becomes
        // `Other`, which a structured stage refuses.
        let mut warnings = Vec::new();
        let reason = finish_reason(reported, &mut warnings);
        self.finished = true;
        events.push(StreamEvent::Finish { reason });
    }

    /// The position of the slot for `index`, when it opened.
    fn slot(&self, index: usize) -> Option<usize> {
        self.blocks.iter().position(|slot| slot.index == index)
    }
}

/// Reads an HTTP response body as a normalized stream.
///
/// Every failure inside the stream becomes an item, never a panic: a frame that
/// is not JSON, an `error` frame, a delta for a block that never opened, or a
/// connection that drops all end the stream with a typed [`ProviderError`].
pub(crate) fn model_stream(
    response: reqwest::Response,
    reference: ModelRef,
    redactor: Box<dyn Redactor>,
    warnings: Vec<ResponseWarning>,
) -> ModelStream {
    let state = SseState {
        events: Box::pin(response.bytes_stream().eventsource()),
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
    pending: std::collections::VecDeque<StreamItem>,
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

/// Produces the next item, reading as many frames as it takes to have one.
async fn next_item(mut state: SseState) -> Option<(StreamItem, SseState)> {
    loop {
        if let Some(item) = state.pending.pop_front() {
            return Some((item, state));
        }
        if state.done {
            return None;
        }
        match state.events.next().await {
            // The body ended. Whatever the decoder already emitted stands; if
            // no finish was among it, the accumulator reports the truncation.
            None => state.done = true,
            Some(Err(_transport)) => state.fail(ProviderError::transport("stream_read_failed")),
            Some(Ok(event)) => {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }
                match serde_json::from_str::<StreamFrame>(data) {
                    Ok(StreamFrame::Error { error }) => {
                        let envelope = ApiErrorEnvelope::of(error);
                        let failure = classify_stream(&envelope, state.redactor.as_ref());
                        state.fail(failure);
                    }
                    Ok(frame) => match state.decoder.push(&frame) {
                        Ok(events) => state.queue(events),
                        Err(error) => state.fail(error),
                    },
                    Err(_) => state.fail(ProviderError::malformed("stream_frame_not_json")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use turnframe_provider::ids::RequestId;
    use turnframe_provider::response::{FinishReason, TokenUsage};
    use turnframe_provider::stream::{StreamAccumulator, reconstruct};

    fn frame(value: Value) -> StreamFrame {
        serde_json::from_value(value).expect("decodes")
    }

    fn drain(frames: Vec<Value>) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut decoder = StreamDecoder::new();
        let mut events = Vec::new();
        for value in frames {
            events.extend(decoder.push(&frame(value))?);
        }
        Ok(events)
    }

    fn text_stream() -> Vec<Value> {
        vec![
            json!({"type": "message_start", "message": {
                "id": "msg_1", "type": "message", "role": "assistant",
                "content": [], "model": "claude-test", "stop_reason": null,
                "usage": {"input_tokens": 10, "output_tokens": 1,
                          "cache_creation_input_tokens": 5,
                          "cache_read_input_tokens": 30}
            }}),
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "text", "text": ""}}),
            json!({"type": "ping"}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "Ho preparato "}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "la modifica."}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta",
                   "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                   "usage": {"output_tokens": 7}}),
            json!({"type": "message_stop"}),
        ]
    }

    #[tokio::test]
    async fn text_deltas_reassemble_into_one_part_with_the_merged_usage() {
        let events = drain(text_stream()).expect("decodes");
        let response = reconstruct(
            ModelStream::from_events(events),
            StreamAccumulator::new(RequestId::nil(), "anthropic", "claude-test"),
        )
        .await
        .expect("reassembles");
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert_eq!(response.content.len(), 1);
        assert_eq!(response.finish, FinishReason::Stop);
        // 10 fresh + 5 written + 30 read on the prompt, 7 generated.
        assert_eq!(response.usage, TokenUsage::new(45, 7).with_cached_input(30));
    }

    #[tokio::test]
    async fn a_tool_call_is_announced_at_block_start_and_closed_before_the_finish() {
        let events = drain(vec![
            json!({"type": "message_start", "message": {"id": "msg_1",
                   "usage": {"input_tokens": 12, "output_tokens": 1}}}),
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "text", "text": "Certo."}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block":
                   {"type": "tool_use", "id": "toolu_1", "name": "plan", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1,
                   "delta": {"type": "input_json_delta", "partial_json": "{\"acts\""}}),
            json!({"type": "content_block_delta", "index": 1,
                   "delta": {"type": "input_json_delta", "partial_json": ": []}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                   "usage": {"output_tokens": 20}}),
            json!({"type": "message_stop"}),
        ])
        .expect("decodes");

        let kinds: Vec<&str> = events.iter().map(StreamEvent::kind).collect();
        assert_eq!(
            kinds,
            [
                // The message id leads the stream, so the rebuilt response
                // carries it without the caller seeding it.
                "response_id",
                "text_delta",
                "tool_call_start",
                "tool_call_delta",
                "tool_call_delta",
                "tool_call_end",
                "usage",
                "finish"
            ]
        );
        let response = reconstruct(
            ModelStream::from_events(events),
            StreamAccumulator::new(RequestId::nil(), "anthropic", "claude-test"),
        )
        .await
        .expect("reassembles");
        assert_eq!(response.text(), "Certo.");
        assert_eq!(response.tool_calls()[0].id.as_str(), "toolu_1");
        assert_eq!(response.tool_calls()[0].arguments, json!({"acts": []}));
        assert_eq!(response.finish, FinishReason::ToolCalls);
    }

    #[tokio::test]
    async fn a_no_argument_call_reassembles_into_an_empty_object() {
        let events = drain(vec![
            json!({"type": "content_block_start", "index": 0, "content_block":
                   {"type": "tool_use", "id": "toolu_9", "name": "ping", "input": {}}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
        ])
        .expect("decodes");
        let response = reconstruct(
            ModelStream::from_events(events),
            StreamAccumulator::new(RequestId::nil(), "anthropic", "claude-test"),
        )
        .await
        .expect("reassembles");
        assert_eq!(response.tool_calls()[0].arguments, json!({}));
    }

    #[test]
    fn a_thinking_block_produces_nothing_at_all() {
        let events = drain(vec![
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "thinking_delta", "thinking": "step one"}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "signature_delta", "signature": "sig"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1,
                   "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1,
                   "delta": {"type": "text_delta", "text": "answer"}}),
        ])
        .expect("decodes");
        assert_eq!(events, vec![StreamEvent::text("answer")]);
    }

    #[test]
    fn a_frame_that_cannot_be_placed_is_malformed() {
        let cases = [
            (
                vec![json!({"type": "content_block_delta", "index": 3,
                            "delta": {"type": "text_delta", "text": "x"}})],
                "delta_without_block",
            ),
            (
                vec![json!({"type": "content_block_stop", "index": 3})],
                "block_stop_without_start",
            ),
            (
                vec![
                    json!({"type": "content_block_start", "index": 0,
                           "content_block": {"type": "text", "text": ""}}),
                    json!({"type": "content_block_start", "index": 0,
                           "content_block": {"type": "text", "text": ""}}),
                ],
                "content_block_started_twice",
            ),
            (
                vec![json!({"type": "content_block_start", "index": 0,
                            "content_block": {"type": "tool_use", "id": "toolu_1"}})],
                "tool_use_without_name",
            ),
            (
                vec![
                    json!({"type": "content_block_start", "index": 0,
                           "content_block": {"type": "text", "text": ""}}),
                    json!({"type": "content_block_delta", "index": 0,
                           "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
                ],
                "input_json_delta_on_text_block",
            ),
            (
                vec![
                    json!({"type": "content_block_start", "index": 0, "content_block":
                           {"type": "tool_use", "id": "toolu_1", "name": "plan"}}),
                    json!({"type": "content_block_delta", "index": 0,
                           "delta": {"type": "text_delta", "text": "x"}}),
                ],
                "text_delta_on_tool_block",
            ),
        ];
        for (frames, expected) in cases {
            let error = drain(frames).expect_err("malformed");
            assert_eq!(
                error.code().map(|code| code.as_str().to_owned()),
                Some(expected.to_owned()),
                "{error}"
            );
            assert!(matches!(
                error.kind(),
                turnframe_provider::error::ProviderErrorKind::Malformed
            ));
        }
    }

    #[tokio::test]
    async fn a_stream_that_dies_before_the_finish_is_a_truncation() {
        let events = drain(vec![
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "text", "text": "meta"}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": " risp"}}),
        ])
        .expect("decodes");
        let error = reconstruct(
            ModelStream::from_events(events),
            StreamAccumulator::new(RequestId::nil(), "anthropic", "claude-test"),
        )
        .await
        .expect_err("a truncated stream is not a short answer");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("stream_ended_without_finish".to_owned())
        );
    }

    #[test]
    fn message_stop_closes_a_stream_that_never_reported_a_reason() {
        let events = drain(vec![
            json!({"type": "content_block_start", "index": 0, "content_block":
                   {"type": "tool_use", "id": "toolu_1", "name": "plan"}}),
            json!({"type": "message_stop"}),
        ])
        .expect("decodes");
        // The open call is closed before the finish, and the finish arrives.
        assert_eq!(
            events.iter().map(StreamEvent::kind).collect::<Vec<_>>(),
            ["tool_call_start", "tool_call_end", "usage", "finish"]
        );
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish {
                reason: FinishReason::Stop
            })
        );
    }

    #[test]
    fn only_one_finish_is_ever_emitted() {
        let events = drain(vec![
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            json!({"type": "message_stop"}),
            json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens"}}),
        ])
        .expect("decodes");
        let finishes = events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Finish { .. }))
            .count();
        assert_eq!(finishes, 1);
    }

    #[test]
    fn an_unmodelled_frame_is_ignored_rather_than_guessed_at() {
        let events = drain(vec![
            json!({"type": "message_limits", "limits": {"anything": 1}}),
            json!({"type": "ping"}),
        ])
        .expect("decodes");
        assert!(events.is_empty());
        assert!(matches!(
            frame(json!({"type": "message_limits"})),
            StreamFrame::Unknown
        ));
    }
}
