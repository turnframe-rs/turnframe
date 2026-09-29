//! Newline-delimited JSON → normalized stream events.
//!
//! Ollama does **not** stream server-sent events. `/api/chat` with
//! `"stream": true` answers with a body of newline-delimited JSON: one complete
//! chat object per line, the last of which carries `"done": true` and the token
//! counts. There is no `data:` prefix, no event framing and no `[DONE]`
//! sentinel — the `done` flag is the sentinel, and it arrives inside the
//! payload rather than beside it.
//!
//! Two pieces do the work:
//!
//! * [`LineDecoder`] turns a byte stream into whole lines. HTTP chunk
//!   boundaries fall wherever they like, so the tail of a chunk is routinely
//!   half a JSON object; it is buffered until its newline arrives. Lines are
//!   kept as bytes and decoded straight from bytes, so a multi-byte character
//!   split across two chunks cannot be corrupted on the way through.
//! * [`StreamDecoder`] turns those objects into the vocabulary
//!   [`StreamAccumulator`](turnframe_provider::stream::StreamAccumulator)
//!   consumes, and the contract it must keep is strict: reassembling the events
//!   must produce the *same*
//!   [`ModelResponse`](turnframe_provider::response::ModelResponse) the
//!   non-streamed call returns for the same exchange (spec §20.8).
//!
//! Three decisions follow from that contract:
//!
//! * a tool call arrives **whole** — Ollama parses it in the runner and emits
//!   the finished arguments object, so unlike the OpenAI format there are no
//!   argument fragments to buffer — and is therefore announced, filled and
//!   closed within one chunk;
//! * call ids are synthesized from the call's position across the whole
//!   stream, by the same
//!   [`synthesized_call_id`](super::response::synthesized_call_id) the whole
//!   path uses, so the two agree;
//! * a body that ends without a `"done": true` line emits no finish event at
//!   all, so the accumulator reports a truncated stream. A silent `Stop` there
//!   would turn a dropped connection into a short answer.

use std::collections::VecDeque;
use std::pin::Pin;

use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::ModelRef;
use turnframe_provider::response::ResponseWarning;
use turnframe_provider::secret::Redactor;
use turnframe_provider::stream::{ModelStream, StreamEvent, StreamItem};

use crate::error::classify_stream;
use crate::wire::response::{ChatResponse, finish_reason, synthesized_call_id, tool_arguments};

/// Splits a byte stream into newline-delimited records.
///
/// Complete lines come out of [`push`](Self::push); whatever is left over when
/// the body ends comes out of [`finish`](Self::finish), because a daemon that
/// closes without a trailing newline has still sent a whole last object.
#[derive(Debug, Default)]
pub(crate) struct LineDecoder {
    buffer: Vec<u8>,
}

impl LineDecoder {
    /// A decoder for one body.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Absorbs a chunk and returns every complete line it completed.
    ///
    /// Blank lines are dropped: some proxies pad the stream with them.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=position).collect();
            line.pop();
            trim_ascii_end(&mut line);
            if !line.is_empty() {
                lines.push(line);
            }
        }
        lines
    }

    /// Returns the trailing line, when the body ended without a newline.
    pub(crate) fn finish(&mut self) -> Option<Vec<u8>> {
        let mut line = std::mem::take(&mut self.buffer);
        trim_ascii_end(&mut line);
        if line.is_empty() { None } else { Some(line) }
    }
}

/// Drops trailing ASCII whitespace, `\r` included.
fn trim_ascii_end(line: &mut Vec<u8>) {
    while line.last().is_some_and(u8::is_ascii_whitespace) {
        line.pop();
    }
}

/// Rebuilds normalized stream events from `/api/chat` chunks.
#[derive(Debug, Default)]
pub(crate) struct StreamDecoder {
    next_call: usize,
    saw_tool_calls: bool,
    finished: bool,
}

