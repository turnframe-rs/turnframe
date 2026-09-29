//! Optional live smoke tests (spec §20.8), outside CI: one structured task and one streamed
//! reply. Mocks cannot say whether a real daemon accepts this adapter's request: a `format`
//! the running version rejects, a model never pulled, a renamed `options` key.
//!
//! They run only when [`MODEL_VAR`] names a pulled model, and skip when `CI` is set; a skip
//! prints a note and passes. The switch is the model, since a plain `ollama serve`
//! authenticates nothing; a token is accepted for a daemon behind a proxy.
//!
//! ```bash
//! TURNFRAME_OLLAMA_LIVE_MODEL=qwen3:8b cargo test -p turnframe-provider-ollama \
//!     --test live_smoke -- --nocapture
//! ```
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `TURNFRAME_OLLAMA_LIVE_MODEL` | the model to call; absent means skip | — |
//! | `TURNFRAME_OLLAMA_LIVE_BASE_URL` | the daemon to call | `http://127.0.0.1:11434` |
//! | `TURNFRAME_OLLAMA_LIVE_TOKEN` | a bearer token, for a proxied daemon | none |
//!
//! A green run says the daemon accepted the request, not that the model honours a schema:
//! that is the conformance suite's to prove, per model
//! ([`declarations::baseline`](turnframe_provider_ollama::declarations::baseline)).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use turnframe_provider::conformance::payloads;
use turnframe_provider::prelude::*;
use turnframe_provider_ollama::{OllamaProvider, declarations};

/// The model that switches these tests on.
const MODEL_VAR: &str = "TURNFRAME_OLLAMA_LIVE_MODEL";

/// The daemon to call, when it is not the local one.
const BASE_URL_VAR: &str = "TURNFRAME_OLLAMA_LIVE_BASE_URL";

/// A bearer token, for a daemon behind an authenticating proxy.
const TOKEN_VAR: &str = "TURNFRAME_OLLAMA_LIVE_TOKEN";

/// A live call gets more room than a mock and still cannot hang a suite.
///
/// Longer than the hosted adapters allow themselves: a local daemon may have to
/// load several gigabytes of weights before it answers the first token.
const LIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// What the structured call asks the model for.
///
/// Deliberately trivial and answerable in a handful of tokens: this is a wire
/// check, not an evaluation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Smoke {
    /// The word the model was asked to echo.
    word: String,
    /// How many letters it has, which forces the model to fill two fields.
    letters: u32,
}

/// What a live run declares for the model it was pointed at.
///
/// The baseline plus the schema transport: `format` takes a JSON Schema and
/// constrains decoding against it, which is what the structured call exercises.
/// It is a declaration about *that* model, made by whoever set the variable.
fn live_capabilities() -> ProviderCapabilities {
    declarations::baseline().with_structured_output(StructuredOutputCapability::NativeJsonSchema)
}

/// Loads the repository's `.env` once per test binary, before any test reads a
/// variable or opens a connection. A variable already set in the shell wins.
fn load_dotenv() {
    static LOADED: std::sync::Once = std::sync::Once::new();
    LOADED.call_once(|| {
        let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    });
}

/// Builds the provider, or explains why the test is skipping.
///
/// Returns `None` after printing a note, so a machine with no daemon runs a
/// green suite rather than a red one.
fn live_provider() -> Option<OllamaProvider> {
    load_dotenv();
    if std::env::var_os("CI").is_some() {
        println!("live smoke skipped: CI is set, and these tests never run there");
        return None;
    }
    let Some(model) = std::env::var_os(MODEL_VAR).and_then(|value| value.into_string().ok()) else {
        println!("live smoke skipped: set {MODEL_VAR} to run it against a real daemon");
        return None;
    };
    if model.trim().is_empty() {
        println!("live smoke skipped: {MODEL_VAR} is empty");
        return None;
    }
    let mut builder = OllamaProvider::local()
        .model(model)
        .capabilities(live_capabilities())
        .timeout(LIVE_TIMEOUT);
    if let Ok(base_url) = std::env::var(BASE_URL_VAR) {
        builder = builder.base_url(base_url);
    }
    if let Ok(token) = std::env::var(TOKEN_VAR) {
        builder = builder.bearer_token(ApiKey::new(token));
    }
    Some(builder.build().expect("the live configuration is valid"))
}

#[tokio::test]
async fn a_real_endpoint_enforces_a_real_schema() {
    let Some(provider) = live_provider() else {
        return;
    };
    let schema = json!({
        "type": "object",
        "properties": {
            "word": {"type": "string"},
            "letters": {"type": "integer"}
        },
        "required": ["word", "letters"],
        "additionalProperties": false
    });
    let request = ModelRequest::new(ModelPurpose::Extract)
        .with_system("Answer with the requested document and nothing else.")
        .with_message(Message::user("The word is 'turnframe'. Count its letters."))
        .with_output(OutputSpec::json("turnframe_live_smoke", schema.clone()))
        .with_max_output_tokens(256)
        .with_timeout(LIVE_TIMEOUT);

    let response = provider
        .generate(request)
        .await
        .expect("a live structured call");
    let compiled = CompiledSchema::compile(&schema).expect("the smoke schema compiles");
    let smoke: Smoke = parse_structured(&response, &compiled).expect("the answer is schema-valid");

    // The point is that the *shape* survived the round trip, not that the
    // model can count: a live assertion about content would be flaky by
    // construction, and doubly so for whatever small model is pulled locally.
    assert!(!smoke.word.is_empty(), "{smoke:?}");
    assert!(
        smoke.letters <= 64,
        "the second field must be a small integer, not prose: {smoke:?}"
    );
    assert!(response.finish.is_complete(), "{:?}", response.finish);
    println!(
        "live smoke: {} answered {} in {:?}",
        provider.model_key(),
        response.finish,
        response.latency
    );
    println!("live smoke: parsed {smoke:?}");
}

#[tokio::test]
async fn a_real_stream_reassembles_into_a_real_answer() {
    let Some(provider) = live_provider() else {
        return;
    };
    let request = ModelRequest::new(ModelPurpose::Acknowledge)
        .with_system("Reply in one short sentence.")
        .with_message(Message::user("Say hello to Turnframe."))
        .with_max_output_tokens(64)
        .with_timeout(LIVE_TIMEOUT);

    let stream = provider
        .stream(request.clone())
        .await
        .expect("a live streamed call");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(
            request.request_id,
            provider.provider_key(),
            provider.model_key(),
        ),
    )
    .await
    .expect("the live stream reassembles");

    assert!(!rebuilt.text().trim().is_empty(), "{rebuilt:?}");
    assert!(rebuilt.finish.is_complete(), "{:?}", rebuilt.finish);
    assert!(rebuilt.warnings.contains(&ResponseWarning::Reconstructed));
    println!("live smoke: streamed {:?}", rebuilt.text());
}

#[test]
fn the_corpus_the_mocked_suite_uses_is_reachable_from_here_too() {
    // A cheap guard against the live file drifting away from the suite it
    // complements: both are built on the same crate surface, and a rename that
    // broke one would otherwise only be noticed with a daemon running.
    assert!(!payloads::DUMMY_API_KEY.is_empty());
    assert_ne!(MODEL_VAR, BASE_URL_VAR);
    // The declaration a live run makes is one the builder will accept, and it
    // is honest about the one thing no model behind this endpoint can claim.
    let capabilities = live_capabilities();
    assert!(capabilities.structured_output.enforces_schema());
    assert!(capabilities.streaming);
    assert!(!capabilities.documents, "there is no document channel here");
}
