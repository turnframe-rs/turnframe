//! `ConverseStream` events → normalized stream events.
//!
//! A Converse stream is a sequence of typed event-stream frames the SDK has
//! already decoded:
//!
//! ```text
//! messageStart       the turn opens, with the assistant role
//! contentBlockStart  a block opens at an index — only tool use announces itself
//! contentBlockDelta  more of that block: a text fragment, or tool input bytes
//! contentBlockStop   that block closes
//! messageStop        the stop reason
//! metadata           the token usage and latency metrics, last
//! ```
//!
//! [`StreamDecoder`] turns that into the vocabulary
//! [`StreamAccumulator`](turnframe_provider::stream::StreamAccumulator)
//! consumes, and the contract it must keep is strict: reassembling the events
//! must produce the *same*
//! [`ModelResponse`](turnframe_provider::response::ModelResponse) the
//! non-streamed call returns for the same exchange (spec §20.8).
//!
//! Four decisions follow from that contract:
//!
//! * **Every fragment is forwarded as it arrives.** Nothing is buffered to be
//!   flushed at the end: a stream whose whole point is that prose appears while
//!   it is written must not be re-assembled inside the adapter. Three wire
//!   deltas produce three [`StreamEvent::TextDelta`]s.
//! * **A text block opens implicitly.** Converse announces a `contentBlockStart`
//!   for tool use and not for text, so the first text delta at an index opens
//!   that index. A tool-input delta at an index that never announced itself is a
//!   [`Malformed`](turnframe_provider::error::ProviderErrorKind::Malformed)
//!   failure, because its id would have to be invented.
//! * **Tool input fragments travel verbatim.** They are rarely valid JSON on
//!   their own and are never parsed here; the accumulator concatenates them per
//!   call and parses once.
//! * **Usage arrives after the finish, and is still reported.** Converse sends
//!   `metadata` last, so the [`Usage`](StreamEvent::Usage) event follows
//!   [`Finish`](StreamEvent::Finish) — which the accumulator accepts — and the
//!   reassembled response carries the same counts the non-streamed call
//!   reports. A stream that ends without `messageStop` emits no finish at all,
//!   so the accumulator reports the truncation instead of inventing a `Stop`.

use aws_sdk_bedrockruntime::operation::RequestId as _;
use aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput as StreamResponse;
use aws_sdk_bedrockruntime::types::{ContentBlockDelta, ContentBlockStart, ConverseStreamOutput};
use std::collections::VecDeque;
use tokio::time::Instant;
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelRef};
use turnframe_provider::response::ResponseWarning;
use turnframe_provider::secret::Redactor;
use turnframe_provider::stream::{ModelStream, StreamEvent, StreamItem};

use crate::error::classify_stream;
use crate::wire::response::{finish_reason, normalize_usage};

/// What one content-block index is carrying.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockKind {
    /// Prose.
    Text,
    /// A tool call being assembled.
    Tool { id: CallId, closed: bool },
    /// A block this adapter does not forward — an image, a server-side tool
    /// result. Opened, ignored and closed without producing an event.
    Ignored,
}

/// One open content block.
#[derive(Debug, Clone)]
struct Slot {
    index: i32,
    kind: BlockKind,
}

/// Turns Converse stream events into normalized ones.
#[derive(Debug, Default)]
pub(crate) struct StreamDecoder {
    blocks: Vec<Slot>,
    finished: bool,
}

impl StreamDecoder {
    /// A decoder with no open block.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Absorbs one Converse event and returns what it produces.
    ///
    /// # Errors
    ///
    /// Returns [`Malformed`](turnframe_provider::error::ProviderErrorKind::Malformed)
    /// for a tool-input fragment at an index that never announced a call.
    pub(crate) fn push(
        &mut self,
        event: &ConverseStreamOutput,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = Vec::new();
        match event {
            // The role is already known: the answer is the assistant's.
            ConverseStreamOutput::MessageStart(_) => {}
            ConverseStreamOutput::ContentBlockStart(start) => {
                self.open_block(start.content_block_index, start.start.as_ref(), &mut events);
            }
            ConverseStreamOutput::ContentBlockDelta(delta) => {
                self.push_delta(delta.content_block_index, delta.delta.as_ref(), &mut events)?;
            }
            ConverseStreamOutput::ContentBlockStop(stop) => {
                self.close_block(stop.content_block_index, &mut events);
            }
            ConverseStreamOutput::MessageStop(stop) => {
                self.close(&stop.stop_reason, &mut events);
            }
            ConverseStreamOutput::Metadata(metadata) => {
                if let Some(usage) = metadata.usage.as_ref() {
                    events.push(StreamEvent::Usage {
                        usage: normalize_usage(usage),
                    });
                }
            }
            // An event Converse grew after this adapter was written: ignored,
            // never guessed at.
            _ => {}
        }
        Ok(events)
    }

