//! Persistence overhead (spec §28).
//!
//! Spec §28 asks for persistence time to be measured separately from
//! projection, reduction and provider latency. This file measures the two
//! store operations that sit on the critical path of a turn:
//!
//! * **committing a bundle**, which is the single transactional write a turn
//!   makes — the journal completion, the events, the interaction changes and
//!   the replay record, all or nothing;
//! * **reading a page of the event stream**, which is what a consumer of the
//!   ledger pays per page.
//!
//! What these numbers are *not* is a claim about the real store. This is the
//! in-memory implementation: one mutex, no serialization to a wire, no
//! network, no fsync. It is the floor — the cost of the bookkeeping the store
//! contract imposes, with the database removed — and it exists so that when
//! the PostgreSQL store is measured, the difference is attributable. Read
//! `docs/benchmarks.md` before quoting anything from here.
//!
//! The commit bundle is rebuilt per iteration because
//! [`CommitStore::commit`] consumes it, so construction is kept out of the
//! measured closure with `iter_batched`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::hint::black_box;

use chrono::{DateTime, Utc};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use tokio::runtime::Runtime;
use turnframe_core::case::CaseKey;
use turnframe_core::event::CommittedEvent;
use turnframe_core::ids::{AccountId, CaseRevision, CommandId, EventId};
use turnframe_store::prelude::*;

/// A current-thread runtime: the in-memory store never holds its lock across an
/// await, so a single thread measures the store instead of the scheduler.
fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime")
}

/// One batch of `events` domain events for `case_id` at `revision`.
fn batch(account: &AccountId, case_id: &str, revision: u64, events: usize) -> EventBatch {
    let events = (0..events)
        .map(|index| CommittedEvent {
            event_id: EventId::new(),
            event_type: "trip.extra_added".to_owned(),
            occurred_at: DateTime::<Utc>::UNIX_EPOCH,
            payload: serde_json::json!({
                "extra_id": format!("extra_{index:04}"),
                "amount_cents": 12_500 + index,
            }),
        })
        .collect();
    EventBatch::new(
        account.clone(),
        CaseKey::new("trip", case_id),
        CommandId::new(),
        CaseRevision(revision),
        events,
    )
}

fn commit_bundle(c: &mut Criterion) {
    let runtime = runtime();
    let account = AccountId::from("bench");

    let mut group = c.benchmark_group("store/commit_bundle");
    for events in [1_usize, 8, 64] {
        group.bench_with_input(
            BenchmarkId::from_parameter(events),
            &events,
            |b, &events| {
                let stores = Stores::in_memory();
                let mut revision = 0_u64;
                b.iter_batched(
                    || {
                        // Setup, not measured: a fresh bundle at a fresh
                        // revision, because commit consumes the bundle and the
                        // store rejects a repeated one.
                        revision += 1;
                        CommitBundle::new().with_events(batch(
                            &account,
                            "trip-000042",
                            revision,
                            events,
                        ))
                    },
                    |bundle| {
                        black_box(
                            runtime
                                .block_on(stores.commit().commit(&account, bundle))
                                .expect("the bundle commits"),
                        )
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn event_stream_page(c: &mut Criterion) {
    let runtime = runtime();
    let account = AccountId::from("bench");
    let stores = Stores::in_memory();

    // Seeding is setup: five events on each of four hundred revisions, so a
    // page boundary falls inside a revision and the cursor has to do its job.
    runtime.block_on(async {
        for revision in 1..=400_u64 {
            stores
                .events()
                .append(batch(&account, "trip-000042", revision, 5))
                .await
                .expect("the seed appends");
        }
    });

    let mut group = c.benchmark_group("store/event_stream_page");
    for limit in [32_usize, 128, 512] {
        group.bench_with_input(BenchmarkId::from_parameter(limit), &limit, |b, &limit| {
            b.iter(|| {
                let page = runtime
                    .block_on(
                        stores
                            .events()
                            .read_from(&account, EventCursor::START, limit),
                    )
                    .expect("the page reads");
                black_box(page.next_cursor)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, commit_bundle, event_stream_page);
criterion_main!(benches);