impl StreamDecoder {
    /// A decoder for one stream.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Absorbs one chunk and returns the events it produced, in order.
    pub(crate) fn push(&mut self, chunk: &ChatResponse) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if let Some(message) = &chunk.message {
            if let Some(text) = message.content.as_deref().filter(|text| !text.is_empty()) {
                events.push(StreamEvent::text(text));
            }
            // A reasoning trace is not the answer; the whole path drops it too,
            // so dropping it here is what keeps the two paths equal.
            for call in &message.tool_calls {
                let id = synthesized_call_id(self.next_call);
                self.next_call += 1;
                self.saw_tool_calls = true;
                events.push(StreamEvent::tool_call_start(
                    id.clone(),
                    call.function.name.clone().unwrap_or_default(),
                ));
                events.push(StreamEvent::tool_call_delta(
                    id.clone(),
                    tool_arguments(&call.function).to_string(),
                ));
                events.push(StreamEvent::tool_call_end(id));
            }
        }
        if chunk.done && !self.finished {
            self.finished = true;
            let usage = chunk.usage();
            if !usage.is_unreported() {
                events.push(StreamEvent::Usage { usage });
            }
            // Warnings have no place in a stream; an unknown label is reported
            // as `Other`, which a structured stage refuses.
            let mut ignored = Vec::new();
            events.push(StreamEvent::Finish {
                reason: finish_reason(
                    chunk.done_reason.as_deref(),
                    self.saw_tool_calls,
                    &mut ignored,
                ),
            });
        }
        events
    }
}