    /// Opens a block at `index`, announcing a tool call when that is what it is.
    fn open_block(
        &mut self,
        index: i32,
        start: Option<&ContentBlockStart>,
        events: &mut Vec<StreamEvent>,
    ) {
        let kind = match start {
            Some(ContentBlockStart::ToolUse(tool)) => {
                let id = CallId::new(&tool.tool_use_id);
                events.push(StreamEvent::tool_call_start(id.clone(), &tool.name));
                BlockKind::Tool { id, closed: false }
            }
            _ => BlockKind::Ignored,
        };
        self.replace(index, kind);
    }

    /// Routes one delta to the block it belongs to, opening a text block that
    /// never announced itself.
    fn push_delta(
        &mut self,
        index: i32,
        delta: Option<&ContentBlockDelta>,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        match delta {
            Some(ContentBlockDelta::Text(text)) => {
                if self.slot(index).is_none() {
                    // Converse announces tool use and not text: the first
                    // fragment is the block's start.
                    self.replace(index, BlockKind::Text);
                }
                if matches!(self.kind(index), Some(BlockKind::Text)) && !text.is_empty() {
                    events.push(StreamEvent::text(text));
                }
                Ok(())
            }
            Some(ContentBlockDelta::ToolUse(fragment)) => {
                let Some(BlockKind::Tool { id, closed }) = self.kind(index).cloned() else {
                    return Err(ProviderError::malformed("tool_use_delta_without_start"));
                };
                if closed {
                    return Err(ProviderError::malformed("tool_use_delta_after_stop"));
                }
                if !fragment.input.is_empty() {
                    events.push(StreamEvent::tool_call_delta(id, &fragment.input));
                }
                Ok(())
            }
            // A reasoning fragment, a citation, an image chunk: not prose the
            // user may be shown (spec §18.5), and not part of the answer.
            _ => Ok(()),
        }
    }

    /// Closes one block, ending its tool call if it had one.
    ///
    /// A stop for an index that produced nothing is not an error: Converse
    /// closes an empty block the same way it closes a full one.
    fn close_block(&mut self, index: i32, events: &mut Vec<StreamEvent>) {
        if let Some(BlockKind::Tool { id, closed }) = self.kind_mut(index)
            && !*closed
        {
            *closed = true;
            events.push(StreamEvent::tool_call_end(id.clone()));
        }
    }

    /// Closes every open call and finishes.
    fn close(
        &mut self,
        reported: &aws_sdk_bedrockruntime::types::StopReason,
        events: &mut Vec<StreamEvent>,
    ) {
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
        // Warnings have no place in a stream; an unmodelled label becomes
        // `Other`, which a structured stage refuses.
        let mut warnings = Vec::new();
        self.finished = true;
        events.push(StreamEvent::Finish {
            reason: finish_reason(reported, &mut warnings),
        });
    }

    /// Records the kind of the block at `index`, replacing any earlier one.
    fn replace(&mut self, index: i32, kind: BlockKind) {
        match self.blocks.iter_mut().find(|slot| slot.index == index) {
            Some(slot) => slot.kind = kind,
            None => self.blocks.push(Slot { index, kind }),
        }
    }

    /// The slot for `index`, when it opened.
    fn slot(&self, index: i32) -> Option<&Slot> {
        self.blocks.iter().find(|slot| slot.index == index)
    }

    /// The kind of the block at `index`.
    fn kind(&self, index: i32) -> Option<&BlockKind> {
        self.slot(index).map(|slot| &slot.kind)
    }

