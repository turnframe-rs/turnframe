//! Property tests for stream reassembly (spec §20.8, invariant I18).
//!
//! An adapter has no control over where a provider cuts its chunks: a tool
//! call's arguments arrive as `{"amo`, `unt": 12`, `00}` on one connection and
//! whole on the next. Reassembly must not care. These tests generate random
//! chunkings and random interleavings of the same logical answer and assert
//! the rebuilt [`ModelResponse`] is always the same one.
//!
//! That property is what lets the conformance suite compare a streamed answer
//! against a non-streamed one and treat a difference as an adapter defect
//! rather than as timing noise.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures::executor::block_on;
use proptest::prelude::*;
use serde_json::{Value, json};
use turnframe_provider::ids::{CallId, RequestId};
use turnframe_provider::prelude::*;
use turnframe_provider::request::{ContentPart, ToolCall};
use turnframe_provider::stream::reconstruct;

/// Splits `input` into chunks whose sizes cycle through `sizes`, always on
/// character boundaries so every chunk is valid UTF-8.
fn chunk(input: &str, sizes: &[usize]) -> Vec<String> {
    let characters: Vec<char> = input.chars().collect();
    let mut chunks = Vec::new();
    let mut cursor = 0;
    let mut index = 0;
    while cursor < characters.len() {
        let size = sizes
            .get(index % sizes.len().max(1))
            .copied()
            .unwrap_or(1)
            .max(1);
        let end = (cursor + size).min(characters.len());
        chunks.push(characters[cursor..end].iter().collect::<String>());
        cursor = end;
        index += 1;
    }
    chunks
}

/// The logical answer both halves of a property build from.
#[derive(Debug, Clone)]
struct Answer {
    text: String,
    calls: Vec<(CallId, String, Value)>,
}

impl Answer {
    /// The response a non-streaming call would have returned.
    fn expected(&self) -> ModelResponse {
        let mut response =
            ModelResponse::new(RequestId::nil(), "p", "m").with_finish(if self.calls.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolCalls
            });
        if !self.text.is_empty() {
            response = response.with_text(self.text.clone());
        }
        for (id, name, arguments) in &self.calls {
            response =
                response.with_tool_call(ToolCall::new(id.clone(), name.clone(), arguments.clone()));
        }
        response
    }

    /// Events with the text delivered first, then each call in turn.
    fn sequential_events(
        &self,
        text_sizes: &[usize],
        argument_sizes: &[usize],
    ) -> Vec<StreamEvent> {
        let mut events: Vec<StreamEvent> = chunk(&self.text, text_sizes)
            .into_iter()
            .map(StreamEvent::text)
            .collect();
        for (id, name, arguments) in &self.calls {
            events.push(StreamEvent::tool_call_start(id.clone(), name.clone()));
            for fragment in chunk(&arguments.to_string(), argument_sizes) {
                events.push(StreamEvent::tool_call_delta(id.clone(), fragment));
            }
            events.push(StreamEvent::tool_call_end(id.clone()));
        }
        events.push(StreamEvent::Finish {
            reason: self.expected().finish,
        });
        events
    }

    /// The same events, with every call announced up front and the fragments
    /// round-robined — the shape a provider that streams calls in parallel
    /// produces.
    fn interleaved_events(
        &self,
        text_sizes: &[usize],
        argument_sizes: &[usize],
    ) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        for (id, name, _) in &self.calls {
            events.push(StreamEvent::tool_call_start(id.clone(), name.clone()));
        }
        let text_chunks = chunk(&self.text, text_sizes);
        let argument_chunks: Vec<Vec<String>> = self
            .calls
            .iter()
            .map(|(_, _, arguments)| chunk(&arguments.to_string(), argument_sizes))
            .collect();
        let rounds = text_chunks
            .len()
            .max(argument_chunks.iter().map(Vec::len).max().unwrap_or(0));
        for round in 0..rounds {
            if let Some(text) = text_chunks.get(round) {
                events.push(StreamEvent::text(text.clone()));
            }
            for (index, (id, _, _)) in self.calls.iter().enumerate() {
                if let Some(fragment) = argument_chunks[index].get(round) {
                    events.push(StreamEvent::tool_call_delta(id.clone(), fragment.clone()));
                }
            }
        }
        for (id, _, _) in &self.calls {
            events.push(StreamEvent::tool_call_end(id.clone()));
        }
        events.push(StreamEvent::Finish {
            reason: self.expected().finish,
        });
        events
    }
}

/// Rebuilds a response from events, failing the property on any error.
fn rebuild(events: Vec<StreamEvent>) -> ModelResponse {
    block_on(reconstruct(
        ModelStream::from_events(events),
        StreamAccumulator::new(RequestId::nil(), "p", "m"),
    ))
    .expect("a well-formed event sequence must reassemble")
}

