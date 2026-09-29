//! Canonical hashing (spec §28).
//!
//! Canonical hashing is not a feature, it is a tax: an idempotency key, an
//! obligation identity, a schema fingerprint and a replay digest are all one
//! call to [`Digest::of_canonical`], and every turn makes several. Spec §28
//! asks for the deterministic core to be measured before any claim is made
//! about it, and this is the part of the core that touches every path.
//!
//! The two halves are measured separately because they scale differently:
//! [`canonical_json`] serializes and then sorts every object's keys, so it
//! grows with the *shape* of the value, while [`digest_hex`] is BLAKE3 over the
//! resulting bytes and grows with their length. A single end-to-end number
//! would leave you unable to tell which one to fix.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};
use turnframe_core::hash::{Digest, canonical_json, digest_hex};

/// A payload shaped like the things this workspace actually hashes: an
/// idempotency-key input, a view's obligation, a replay record.
///
/// `extras` controls the size without changing the shape, and the keys are
/// deliberately out of alphabetical order so the canonicalizer has real work.
fn payload(extras: usize) -> Value {
    json!({
        "workflow": "trip",
        "case_id": "trip-000042",
        "revision": 17,
        "actor": {"account_id": "acc_0001", "conversation_id": "conv_0009"},
        "command": {
            "operation": "trip.request_rebooking",
            "arguments": {"channel": "airline", "copy_to_traveler": true},
        },
        "extras": (0..extras)
            .map(|index| json!({
                "extra_id": format!("extra_{index:04}"),
                "description": format!("Hotel night {index}"),
                "amount_cents": 12_500 + index,
                "payer": "company",
            }))
            .collect::<Vec<_>>(),
    })
}

fn canonicalization(c: &mut Criterion) {
    let mut group = c.benchmark_group("hash/canonical_json");
    for extras in [0_usize, 8, 64] {
        let value = payload(extras);
        group.bench_with_input(BenchmarkId::from_parameter(extras), &value, |b, value| {
            b.iter(|| black_box(canonical_json(black_box(value)).unwrap()));
        });
    }
    group.finish();
}

fn digesting(c: &mut Criterion) {
    let mut group = c.benchmark_group("hash/digest_hex");
    for extras in [0_usize, 8, 64] {
        // Canonicalization happens once, outside the closure: this row is the
        // BLAKE3 pass and nothing else.
        let bytes = canonical_json(&payload(extras)).unwrap().into_bytes();
        group.bench_with_input(BenchmarkId::from_parameter(extras), &bytes, |b, bytes| {
            b.iter(|| black_box(digest_hex(black_box(bytes))));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("hash/of_canonical");
    for extras in [0_usize, 8, 64] {
        let value = payload(extras);
        group.bench_with_input(BenchmarkId::from_parameter(extras), &value, |b, value| {
            b.iter(|| black_box(Digest::of_canonical(black_box(value)).unwrap()));
        });
    }
    group.finish();
}

criterion_group!(benches, canonicalization, digesting);
criterion_main!(benches);