    /// The kind of the block at `index`, mutably.
    fn kind_mut(&mut self, index: i32) -> Option<&mut BlockKind> {
        self.blocks
            .iter_mut()
            .find(|slot| slot.index == index)
            .map(|slot| &mut slot.kind)
    }
}

/// Everything the unfolded stream carries between polls.
struct StreamState {
    output: StreamResponse,
    decoder: StreamDecoder,
    pending: VecDeque<StreamItem>,
    done: bool,
    reference: ModelRef,
    redactor: Box<dyn Redactor>,
    deadline: Instant,
}

impl StreamState {
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

/// Reads a `ConverseStream` response as a normalized stream.
///
/// Every failure inside the stream becomes an item, never a panic: a modelled
/// exception frame, a frame the SDK could not decode, a connection that drops,
/// or the call's own deadline passing all end the stream with a typed
/// [`ProviderError`].
pub(crate) fn model_stream(
    output: StreamResponse,
    reference: ModelRef,
    redactor: Box<dyn Redactor>,
    deadline: Instant,
    warnings: Vec<ResponseWarning>,
) -> ModelStream {
    // Two things the frames never carry lead the stream: the AWS request id,
    // which the operation output holds and the whole path already reports, and
    // whatever the request conversion had to give up. Without them the streamed
    // path was quieter than the whole path about the very same call.
    let mut pending: VecDeque<StreamItem> = warnings
        .into_iter()
        .map(|warning| Ok(StreamEvent::warning(warning)))
        .collect();
    if let Some(id) = output.request_id().filter(|id| !id.is_empty()) {
        pending.push_front(Ok(StreamEvent::response_id(id)));
    }
    let state = StreamState {
        output,
        decoder: StreamDecoder::new(),
        pending,
        done: false,
        reference,
        redactor,
        deadline,
    };
    ModelStream::new(futures::stream::unfold(state, next_item))
}

/// Produces the next item, reading as many frames as it takes to have one.
async fn next_item(mut state: StreamState) -> Option<(StreamItem, StreamState)> {
    loop {
        if let Some(item) = state.pending.pop_front() {
            return Some((item, state));
        }
        if state.done {
            return None;
        }
        let received = tokio::time::timeout_at(state.deadline, state.output.stream.recv()).await;
        let received = match received {
            Ok(received) => received,
            Err(_elapsed) => {
                // The request's deadline covers reading the body too
                // (`ModelRequest::timeout`), so a stream that stalls past it
                // ends as a timeout rather than hanging the turn.
                state.fail(ProviderError::timeout());
                continue;
            }
        };
        match received {
            // The body ended. Whatever the decoder already emitted stands; if
            // no finish was among it, the accumulator reports the truncation.
            Ok(None) => state.done = true,
            Ok(Some(event)) => match state.decoder.push(&event) {
                Ok(events) => state.queue(events),
                Err(error) => state.fail(error),
            },
            Err(error) => {
                let failure = classify_stream(&error, state.redactor.as_ref());
                state.fail(failure);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_bedrockruntime::types::{
        ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
        ConverseStreamMetadataEvent, MessageStopEvent, StopReason, ToolUseBlockDelta,
        ToolUseBlockStart,
    };
    use turnframe_provider::response::FinishReason;

    fn text_delta(index: i32, text: &str) -> ConverseStreamOutput {
        ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .delta(ContentBlockDelta::Text(text.to_owned()))
                .content_block_index(index)
                .build()
                .expect("the index is set"),
        )
    }

    fn tool_start(index: i32, id: &str, name: &str) -> ConverseStreamOutput {
        ConverseStreamOutput::ContentBlockStart(
            ContentBlockStartEvent::builder()
                .start(ContentBlockStart::ToolUse(
                    ToolUseBlockStart::builder()
                        .tool_use_id(id)
                        .name(name)
                        .build()
                        .expect("both fields are set"),
                ))
                .content_block_index(index)
                .build()
                .expect("the index is set"),
        )
    }

    fn tool_delta(index: i32, fragment: &str) -> ConverseStreamOutput {
        ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .delta(ContentBlockDelta::ToolUse(
                    ToolUseBlockDelta::builder()
                        .input(fragment)
                        .build()
                        .expect("the input is set"),
                ))
                .content_block_index(index)
                .build()
                .expect("the index is set"),
        )
    }

    fn block_stop(index: i32) -> ConverseStreamOutput {
        ConverseStreamOutput::ContentBlockStop(
            ContentBlockStopEvent::builder()
                .content_block_index(index)
                .build()
                .expect("the index is set"),
        )
    }

    fn message_stop(reason: StopReason) -> ConverseStreamOutput {
        ConverseStreamOutput::MessageStop(
            MessageStopEvent::builder()
                .stop_reason(reason)
                .build()
                .expect("the reason is set"),
        )
    }

    fn metadata(input: i32, output: i32) -> ConverseStreamOutput {
        ConverseStreamOutput::Metadata(
            ConverseStreamMetadataEvent::builder()
                .usage(
                    aws_sdk_bedrockruntime::types::TokenUsage::builder()
                        .input_tokens(input)
                        .output_tokens(output)
                        .total_tokens(input + output)
                        .build()
                        .expect("the counters are set"),
                )
                .build(),
        )
    }

    fn drain(events: &[ConverseStreamOutput]) -> Vec<StreamEvent> {
        let mut decoder = StreamDecoder::new();
        let mut out = Vec::new();
        for event in events {
            out.extend(decoder.push(event).expect("a well-formed stream"));
        }
        out
    }

    #[test]
    fn every_wire_fragment_produces_its_own_delta() {
        let events = drain(&[
            text_delta(0, "Ho "),
            text_delta(0, "preparato "),
            text_delta(0, "la modifica."),
            block_stop(0),
            message_stop(StopReason::EndTurn),
        ]);
        let deltas: Vec<&StreamEvent> = events
            .iter()
            .filter(|event| matches!(event, StreamEvent::TextDelta { .. }))
            .collect();
        assert_eq!(
            deltas.len(),
            3,
            "prose must reach the caller as it arrives, not in one final chunk"
        );
        assert_eq!(deltas[0], &StreamEvent::text("Ho "));
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Finish {
                reason: FinishReason::Stop
            })
        ));
    }

    #[test]
    fn a_tool_call_is_announced_fragmented_and_closed() {
        let events = drain(&[
            tool_start(1, "tooluse_1", "read"),
            tool_delta(1, "{\"target\":"),
            tool_delta(1, "\"tok_1\"}"),
            block_stop(1),
            message_stop(StopReason::ToolUse),
        ]);
        assert_eq!(events[0], StreamEvent::tool_call_start("tooluse_1", "read"));
        assert_eq!(
            events[1],
            StreamEvent::tool_call_delta("tooluse_1", "{\"target\":")
        );
        assert_eq!(events[3], StreamEvent::tool_call_end("tooluse_1"));
    }

    #[test]
    fn an_open_call_is_closed_by_the_message_stop() {
        let events = drain(&[
            tool_start(0, "tooluse_1", "read"),
            tool_delta(0, "{}"),
            message_stop(StopReason::ToolUse),
        ]);
        assert_eq!(events[2], StreamEvent::tool_call_end("tooluse_1"));
    }

    #[test]
    fn usage_follows_the_finish_and_is_still_reported() {
        let events = drain(&[
            text_delta(0, "ok"),
            message_stop(StopReason::EndTurn),
            metadata(42, 7),
        ]);
        assert!(matches!(events[1], StreamEvent::Finish { .. }));
        let StreamEvent::Usage { usage } = &events[2] else {
            panic!("the metadata event carries the counts");
        };
        assert_eq!(usage.input, 42);
        assert_eq!(usage.output, 7);
    }

    #[test]
    fn a_tool_fragment_for_a_call_that_never_started_is_malformed() {
        let mut decoder = StreamDecoder::new();
        let error = decoder
            .push(&tool_delta(3, "{}"))
            .expect_err("an unannounced call has no id to attach to");
        assert_eq!(error.kind().as_str(), "malformed");
    }

    #[test]
    fn a_stop_for_a_block_that_produced_nothing_is_not_an_error() {
        let events = drain(&[block_stop(7), message_stop(StopReason::EndTurn)]);
        assert_eq!(events.len(), 1);
    }
}
