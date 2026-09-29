//! Stream reassembly over a realistically chunked answer (spec §28).
//!
//! Reassembly is a contract, not a convenience: the conformance suite asserts
//! that a reconstructed stream equals the answer `generate` would have
//! returned, which is what lets the runtime stream one purpose and not another
//! without maintaining two parsers. That contract is paid for once per streamed
//! token, so its cost belongs in the same table as projection and persistence.
//!
//! The chunking is the point. Providers do not send one delta per sentence;
//! they send a few characters at a time, and tool-call arguments arrive as
//! fragments that are not valid JSON until the last one lands. A benchmark
//! that fed the accumulator three fat deltas would measure nothing that
//! happens in production, so these fixtures split by grapheme-sized pieces and
//! interleave two tool calls.
//!
//! Two rows:
//!
//! * `accumulator` drives [`StreamAccumulator`] directly, which is the
//!   synchronous work — the concatenation, the per-call fragment buffers and
//!   the final JSON parse;
//! * `reconstruct` adds the stream plumbing on top, so the difference is what
//!   the async wrapper costs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use turnframe_provider::ids::RequestId;
use turnframe_provider::response::FinishReason;
use turnframe_provider::stream::{ModelStream, StreamAccumulator, StreamEvent, reconstruct};

/// The prose a narration turn streams, repeated to the requested length.
const NARRATION: &str = "Ho spostato il viaggio al 30 novembre e ho messo i bagagli a carico \
    della compagnia aerea. Il cambio non è stato inviato: hai chiesto di non inviare nulla, \
    quindi la pratica resta aperta. ";

/// Argument JSON one tool call streams in fragments.
const ARGUMENTS: &str = r#"{"extra":"extra_0007","payer":"airline","note":"settimana 7"}"#;

/// A stream of `chars_per_delta`-sized text deltas covering `prose`, then two
/// tool calls whose arguments arrive in eight-byte fragments, then a finish.
fn events(prose: &str, chars_per_delta: usize, tool_calls: usize) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    let mut buffer = String::new();
    for character in prose.chars() {
        buffer.push(character);
        if buffer.chars().count() >= chars_per_delta {
            events.push(StreamEvent::text(std::mem::take(&mut buffer)));
        }
    }
    if !buffer.is_empty() {
        events.push(StreamEvent::text(buffer));
    }
    for index in 0..tool_calls {
        let id = format!("call_{index:04}");
        events.push(StreamEvent::tool_call_start(id.clone(), "trip.read_case"));
        for fragment in ARGUMENTS.as_bytes().chunks(8) {
            events.push(StreamEvent::tool_call_delta(
                id.clone(),
                String::from_utf8_lossy(fragment).into_owned(),
            ));
        }
        events.push(StreamEvent::tool_call_end(id));
    }
    events.push(StreamEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    events
}

/// The prose of a short, a normal and a long answer.
fn prose(repeats: usize) -> String {
    NARRATION.repeat(repeats)
}

fn accumulator(c: &mut Criterion) {
    let mut group = c.benchmark_group("provider/stream_accumulator");
    for (label, repeats, tool_calls) in [
        ("short_1_call", 1_usize, 1_usize),
        ("typical_2_calls", 4, 2),
        ("long_2_calls", 16, 2),
    ] {
        // The events are built once: an adapter decodes them from the wire, and
        // measuring that decoding here would measure the fixture.
        let script = events(&prose(repeats), 3, tool_calls);
        group.throughput(criterion::Throughput::Elements(script.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(label),
            &script,
            |b, script: &Vec<StreamEvent>| {
                // `push` consumes each event, so the clone is setup rather
                // than part of what is measured.
                b.iter_batched(
                    || script.clone(),
                    |events| {
                        let mut accumulator =
                            StreamAccumulator::new(RequestId::nil(), "openai", "gpt-4o");
                        for event in events {
                            accumulator.push(black_box(event)).unwrap();
                        }
                        black_box(accumulator.finish().unwrap())
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn whole_stream(c: &mut Criterion) {
    let mut group = c.benchmark_group("provider/reconstruct");
    for (label, repeats, tool_calls) in [
        ("short_1_call", 1_usize, 1_usize),
        ("typical_2_calls", 4, 2),
        ("long_2_calls", 16, 2),
    ] {
        let script = events(&prose(repeats), 3, tool_calls);
        group.throughput(criterion::Throughput::Elements(script.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(label),
            &script,
            |b, script: &Vec<StreamEvent>| {
                // `from_events` is a fixture helper over an in-memory
                // iterator, so the only I/O-shaped cost measured is the poll
                // loop itself. Building the stream is setup: the events are
                // cloned before the clock starts.
                b.iter_batched(
                    || ModelStream::from_events(script.clone()),
                    |stream| {
                        let seed = StreamAccumulator::new(RequestId::nil(), "openai", "gpt-4o");
                        black_box(
                            futures::executor::block_on(reconstruct(stream, seed))
                                .expect("the script reassembles"),
                        )
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, accumulator, whole_stream);
criterion_main!(benches);