/// Reads an HTTP response body as a normalized stream.
///
/// Every failure inside the stream becomes an item, never a panic: a line that
/// is not JSON, an error object inside a chunk, or a connection that drops all
/// end the stream with a typed [`ProviderError`].
pub(crate) fn model_stream(
    response: reqwest::Response,
    reference: ModelRef,
    redactor: Box<dyn Redactor>,
    warnings: Vec<ResponseWarning>,
) -> ModelStream {
    let state = NdjsonState {
        bytes: Box::pin(response.bytes_stream()),
        lines: LineDecoder::new(),
        decoder: StreamDecoder::new(),
        // What the request conversion gave up leads the stream, so the streamed
        // path reports exactly what the whole path reports. The daemon sends no
        // response identifier of its own on either path, so there is none to
        // carry.
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

/// Code attached when a newline-terminated line does not decode.
pub(crate) const MANGLED_LINE_CODE: &str = "stream_line_not_json";

/// Code attached when the body stopped in the middle of a frame.
pub(crate) const TRUNCATED_FRAME_CODE: &str = "stream_ended_mid_frame";

/// The raw body, as chunks of bytes.
type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// Everything the unfolded stream carries between polls.
struct NdjsonState {
    bytes: ByteStream,
    lines: LineDecoder,
    decoder: StreamDecoder,
    pending: VecDeque<StreamItem>,
    done: bool,
    reference: ModelRef,
    redactor: Box<dyn Redactor>,
}

impl NdjsonState {
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

    /// Decodes one line and queues whatever it produced.
    ///
    /// `malformed_code` names the failure a line that is not JSON produces: a
    /// newline-terminated line that will not decode is a line the daemon
    /// mangled, while a trailing fragment that will not decode is a body that
    /// stopped in the middle of a frame. Both are typed failures, and telling
    /// them apart is the difference between "the daemon sent nonsense" and
    /// "the connection died".
    fn absorb(&mut self, line: &[u8], malformed_code: &'static str) {
        match serde_json::from_slice::<ChatResponse>(line) {
            Err(_) => self.fail(ProviderError::malformed(malformed_code)),
            Ok(chunk) => {
                if let Some(reported) = &chunk.error {
                    let error = classify_stream(reported, self.redactor.as_ref());
                    self.fail(error);
                    return;
                }
                let events = self.decoder.push(&chunk);
                self.queue(events);
            }
        }
    }
}

/// Produces the next item, reading as many chunks as it takes to have one.
async fn next_item(mut state: NdjsonState) -> Option<(StreamItem, NdjsonState)> {
    loop {
        if let Some(item) = state.pending.pop_front() {
            return Some((item, state));
        }
        if state.done {
            return None;
        }
        match state.bytes.next().await {
            None => {
                state.done = true;
                // A daemon that closes without a trailing newline has still
                // sent a whole last object; anything else is a truncation the
                // accumulator will report.
                if let Some(line) = state.lines.finish() {
                    state.done = false;
                    state.absorb(&line, TRUNCATED_FRAME_CODE);
                    state.done = true;
                }
            }
            Some(Err(_transport)) => {
                state.fail(ProviderError::transport("stream_read_failed"));
            }
            Some(Ok(bytes)) => {
                for line in state.lines.push(&bytes) {
                    if state.done {
                        break;
                    }
                    state.absorb(&line, MANGLED_LINE_CODE);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use turnframe_provider::ids::{ModelKey, ProviderKey, RequestId};
    use turnframe_provider::response::{FinishReason, ModelResponse, TokenUsage};
    use turnframe_provider::stream::StreamAccumulator;

    use crate::wire::response::build_response;

    const MODEL: &str = "qwen3:8b";

    fn text(line: &str) -> String {
        line.to_owned()
    }

    fn lines(decoder: &mut LineDecoder, chunk: &str) -> Vec<String> {
        decoder
            .push(chunk.as_bytes())
            .into_iter()
            .map(|line| String::from_utf8(line).expect("valid utf-8"))
            .collect()
    }

    #[test]
    fn the_decoder_buffers_a_partial_trailing_line_until_its_newline_arrives() {
        let mut decoder = LineDecoder::new();
        // The tail of an HTTP chunk is routinely half an object.
        assert!(lines(&mut decoder, "{\"done\":fal").is_empty());
        assert!(lines(&mut decoder, "se,\"a\":1").is_empty());
        assert_eq!(
            lines(&mut decoder, "}\n"),
            vec![text("{\"done\":false,\"a\":1}")]
        );
        assert!(decoder.finish().is_none());
    }

    #[test]
    fn the_decoder_returns_a_last_line_that_never_got_its_newline() {
        let mut decoder = LineDecoder::new();
        assert_eq!(
            lines(&mut decoder, "{\"a\":1}\n{\"b\":2}"),
            vec![text("{\"a\":1}")]
        );
        let trailing = decoder.finish().expect("a whole last object");
        assert_eq!(String::from_utf8(trailing).expect("utf-8"), "{\"b\":2}");
        // Draining it leaves nothing behind.
        assert!(decoder.finish().is_none());
    }

    #[test]
    fn the_decoder_survives_crlf_blank_lines_and_a_split_multibyte_character() {
        let mut decoder = LineDecoder::new();
        assert_eq!(
            lines(&mut decoder, "{\"a\":1}\r\n\n"),
            vec![text("{\"a\":1}")]
        );
        // "è" is two bytes; the chunk boundary falls between them.
        let word = "{\"t\":\"perch\u{00e8}\"}\n".as_bytes();
        let split = 10;
        assert!(decoder.push(&word[..split]).is_empty());
        let completed = decoder.push(&word[split..]);
        assert_eq!(
            String::from_utf8(completed[0].clone()).expect("utf-8"),
            "{\"t\":\"perch\u{00e8}\"}"
        );
    }

    /// Turns a body of newline-delimited frames into the events it produces.
    fn events_of(body: &str) -> Vec<StreamEvent> {
        let mut lines = LineDecoder::new();
        let mut decoder = StreamDecoder::new();
        let mut events = Vec::new();
        for line in lines.push(body.as_bytes()) {
            let chunk: ChatResponse = serde_json::from_slice(&line).expect("a frame");
            events.extend(decoder.push(&chunk));
        }
        if let Some(line) = lines.finish() {
            let chunk: ChatResponse = serde_json::from_slice(&line).expect("a frame");
            events.extend(decoder.push(&chunk));
        }
        events
    }

    /// The frames a daemon sends for `fragments`, ending with the done frame.
    fn narration_body(fragments: &[&str]) -> String {
        let mut body = String::new();
        for fragment in fragments {
            body.push_str(
                &json!({
                    "model": MODEL,
                    "created_at": "2026-09-05T10:00:00.000000Z",
                    "message": {"role": "assistant", "content": fragment},
                    "done": false
                })
                .to_string(),
            );
            body.push('\n');
        }
        body.push_str(
            &json!({
                "model": MODEL,
                "created_at": "2026-09-05T10:00:01.000000Z",
                "message": {"role": "assistant", "content": ""},
                "done": true,
                "done_reason": "stop",
                "prompt_eval_count": 42,
                "eval_count": 7
            })
            .to_string(),
        );
        body.push('\n');
        body
    }

    #[test]
    fn several_frames_produce_several_deltas_not_one_buffered_answer() {
        let events = events_of(&narration_body(&["Ho ", "preparato ", "la modifica."]));
        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        // The point of streaming: three frames, three deltas. An adapter that
        // buffered the answer and emitted it at the end would produce one here
        // and still reassemble correctly, which is why this is asserted.
        assert_eq!(deltas, vec!["Ho ", "preparato ", "la modifica."]);
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish {
                reason: FinishReason::Stop
            })
        );
    }

    #[test]
    fn the_empty_content_of_the_done_frame_produces_no_delta() {
        let events = events_of(&narration_body(&["solo"]));
        let deltas = events
            .iter()
            .filter(|event| matches!(event, StreamEvent::TextDelta { .. }))
            .count();
        assert_eq!(deltas, 1);
    }

    /// Rebuilds a response from the events of `body`.
    fn reconstructed(body: &str) -> Result<ModelResponse, ProviderError> {
        let mut seed = StreamAccumulator::new(RequestId::nil(), "ollama", MODEL);
        for event in events_of(body) {
            seed.push(event)?;
        }
        seed.finish()
    }

    /// The response the whole path builds for the same answer.
    fn whole(body: Value) -> ModelResponse {
        let decoded: ChatResponse = serde_json::from_value(body).expect("an envelope");
        build_response(
            &decoded,
            RequestId::nil(),
            &ProviderKey::from("ollama"),
            &ModelKey::from(MODEL),
        )
        .expect("builds")
    }

    #[test]
    fn the_streamed_answer_equals_the_whole_one_content_finish_and_usage() {
        let rebuilt = reconstructed(&narration_body(&["Ho preparato ", "la modifica."]))
            .expect("reassembles");
        let whole = whole(json!({
            "model": MODEL,
            "message": {"role": "assistant", "content": "Ho preparato la modifica."},
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 42,
            "eval_count": 7
        }));
        assert_eq!(rebuilt.content, whole.content);
        assert_eq!(rebuilt.finish, whole.finish);
        // The counts ride on the final frame; if that frame were dropped the
        // streamed path would silently report nothing.
        assert_eq!(rebuilt.usage, whole.usage);
        assert_eq!(rebuilt.usage, TokenUsage::new(42, 7));
        assert_eq!(rebuilt.usage.cached_input, 0);
    }

    #[test]
    fn a_streamed_tool_call_equals_the_whole_one() {
        let mut body = json!({
            "model": MODEL,
            "message": {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "load_case", "arguments": {"target": "tok_1"}}}
            ]},
            "done": false
        })
        .to_string();
        body.push('\n');
        body.push_str(
            &json!({
                "model": MODEL,
                "message": {"role": "assistant", "content": ""},
                "done": true, "done_reason": "stop",
                "prompt_eval_count": 12, "eval_count": 4
            })
            .to_string(),
        );
        body.push('\n');

        let rebuilt = reconstructed(&body).expect("reassembles");
        let whole = whole(json!({
            "model": MODEL,
            "message": {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "load_case", "arguments": {"target": "tok_1"}}}
            ]},
            "done": true, "done_reason": "stop",
            "prompt_eval_count": 12, "eval_count": 4
        }));
        assert_eq!(rebuilt.content, whole.content);
        assert_eq!(rebuilt.finish, whole.finish);
        assert_eq!(rebuilt.finish, FinishReason::ToolCalls);
        assert_eq!(rebuilt.tool_calls()[0].id.as_str(), "call_0");
        assert_eq!(
            rebuilt.tool_calls()[0].arguments,
            json!({"target": "tok_1"})
        );
        assert_eq!(rebuilt.usage, whole.usage);
    }

    #[test]
    fn a_body_that_stops_before_the_done_frame_is_a_truncation_not_a_short_answer() {
        let mut body = json!({
            "model": MODEL,
            "message": {"role": "assistant", "content": "meta "},
            "done": false
        })
        .to_string();
        body.push('\n');
        let error = reconstructed(&body).expect_err("no finish arrived");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("stream_ended_without_finish".to_owned())
        );
    }
}
