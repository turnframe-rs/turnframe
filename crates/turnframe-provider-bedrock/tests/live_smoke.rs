//! Optional live smoke tests (spec §20.8), outside CI: one structured task over `Converse`
//! and one streamed reply over `ConverseStream`. Mocks cannot say whether a real service
//! accepts this adapter's request: a model id needing an inference profile, a region without
//! the model, a stricter `inputSchema` check all pass every mocked row.
//!
//! They run only when [`MODEL_VAR`] names a model, and skip when `CI` is set; a skip prints a
//! note and passes. The switch is the model id, not a credential: the AWS SDK resolves
//! credentials from its ordinary chain, and nothing can guess an account- and region-specific
//! model id.
//!
//! ```bash
//! TURNFRAME_BEDROCK_LIVE_MODEL=anthropic.claude-3-5-haiku-20241022-v1:0 \
//! TURNFRAME_BEDROCK_LIVE_REGION=eu-central-1 \
//!     cargo test -p turnframe-provider-bedrock --test live_smoke -- --nocapture
//! ```
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `TURNFRAME_BEDROCK_LIVE_MODEL` | the model id; absent means skip | — |
//! | `TURNFRAME_BEDROCK_LIVE_REGION` | the region to call it in | whatever the AWS chain resolves |

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::time::Duration;

use aws_sdk_bedrockruntime::config::{BehaviorVersion, Region};
use serde::Deserialize;
use serde_json::json;
use turnframe_provider::conformance::payloads;
use turnframe_provider::prelude::*;
use turnframe_provider_bedrock::BedrockProvider;

/// The model id that switches these tests on.
const MODEL_VAR: &str = "TURNFRAME_BEDROCK_LIVE_MODEL";

/// The region to call it in, when the AWS chain does not already say.
const REGION_VAR: &str = "TURNFRAME_BEDROCK_LIVE_REGION";

/// A live call gets more room than a mock and still cannot hang a suite.
const LIVE_TIMEOUT: Duration = Duration::from_secs(30);

/// What the structured call asks the model for.
///
/// Deliberately trivial and answerable in a handful of tokens: this is a wire
/// check, not an evaluation. It travels as the `inputSchema` of the one forced
/// tool, which is the only structured-output transport Converse has.
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
/// It is a declaration about *that* model, made by whoever set the variable —
/// which is the rule this whole crate is built on. The two things the protocol
/// decides come from [`BedrockProvider::converse_defaults`]; the transport and
/// tool calling are what the smoke test needs to exercise a structured call.
fn live_capabilities() -> ProviderCapabilities {
    BedrockProvider::converse_defaults()
        .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
        .with_tool_calling(ToolCallingCapability::Parallel)
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
/// Returns `None` after printing a note, so a machine with no AWS account runs
/// a green suite rather than a red one.
async fn live_provider() -> Option<BedrockProvider> {
    load_dotenv();
    if std::env::var_os("CI").is_some() {
        println!("live smoke skipped: CI is set, and these tests never run there");
        return None;
    }
    let Some(model) = std::env::var_os(MODEL_VAR).and_then(|value| value.into_string().ok()) else {
        println!("live smoke skipped: set {MODEL_VAR} to run it against a real endpoint");
        return None;
    };
    if model.trim().is_empty() {
        println!("live smoke skipped: {MODEL_VAR} is empty");
        return None;
    }
    // The credential is never held here: the SDK resolves it from the ordinary
    // chain, and this crate only ever holds the configuration around it.
    let mut loader = aws_config::defaults(BehaviorVersion::latest());
    if let Ok(region) = std::env::var(REGION_VAR) {
        loader = loader.region(Region::new(region));
    }
    let sdk = loader.load().await;
    Some(
        BedrockProvider::builder()
            .sdk_config(&sdk)
            .model(model)
            .capabilities(live_capabilities())
            .timeout(LIVE_TIMEOUT)
            .build()
            .expect("the live configuration is valid"),
    )
}

#[tokio::test]
async fn a_real_endpoint_enforces_a_real_schema() {
    let Some(provider) = live_provider().await else {
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
    // construction.
    assert!(!smoke.word.is_empty(), "{smoke:?}");
    assert!(
        smoke.letters <= 64,
        "the second field must be a small integer, not prose: {smoke:?}"
    );
    // The transport is a forced tool, so a complete answer ends on the call.
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
    let Some(provider) = live_provider().await else {
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
    // broke one would otherwise only be noticed with an AWS account in hand.
    assert!(!payloads::DUMMY_API_KEY.is_empty());
    assert_ne!(MODEL_VAR, REGION_VAR);
    // The declaration a live run makes is one the builder will accept.
    let capabilities = live_capabilities();
    assert!(capabilities.structured_output.enforces_schema());
    assert!(capabilities.supports_tools());
    assert!(capabilities.streaming);
}