/// Text that survives a round trip through JSON and chunking.
fn text_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[a-zA-Z0-9 àèéìòù,.!?]{0,120}".prop_map(String::from),
    ]
}

fn calls_strategy() -> impl Strategy<Value = Vec<(CallId, String, Value)>> {
    proptest::collection::vec((0i64..10_000, "[a-z_]{1,12}"), 0..4).prop_map(|specs| {
        specs
            .into_iter()
            .enumerate()
            .map(|(index, (amount, key))| {
                (
                    CallId::new(format!("call_{index}")),
                    "plan".to_owned(),
                    json!({ key: amount, "index": index }),
                )
            })
            .collect()
    })
}

fn sizes_strategy() -> impl Strategy<Value = Vec<usize>> {
    proptest::collection::vec(1usize..7, 1..6)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// However the provider cuts the bytes, the answer is the same.
    #[test]
    fn reassembly_is_independent_of_chunking(
        text in text_strategy(),
        calls in calls_strategy(),
        text_sizes in sizes_strategy(),
        argument_sizes in sizes_strategy(),
    ) {
        let answer = Answer { text, calls };
        let rebuilt = rebuild(answer.sequential_events(&text_sizes, &argument_sizes));
        let expected = answer.expected();
        prop_assert_eq!(&rebuilt.content, &expected.content);
        prop_assert_eq!(rebuilt.finish, expected.finish);
        prop_assert_eq!(rebuilt.request_id, expected.request_id);
    }

    /// Interleaving the fragments of several calls changes nothing: fragments
    /// are joined per call id, not per arrival position.
    #[test]
    fn reassembly_is_independent_of_interleaving(
        text in text_strategy(),
        calls in calls_strategy(),
        text_sizes in sizes_strategy(),
        argument_sizes in sizes_strategy(),
    ) {
        let answer = Answer { text, calls };
        let sequential = rebuild(answer.sequential_events(&text_sizes, &argument_sizes));
        let interleaved = rebuild(answer.interleaved_events(&text_sizes, &argument_sizes));
        prop_assert_eq!(&sequential.content, &interleaved.content);
        prop_assert_eq!(&sequential.content, &answer.expected().content);
    }

    /// Chunking one byte at a time is the worst case, and must still work.
    #[test]
    fn single_character_chunks_reassemble(
        text in text_strategy(),
        calls in calls_strategy(),
    ) {
        let answer = Answer { text, calls };
        let rebuilt = rebuild(answer.sequential_events(&[1], &[1]));
        prop_assert_eq!(&rebuilt.content, &answer.expected().content);
    }

    /// A truncated stream never yields a usable answer, whatever was cut.
    #[test]
    fn a_stream_cut_short_never_yields_a_partial_answer(
        text in text_strategy(),
        calls in calls_strategy(),
        text_sizes in sizes_strategy(),
        argument_sizes in sizes_strategy(),
        cut in 0usize..64,
    ) {
        let answer = Answer { text, calls };
        let mut events = answer.sequential_events(&text_sizes, &argument_sizes);
        // Drop the finish event and anything after `cut` positions.
        let keep = cut.min(events.len().saturating_sub(1));
        events.truncate(keep);
        let outcome = block_on(reconstruct(
            ModelStream::from_events(events),
            StreamAccumulator::new(RequestId::nil(), "p", "m"),
        ));
        prop_assert!(outcome.is_err(), "a stream without a finish event must not reassemble");
    }
}

#[test]
fn the_fixture_builders_agree_with_a_hand_written_case() {
    let answer = Answer {
        text: "Ho preparato la modifica.".to_owned(),
        calls: vec![(
            CallId::new("call_0"),
            "plan".to_owned(),
            json!({"amount": 1200, "index": 0}),
        )],
    };
    let rebuilt = rebuild(answer.sequential_events(&[3], &[5]));
    assert_eq!(rebuilt.text(), "Ho preparato la modifica.");
    assert_eq!(rebuilt.tool_calls().len(), 1);
    assert_eq!(
        rebuilt.tool_calls()[0].arguments,
        json!({"amount": 1200, "index": 0})
    );
    assert!(matches!(rebuilt.content[0], ContentPart::Text { .. }));
    assert_eq!(rebuilt.finish, FinishReason::ToolCalls);
}

#[test]
fn chunking_preserves_every_character() {
    let input = "àbcdè fghì";
    for sizes in [vec![1], vec![2, 3], vec![7], vec![1, 1, 5]] {
        let joined: String = chunk(input, &sizes).concat();
        assert_eq!(joined, input, "sizes {sizes:?}");
    }
    assert!(chunk("", &[3]).is_empty());
}
