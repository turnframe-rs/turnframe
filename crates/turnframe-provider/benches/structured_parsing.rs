//! Structured-output parsing, including schema validation (spec §28): the deterministic
//! work between the network and the reducer, measured apart from provider latency.
//!
//! * `parse_structured`: the whole gate (extract one JSON document, validate it, deserialize
//!   it) on an answer the size a real turn produces;
//! * `validate` alone isolates the `jsonschema` pass, and `deserialize` alone `serde`, so a
//!   regression can be attributed;
//! * `compile` is the cost the [`SchemaCache`] exists to avoid, beside a cache hit.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use turnframe_provider::ids::RequestId;
use turnframe_provider::response::ModelResponse;
use turnframe_provider::structured::{
    CompiledSchema, SchemaCache, parse_structured, parse_structured_value,
};

/// One proposed act, shaped like spec §10.2: an operation, a target the model
/// describes but does not resolve, arguments, and the user's own words as
/// evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposedAct {
    operation: String,
    target: Target,
    arguments: Value,
    evidence: Evidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    kind: String,
    description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    quote: String,
    start: u32,
    end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Question {
    text: String,
    basis: String,
}

/// A document shaped like what one turn proposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnPlan {
    acts: Vec<ProposedAct>,
    questions: Vec<Question>,
    constraints: Vec<String>,
}

/// The schema shown to the model and enforced on the way back. Strict at every
/// level: an unknown field is a violation the schema itself catches.
fn plan_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "acts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "operation": {"type": "string"},
                        "target": {
                            "type": "object",
                            "properties": {
                                "kind": {"type": "string", "enum": ["existing", "new", "ambiguous"]},
                                "description": {"type": "string"}
                            },
                            "required": ["kind", "description"],
                            "additionalProperties": false
                        },
                        "arguments": {"type": "object"},
                        "evidence": {
                            "type": "object",
                            "properties": {
                                "quote": {"type": "string", "minLength": 1},
                                "start": {"type": "integer", "minimum": 0},
                                "end": {"type": "integer", "minimum": 0}
                            },
                            "required": ["quote", "start", "end"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["operation", "target", "arguments", "evidence"],
                    "additionalProperties": false
                }
            },
            "questions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "basis": {"type": "string", "enum": ["current", "proposed", "post_commit"]}
                    },
                    "required": ["text", "basis"],
                    "additionalProperties": false
                }
            },
            "constraints": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["acts", "questions", "constraints"],
        "additionalProperties": false
    })
}

/// A plan with `acts` acts, one question and one constraint — the shape of a
/// long colloquial turn that changes several fields at once.
fn plan(acts: usize) -> Value {
    json!({
        "acts": (0..acts)
            .map(|index| json!({
                "operation": "trip.assign_payer",
                "target": {
                    "kind": "existing",
                    "description": format!("the hotel night of day {index}")
                },
                "arguments": {"extra": index + 1, "payer": "company"},
                "evidence": {
                    "quote": format!("la notte d'albergo del giorno {index} la paga l'azienda"),
                    "start": (index * 48) as u32,
                    "end": (index * 48 + 46) as u32
                }
            }))
            .collect::<Vec<_>>(),
        "questions": [
            {"text": "Questo cambia il mio bagaglio?", "basis": "proposed"}
        ],
        "constraints": ["do_not_submit"]
    })
}

/// The same plan as a model answer, the way an adapter hands it over.
fn response(acts: usize) -> ModelResponse {
    ModelResponse::new(RequestId::new(), "openai", "gpt-4o").with_text(plan(acts).to_string())
}

fn parsing(c: &mut Criterion) {
    let schema = CompiledSchema::compile(&plan_schema()).expect("the bench schema compiles");

    let mut group = c.benchmark_group("provider/parse_structured");
    for acts in [1_usize, 4, 16] {
        // Building the response is not part of the gate: an adapter has
        // already done it by the time this is called.
        let response = response(acts);
        assert!(parse_structured::<TurnPlan>(&response, &schema).is_ok());
        group.bench_with_input(
            BenchmarkId::from_parameter(acts),
            &response,
            |b, response| {
                b.iter(|| {
                    black_box(parse_structured::<TurnPlan>(black_box(response), &schema).unwrap())
                });
            },
        );
    }
    group.finish();

    let mut group = c.benchmark_group("provider/schema_validate");
    for acts in [1_usize, 4, 16] {
        let value = plan(acts);
        group.bench_with_input(BenchmarkId::from_parameter(acts), &value, |b, value| {
            b.iter(|| black_box(schema.validate(black_box(value))).is_ok());
        });
    }
    group.finish();

    let mut group = c.benchmark_group("provider/deserialize");
    for acts in [1_usize, 4, 16] {
        // A schema that validates everything, so this row is serde alone.
        let permissive = CompiledSchema::compile(&json!({})).expect("the empty schema compiles");
        let value = plan(acts);
        group.bench_with_input(BenchmarkId::from_parameter(acts), &value, |b, value| {
            b.iter(|| {
                black_box(
                    parse_structured_value::<TurnPlan>(black_box(value), &permissive).unwrap(),
                )
            });
        });
    }
    group.finish();
}

fn schema_compilation(c: &mut Criterion) {
    let schema = plan_schema();

    let mut group = c.benchmark_group("provider/schema");
    group.bench_function("compile_cold", |b| {
        b.iter(|| black_box(CompiledSchema::compile(black_box(&schema)).unwrap()));
    });

    // The cache is keyed on the canonical digest of the schema, so a hit still
    // pays for canonicalization and a BLAKE3 pass. That is the honest cost of
    // the memoization, and it is what the turn path actually pays.
    let cache = SchemaCache::new();
    cache.compile(&schema).expect("the first compile succeeds");
    group.bench_function("cache_hit", |b| {
        b.iter(|| black_box(cache.compile(black_box(&schema)).unwrap()));
    });
    group.finish();
}

criterion_group!(benches, parsing, schema_compilation);
criterion_main!(benches);
