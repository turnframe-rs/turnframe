//! The conformance suite of spec §20.8, run against this adapter.
//!
//! This file is the deliverable that proves the crate. It runs the whole
//! twenty-row feature suite from `turnframe_provider::conformance`, and its
//! thirteen per-status rows, against a
//! wiremock server speaking the chat-completions format, three times over:
//!
//! * the **OpenAI** profile, with every capability declared — nothing may be
//!   skipped, because a skipped row is not a pass;
//! * the **Azure OpenAI** profile, whose deployment-shaped path and `api-key`
//!   header are a different surface for the same behaviour;
//! * a **generic compatible** profile that declares `json_object` and no tool
//!   calling — which must *pass* while skipping the rows it cannot honestly
//!   claim, because honesty is not a failure.
//!
//! Everything the fixtures return is vendor framing around the suite's own
//! corpus: the schema, the payloads and the planted credential all come from
//! `payloads`, so this adapter is measured on the same thing every other
//! adapter is.
//!
//! **Conformance is per provider-model pair.** These runs say something about
//! this adapter against these fixtures. They say nothing about `gpt-4o` versus
//! `gpt-4o-mini`, and nothing at all about a gateway nobody has measured.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::conformance::{
    Check, CheckStatus, ConformanceReport, ProviderFactory, Scenario, StatusRow, StatusSupport,
    WireFixtures, payloads, run_all,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::prelude::*;
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::secret::ApiKey;
use turnframe_provider::stream::{StreamAccumulator, reconstruct};
use turnframe_provider_openai::OpenAiProvider;
use turnframe_provider_openai::profile::{DEFAULT_AZURE_API_VERSION, EndpointProfile, Preset};
use wiremock::matchers::{body_string_contains, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The model every run is configured with. URL-safe, because the Azure profile
/// puts it in the path.
const MODEL: &str = "gpt-4o-2024-08-06";

/// Matches the chat-completions path of both route shapes.
const CHAT_PATH: &str = r".*/chat/completions$";

/// The prose the streaming scenario answers with, whole and in fragments.
const NARRATION: &str = "Ho preparato la modifica.";

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Builds this adapter against a mock server, for one profile.
struct Factory {
    profile: EndpointProfile,
    capabilities: Option<ProviderCapabilities>,
}

impl Factory {
    fn openai() -> Self {
        Self {
            profile: EndpointProfile::openai(),
            capabilities: None,
        }
    }

    fn azure() -> Self {
        Self {
            profile: EndpointProfile::azure_openai(DEFAULT_AZURE_API_VERSION),
            capabilities: None,
        }
    }

    /// A gateway nobody has measured: `json_object`, no tools, streaming.
    fn generic() -> Self {
        Self {
            profile: EndpointProfile::preset(Preset::OpenRouter),
            capabilities: Some(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::JsonObject)
                    .with_tool_calling(ToolCallingCapability::None)
                    .with_streaming(true),
            ),
        }
    }

    /// Everything the OpenAI profile declares, with `transport` in place of
    /// the schema-enforcing `response_format`.
    fn transport(transport: StructuredOutputCapability) -> Self {
        Self {
            profile: EndpointProfile::openai(),
            capabilities: Some(fully_capable(transport)),
        }
    }

    /// A self-hosted runtime declaring the grammar transport its server backs.
    fn runtime(preset: Preset) -> Self {
        Self {
            profile: EndpointProfile::preset(preset),
            capabilities: Some(fully_capable(
                StructuredOutputCapability::GrammarConstrained,
            )),
        }
    }
}

/// A declaration that leaves no conformance row unproven.
fn fully_capable(transport: StructuredOutputCapability) -> ProviderCapabilities {
    ProviderCapabilities::minimal()
        .with_structured_output(transport)
        .with_tool_calling(ToolCallingCapability::Parallel)
        .with_streaming(true)
        .with_vision(true)
        .with_preserves_call_ids(true)
}

impl ProviderFactory for Factory {
    type Provider = OpenAiProvider;

    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        let mut builder = OpenAiProvider::builder(self.profile.clone())
            .api_key(api_key)
            .base_url(base_url)
            .model(MODEL);
        if let Some(capabilities) = &self.capabilities {
            builder = builder.capabilities(capabilities.clone());
        }
        builder.build().map_err(ProviderError::from)
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Fixtures;

/// A completed chat-completions answer carrying `content`.
fn completion(content: &str, finish: &str) -> Value {
    json!({
        "id": "chatcmpl-turnframe-1",
        "object": "chat.completion",
        "created": 1_760_000_000_u64,
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content, "refusal": null},
            "logprobs": null,
            "finish_reason": finish
        }],
        "usage": {
            "prompt_tokens": payloads::USAGE_INPUT_TOKENS,
            "completion_tokens": payloads::USAGE_OUTPUT_TOKENS,
            "total_tokens": payloads::USAGE_INPUT_TOKENS + payloads::USAGE_OUTPUT_TOKENS,
            // `prompt_tokens` is the whole prompt and `cached_tokens` is the
            // part of it that was served from the cache — the contract the
            // usage row holds every adapter to.
            "prompt_tokens_details": {"cached_tokens": payloads::USAGE_CACHED_TOKENS}
        },
        "system_fingerprint": "fp_turnframe"
    })
}

/// An error body in the shape every compatible endpoint copies.
fn api_error(code: &str, message: &str, kind: &str) -> Value {
    json!({"error": {"code": code, "message": message, "type": kind, "param": null}})
}

/// One server-sent event line.
fn sse(payload: &Value) -> String {
    format!("data: {payload}\n\n")
}

/// The streamed twin of [`completion`] for the narration answer.
fn narration_stream() -> String {
    let chunk = |delta: Value, finish: Value| {
        json!({
            "id": "chatcmpl-turnframe-1",
            "object": "chat.completion.chunk",
            "model": MODEL,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]
        })
    };
    let mut body = String::new();
    body.push_str(&sse(&chunk(
        json!({"role": "assistant", "content": "Ho preparato "}),
        Value::Null,
    )));
    body.push_str(&sse(&chunk(
        json!({"content": "la modifica."}),
        Value::Null,
    )));
    body.push_str(&sse(&chunk(json!({}), json!("stop"))));
    body.push_str(&sse(&json!({
        "id": "chatcmpl-turnframe-1",
        "choices": [],
        "usage": {"prompt_tokens": payloads::USAGE_INPUT_TOKENS,
                  "completion_tokens": payloads::USAGE_OUTPUT_TOKENS,
                  "prompt_tokens_details": {"cached_tokens": payloads::USAGE_CACHED_TOKENS}}
    })));
    body.push_str("data: [DONE]\n\n");
    body
}

/// The template each scenario answers with, once the streaming special case is
/// out of the way.
fn template(scenario: Scenario) -> ResponseTemplate {
    match scenario {
        Scenario::ValidStructured => ResponseTemplate::new(200)
            .set_body_json(completion(&payloads::valid_plan().to_string(), "stop")),
        Scenario::MalformedJson => {
            ResponseTemplate::new(200).set_body_json(completion(payloads::MALFORMED_JSON, "stop"))
        }
        Scenario::UnknownField => ResponseTemplate::new(200).set_body_json(completion(
            &payloads::plan_with_unknown_field().to_string(),
            "stop",
        )),
        Scenario::MissingField => ResponseTemplate::new(200).set_body_json(completion(
            &payloads::plan_with_missing_field().to_string(),
            "stop",
        )),
        Scenario::MultipleActs => ResponseTemplate::new(200)
            .set_body_json(completion(&payloads::two_act_plan().to_string(), "stop")),
        Scenario::ToolCallIds => ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-turnframe-1",
            "model": MODEL,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": payloads::EXPECTED_CALL_ID,
                        "type": "function",
                        "function": {
                            "name": payloads::TOOL_NAME,
                            "arguments": "{\"target\": \"tok_1\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 12, "completion_tokens": 4}
        })),
        Scenario::StreamingReconstruction => {
            ResponseTemplate::new(200).set_body_json(completion(NARRATION, "stop"))
        }
        // The same answer, mounted for the usage row: `completion` already
        // reports the whole prompt and the cached slice of it, which is what
        // that row reads.
        Scenario::CachedUsage => {
            ResponseTemplate::new(200).set_body_json(completion(NARRATION, "stop"))
        }
        // A completion with nothing in it: the shape a filtered or exhausted
        // answer really takes, not an empty HTTP body.
        Scenario::EmptyOutput => ResponseTemplate::new(200).set_body_json(completion("", "stop")),
        Scenario::Refusal => ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-turnframe-1",
            "model": MODEL,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": payloads::REFUSAL_TEXT
                },
                "finish_reason": "stop"
            }]
        })),
        Scenario::SlowResponse => ResponseTemplate::new(200)
            .set_body_json(completion("troppo tardi", "stop"))
            .set_delay(payloads::SLOW_RESPONSE_DELAY),
        Scenario::RateLimited => ResponseTemplate::new(429)
            .insert_header(
                "retry-after",
                payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
            )
            .set_body_json(api_error(
                "rate_limit_exceeded",
                "Rate limit reached for requests",
                "requests",
            )),
        Scenario::Authentication => ResponseTemplate::new(401).set_body_json(api_error(
            "invalid_api_key",
            "Incorrect API key provided.",
            "invalid_request_error",
        )),
        Scenario::ContextOverflow => ResponseTemplate::new(400).set_body_json(api_error(
            "context_length_exceeded",
            "This model's maximum context length is 8192 tokens. However, your messages \
             resulted in 10245 tokens. Please reduce the length of the messages.",
            "invalid_request_error",
        )),
        // The credential is echoed back where a careless gateway puts it: in a
        // debug envelope field and in a response header. Neither is read by the
        // adapter, and neither may survive into any rendering of it.
        Scenario::SecretInBody => ResponseTemplate::new(200)
            .insert_header(
                "x-upstream-authorization",
                format!("Bearer {}", payloads::DUMMY_API_KEY).as_str(),
            )
            .set_body_json(json!({
                "id": "chatcmpl-turnframe-1",
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant",
                                "content": payloads::valid_plan().to_string()},
                    "finish_reason": "stop"
                }],
                "_debug": {"authorization": format!("Bearer {}", payloads::DUMMY_API_KEY)}
            })),
        // ------------------------------------------------------------------
        // The per-status rows. Each body is the shape this endpoint family
        // really returns, because the point of these rows is that an adapter
        // reads the *body* where the status alone would mislead it.
        // ------------------------------------------------------------------
        Scenario::Authorization => ResponseTemplate::new(403).set_body_json(api_error(
            "insufficient_permissions",
            "You have insufficient permissions for this operation. Missing scopes: \
             model.request.",
            "invalid_request_error",
        )),
        Scenario::ModelNotFound => ResponseTemplate::new(404).set_body_json(api_error(
            "model_not_found",
            "The model 'gpt-9' does not exist or you do not have access to it.",
            "invalid_request_error",
        )),
        Scenario::RequestTimeout => ResponseTemplate::new(408).set_body_json(api_error(
            "request_timeout",
            "Request timed out. Please try again.",
            "timeout",
        )),
        // Deliberately a 400 nobody could mistake for a context problem: the
        // two 400 rows exist to be told apart.
        Scenario::InvalidRequest => ResponseTemplate::new(400).set_body_json(api_error(
            "invalid_value",
            "Invalid value for 'temperature': expected a number between 0 and 2.",
            "invalid_request_error",
        )),
        Scenario::ServerError => ResponseTemplate::new(500).set_body_json(api_error(
            "server_error",
            "The server had an error while processing your request. Sorry about that!",
            "server_error",
        )),
        Scenario::ServiceUnavailable => ResponseTemplate::new(503).set_body_json(api_error(
            "engine_overloaded",
            "The engine is currently overloaded. Please try again later.",
            "server_error",
        )),
        // Azure's shape, which OpenAI's own filter mirrors: a 400 whose code
        // says the safety filter fired. Never a server error.
        Scenario::ContentFilter => ResponseTemplate::new(400).set_body_json(api_error(
            "content_filter",
            "The response was filtered due to the prompt triggering our content management \
             policy. Please modify your prompt and retry.",
            "invalid_request_error",
        )),
        // The same 401 an incorrect key produces, with the one word that
        // separates "refresh me" from "replace me" in the body.
        Scenario::ExpiredCredential => ResponseTemplate::new(401).set_body_json(api_error(
            "token_expired",
            "The access token has expired. Refresh the token and try again.",
            "invalid_request_error",
        )),
        // The trap: a 429 carrying a Retry-After, exactly like a rate limit,
        // for something no amount of waiting fixes.
        Scenario::QuotaExhausted => ResponseTemplate::new(429)
            .insert_header(
                "retry-after",
                payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
            )
            .set_body_json(api_error(
                "insufficient_quota",
                "You exceeded your current quota, please check your plan and billing details.",
                "insufficient_quota",
            )),
        // A scenario this fixture does not model must fail loudly. Answering
        // 200 would let a future row pass by accident, which is how the nine
        // rows above went unnoticed in the first place.
        _ => ResponseTemplate::new(501).set_body_json(api_error(
            "fixture_not_modelled",
            "these fixtures do not model this scenario",
            "server_error",
        )),
    }
}

#[async_trait]
impl WireFixtures for Fixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        if scenario == Scenario::StreamingReconstruction {
            // Mounted first, and matched on the flag the adapter puts on the
            // wire, so the non-streamed call falls through to the twin below.
            Mock::given(method("POST"))
                .and(path_regex(CHAT_PATH))
                .and(body_string_contains("\"stream\":true"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(narration_stream(), "text/event-stream"),
                )
                .mount(server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path_regex(CHAT_PATH))
            .respond_with(template(scenario))
            .mount(server)
            .await;
    }
}

// ---------------------------------------------------------------------------
// The runs
// ---------------------------------------------------------------------------

async fn run(factory: Factory) -> ConformanceReport {
    run_all(&factory, &Fixtures).await
}

#[tokio::test]
async fn the_openai_profile_passes_every_row_without_skipping_one() {
    let report = run(Factory::openai()).await;
    assert!(report.passed(), "{report}");
    assert_eq!(report.provider.as_str(), "openai");
    assert_eq!(report.model.as_str(), MODEL);
    // The twenty feature rows plus the thirteen per-status rows.
    let expected = Check::run_order();
    assert_eq!(report.results.len(), expected.len());
    let (passed, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(
        skipped, 0,
        "a fully capable profile proves every row:\n{report}"
    );
    assert_eq!(passed, expected.len());
    let order: Vec<Check> = report.results.iter().map(|result| result.check).collect();
    assert_eq!(order, expected);
    // Every per-status row was exercised rather than declared unproducible.
    for check in Check::STATUS {
        assert_eq!(
            report.result(check).expect("row ran").status,
            CheckStatus::Passed,
            "{check}:\n{report}"
        );
    }
}

#[tokio::test]
async fn the_azure_profile_passes_the_same_rows_through_a_different_surface() {
    let report = run(Factory::azure()).await;
    assert!(report.passed(), "{report}");
    assert_eq!(report.provider.as_str(), "azure-openai");
    let (_, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(skipped, 0, "{report}");
}

#[tokio::test]
async fn a_modest_compatible_profile_passes_by_skipping_what_it_cannot_claim() {
    let report = run(Factory::generic()).await;
    assert!(report.passed(), "honesty is not a failure:\n{report}");
    for check in [
        Check::ToolAndReadRequestIds,
        Check::NoSilentCapabilityDowngrade,
    ] {
        assert_eq!(
            report.result(check).expect("check ran").status,
            CheckStatus::Skipped,
            "{check} cannot be proven by a json_object, tool-less profile:\n{report}"
        );
    }
    // Streaming is declared, so it is proven rather than skipped.
    assert_eq!(
        report
            .result(Check::StreamingReconstruction)
            .expect("check ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
}

/// A deployment whose edge answers before the upstream can.
///
/// These fixtures mount all thirteen per-status rows, so nothing is unproven
/// in the runs above. This one exists to exercise the other half of the
/// contract: a row an endpoint genuinely cannot produce is declared, in words,
/// and the report calls it **unproven** rather than passing it over.
struct EdgeGateway;

#[async_trait]
impl WireFixtures for EdgeGateway {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        if scenario == Scenario::RequestTimeout {
            return;
        }
        Fixtures.mount(server, scenario).await;
    }

    fn status_support(&self, row: StatusRow) -> StatusSupport {
        match row {
            StatusRow::RequestTimeout => StatusSupport::not_producible(
                "this deployment sits behind an edge that answers 504 on its own deadline, \
                 so an upstream 408 never reaches the adapter",
            ),
            _ => StatusSupport::Mounted,
        }
    }
}

#[tokio::test]
async fn a_row_the_endpoint_cannot_produce_is_unproven_and_says_why() {
    let report = run_all(&Factory::openai(), &EdgeGateway).await;
    assert!(
        report.passed(),
        "an honest skip is not a failure:\n{report}"
    );
    let row = report
        .result(Check::StatusMapping(StatusRow::RequestTimeout))
        .expect("the row ran");
    assert_eq!(row.status, CheckStatus::Skipped);
    assert!(
        row.detail.as_deref().unwrap_or_default().contains("504"),
        "the reason travels into the report: {row:?}"
    );
    // A published table cannot claim what the run never demonstrated.
    let table = report.compatibility_table();
    assert!(
        table.contains("| `status_408_timeout` | unproven |"),
        "{table}"
    );
    let (_, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(skipped, 1, "only the declared row is unproven:\n{report}");
}

// ---------------------------------------------------------------------------
// What actually went on the wire
// ---------------------------------------------------------------------------

/// Builds a provider against a fresh server with `scenario` mounted.
async fn staged(factory: &Factory, scenario: Scenario) -> (MockServer, OpenAiProvider) {
    let server = MockServer::start().await;
    Fixtures.mount(&server, scenario).await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    (server, provider)
}

/// The body of the first request the server saw.
async fn first_body(server: &MockServer) -> Value {
    let requests = server.received_requests().await.expect("recording enabled");
    assert!(!requests.is_empty(), "no request reached the server");
    serde_json::from_slice(&requests[0].body).expect("a JSON body")
}

#[tokio::test]
async fn a_schema_enforcing_profile_puts_the_schema_on_the_wire() {
    let factory = Factory::openai();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(
        body["response_format"]["json_schema"]["name"],
        payloads::SCHEMA_NAME
    );
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert!(
        body.to_string().contains(payloads::SCHEMA_MARKER),
        "the schema itself must travel, not just its name"
    );
}

#[tokio::test]
async fn a_json_object_profile_does_not_quietly_upgrade_itself() {
    let factory = Factory::generic();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;
    // The weaker transport, and the schema described in the prompt instead.
    assert_eq!(body["response_format"]["type"], "json_object");
    assert!(body["response_format"].get("json_schema").is_none());
    let system = body["messages"][0]["content"]
        .as_str()
        .expect("a system message");
    assert!(system.contains("JSON"), "{system}");
    assert!(system.contains(payloads::SCHEMA_MARKER), "{system}");
}

#[tokio::test]
async fn the_azure_route_and_header_are_what_azure_expects() {
    let factory = Factory::azure();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let requests = server.received_requests().await.expect("recording enabled");
    let url = &requests[0].url;
    assert!(
        url.path()
            .ends_with(&format!("/openai/deployments/{MODEL}/chat/completions")),
        "{url}"
    );
    assert_eq!(
        url.query_pairs()
            .find(|(key, _)| key == "api-version")
            .map(|(_, value)| value.to_string()),
        Some(DEFAULT_AZURE_API_VERSION.to_owned())
    );
    // Azure authenticates with `api-key`, never with a bearer token.
    assert!(requests[0].headers.contains_key("api-key"));
    assert!(!requests[0].headers.contains_key("authorization"));
    // And the stable request id travels as an idempotency hint.
    assert!(requests[0].headers.contains_key("idempotency-key"));
}

#[tokio::test]
async fn the_streamed_answer_equals_the_whole_one_field_for_field() {
    let factory = Factory::openai();
    let (_server, provider) = staged(&factory, Scenario::StreamingReconstruction).await;
    let request = payloads::narration_request();

    let whole = provider.generate(request.clone()).await.expect("whole");
    let stream = provider.stream(request.clone()).await.expect("stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(
            request.request_id,
            provider.provider_key(),
            provider.model_key(),
        ),
    )
    .await
    .expect("reassembles");

    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.text(), NARRATION);
    assert_eq!(rebuilt.finish, whole.finish);
    // Usage arrives at the end of the stream, so the two agree on it too.
    assert_eq!(rebuilt.usage, whole.usage);
    assert_eq!(rebuilt.usage.cached_input, payloads::USAGE_CACHED_TOKENS);
    // The identifier no longer has to be seeded: it arrives on the first chunk.
    assert_eq!(rebuilt.raw_id, whole.raw_id);
    assert_eq!(rebuilt.raw_id.as_deref(), Some("chatcmpl-turnframe-1"));
    // The reassembled answer says it was reassembled; that is the one honest
    // difference between the paths.
    assert!(rebuilt.warnings.contains(&ResponseWarning::Reconstructed));
    assert!(!whole.warnings.contains(&ResponseWarning::Reconstructed));
}

#[tokio::test]
async fn the_streamed_path_reports_the_feature_it_dropped_just_as_the_whole_one_does() {
    // The endpoint takes four stop sequences. A fifth is dropped, and until the
    // stream had a warning event the whole call said so and the streamed call
    // said nothing — the same request, honest one way and silent the other.
    let factory = Factory::openai();
    let (_server, provider) = staged(&factory, Scenario::StreamingReconstruction).await;
    let request = payloads::narration_request().with_stop(
        ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    );
    let dropped = ResponseWarning::FeatureDropped {
        feature: "stop_sequences_over_limit".to_owned(),
    };

    let whole = provider.generate(request.clone()).await.expect("whole");
    assert!(whole.warnings.contains(&dropped), "{:?}", whole.warnings);

    let stream = provider.stream(request.clone()).await.expect("stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "openai", MODEL),
    )
    .await
    .expect("reassembles");
    assert!(
        rebuilt.warnings.contains(&dropped),
        "{:?}",
        rebuilt.warnings
    );
}

#[tokio::test]
async fn a_failing_call_never_renders_the_configured_key() {
    let factory = Factory::openai();
    let (_server, provider) = staged(&factory, Scenario::Authentication).await;
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a 401 is a failure");
    let renderings = [
        error.to_string(),
        format!("{error:?}"),
        format!("{provider:?}"),
    ];
    for rendering in &renderings {
        assert!(
            !rendering.contains(&payloads::DUMMY_API_KEY[..20]),
            "the credential surfaced: {rendering}"
        );
        // Nor does anything else the endpoint said: the prose of the error body
        // and the response headers are read, classified and dropped.
        assert!(
            !rendering.contains("Incorrect API key provided"),
            "the response body surfaced: {rendering}"
        );
    }
    // What survives is the kind, the keys and a sanitized machine code.
    assert_eq!(
        error.to_string(),
        format!("provider call failed: authentication [openai/{MODEL}] code=invalid_api_key")
    );
    assert_eq!(error.retry_class(), RetryClass::Fallback);

    // The endpoint's sentence is carried and rendered by neither `Display` nor
    // `Debug` — the two asserted above — and read deliberately instead. Without
    // it a malformed request this library sent and a provider that is down are
    // the same line to an adopter, and only one of them is theirs to fix.
    let detail = error.detail().expect("the endpoint said something");
    assert!(
        detail.as_str().contains("Incorrect API key provided"),
        "{detail}"
    );
    assert!(
        !detail.as_str().contains(&payloads::DUMMY_API_KEY[..20]),
        "and it goes through the same redactor the code does: {detail}"
    );
}

#[tokio::test]
async fn a_rate_limit_keeps_its_delay_and_a_quota_failure_does_not_pretend_to_be_one() {
    let factory = Factory::openai();
    let (_server, provider) = staged(&factory, Scenario::RateLimited).await;
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a 429 is a failure");
    assert_eq!(
        error.retry_after(),
        Some(std::time::Duration::from_secs(
            payloads::RETRY_AFTER_SECONDS
        ))
    );
    assert_eq!(error.retry_class(), RetryClass::RetryAfter);

    // The same status with an exhausted quota is not something to wait out.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(CHAT_PATH))
        .respond_with(ResponseTemplate::new(429).set_body_json(api_error(
            "insufficient_quota",
            "You exceeded your current quota, please check your plan and billing details.",
            "insufficient_quota",
        )))
        .mount(&server)
        .await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a quota failure is a failure");
    assert_eq!(error.retry_class(), RetryClass::Fallback);
}

#[tokio::test]
async fn a_streamed_tool_call_reassembles_into_the_non_streamed_answer() {
    let arguments = "{\"target\": \"tok_1\"}";
    let server = MockServer::start().await;
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "chatcmpl-1", "model": MODEL,
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let mut body = String::new();
    body.push_str(&sse(&chunk(
        json!({"role": "assistant", "tool_calls": [{
            "index": 0, "id": payloads::EXPECTED_CALL_ID, "type": "function",
            "function": {"name": payloads::TOOL_NAME, "arguments": ""}
        }]}),
        Value::Null,
    )));
    for fragment in [&arguments[..8], &arguments[8..]] {
        body.push_str(&sse(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": fragment}}]}),
            Value::Null,
        )));
    }
    body.push_str(&sse(&chunk(json!({}), json!("tool_calls"))));
    body.push_str("data: [DONE]\n\n");

    Mock::given(method("POST"))
        .and(path_regex(CHAT_PATH))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(CHAT_PATH))
        .respond_with(template(Scenario::ToolCallIds))
        .mount(&server)
        .await;

    let provider = Factory::openai()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::tool_request();
    let whole = provider.generate(request.clone()).await.expect("whole");
    let stream = provider.stream(request.clone()).await.expect("stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "openai", MODEL),
    )
    .await
    .expect("reassembles");

    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.finish, whole.finish);
    assert_eq!(
        rebuilt.tool_calls()[0].id.as_str(),
        payloads::EXPECTED_CALL_ID
    );
    assert_eq!(
        rebuilt.tool_calls()[0].arguments,
        json!({"target": "tok_1"})
    );
}

// ---------------------------------------------------------------------------
// The other two structured-output transports
// ---------------------------------------------------------------------------

/// The whole and streamed halves of one answer, mounted together.
///
/// The streaming mock matches on the flag the adapter puts on the wire, so the
/// non-streamed call falls through to the whole answer below it.
async fn mount_pair(server: &MockServer, whole: Value, streamed: String) {
    Mock::given(method("POST"))
        .and(path_regex(CHAT_PATH))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(streamed, "text/event-stream"))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(whole))
        .mount(server)
        .await;
}

/// The plan answered as one forced-function call, whole.
fn plan_as_tool_call() -> Value {
    json!({
        "id": "chatcmpl-turnframe-1",
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": payloads::EXPECTED_CALL_ID,
                    "type": "function",
                    "function": {
                        "name": payloads::SCHEMA_NAME,
                        "arguments": payloads::valid_plan().to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 12, "completion_tokens": 4}
    })
}

/// The same call, arriving in fragments.
fn plan_as_streamed_tool_call() -> String {
    let arguments = payloads::valid_plan().to_string();
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "chatcmpl-turnframe-1", "model": MODEL,
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let mut body = sse(&chunk(
        json!({"role": "assistant", "tool_calls": [{
            "index": 0, "id": payloads::EXPECTED_CALL_ID, "type": "function",
            "function": {"name": payloads::SCHEMA_NAME, "arguments": ""}
        }]}),
        Value::Null,
    ));
    let split = arguments.len() / 2;
    for fragment in [&arguments[..split], &arguments[split..]] {
        body.push_str(&sse(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": fragment}}]}),
            Value::Null,
        )));
    }
    body.push_str(&sse(&chunk(json!({}), json!("tool_calls"))));
    body.push_str(&sse(&json!({
        "id": "chatcmpl-turnframe-1", "choices": [],
        "usage": {"prompt_tokens": 12, "completion_tokens": 4}
    })));
    body.push_str("data: [DONE]\n\n");
    body
}

/// The plan answered as content, whole and streamed in two fragments.
fn plan_as_streamed_content() -> String {
    let payload = payloads::valid_plan().to_string();
    let split = payload.len() / 2;
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "chatcmpl-turnframe-1", "model": MODEL,
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let mut body = sse(&chunk(
        json!({"role": "assistant", "content": &payload[..split]}),
        Value::Null,
    ));
    body.push_str(&sse(&chunk(
        json!({"content": &payload[split..]}),
        Value::Null,
    )));
    body.push_str(&sse(&chunk(json!({}), json!("stop"))));
    body.push_str("data: [DONE]\n\n");
    body
}

/// The plan every transport must deliver, parsed through the suite's schema.
fn parse_plan(response: &ModelResponse) -> Value {
    let schema = CompiledSchema::compile(&payloads::plan_schema()).expect("the suite's schema");
    let plan: payloads::ConformancePlan =
        parse_structured(response, &schema).expect("a valid plan");
    serde_json::to_value(plan).expect("serializes")
}

/// Runs one structured call whole and streamed, returning both answers and the
/// bodies the endpoint saw.
async fn both_paths(
    factory: &Factory,
    whole: Value,
    streamed: String,
) -> (ModelResponse, ModelResponse, Vec<Value>) {
    let server = MockServer::start().await;
    mount_pair(&server, whole, streamed).await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::structured_request();
    let complete = provider.generate(request.clone()).await.expect("whole");
    let stream = provider.stream(request.clone()).await.expect("stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(
            request.request_id,
            provider.provider_key(),
            provider.model_key(),
        ),
    )
    .await
    .expect("reassembles");
    let bodies = server
        .received_requests()
        .await
        .expect("recording enabled")
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("a JSON body"))
        .collect();
    (complete, rebuilt, bodies)
}

#[tokio::test]
async fn a_forced_function_is_a_transport_on_both_paths_and_parses_the_same() {
    let factory = Factory::transport(StructuredOutputCapability::NativeFunctionSchema);
    let (whole, rebuilt, bodies) =
        both_paths(&factory, plan_as_tool_call(), plan_as_streamed_tool_call()).await;

    // Both requests pin the choice to exactly the function that carries the
    // schema, so the model has no other move than to fill it in.
    assert_eq!(bodies.len(), 2, "one whole call and one streamed call");
    for body in &bodies {
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["tools"][0]["function"]["name"], payloads::SCHEMA_NAME);
        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(
            body["tool_choice"]["function"]["name"],
            payloads::SCHEMA_NAME
        );
        assert_eq!(body["parallel_tool_calls"], false, "one document, one call");
        assert!(body.get("response_format").is_none());
        assert!(
            body.to_string().contains(payloads::SCHEMA_MARKER),
            "the schema itself must travel as the function's parameters"
        );
    }

    // The streamed answer reassembles into the whole one, field for field.
    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.finish, whole.finish);
    assert_eq!(rebuilt.usage, whole.usage);
    assert_eq!(parse_plan(&rebuilt), parse_plan(&whole));
}

#[tokio::test]
async fn the_forced_function_yields_what_the_schema_transport_would_have() {
    // The point of the transport: a caller cannot tell which one served it.
    let function = Factory::transport(StructuredOutputCapability::NativeFunctionSchema);
    let (through_function, _, _) =
        both_paths(&function, plan_as_tool_call(), plan_as_streamed_tool_call()).await;

    let schema = Factory::openai();
    let (through_schema, _, _) = both_paths(
        &schema,
        completion(&payloads::valid_plan().to_string(), "stop"),
        plan_as_streamed_content(),
    )
    .await;

    assert_eq!(parse_plan(&through_function), parse_plan(&through_schema));
    assert_eq!(parse_plan(&through_function), payloads::valid_plan());
}

#[tokio::test]
async fn a_forced_function_profile_passes_the_whole_suite() {
    let report = run(Factory::transport(
        StructuredOutputCapability::NativeFunctionSchema,
    ))
    .await;
    assert!(report.passed(), "{report}");
    let (_, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(skipped, 0, "{report}");
}

#[tokio::test]
async fn vllm_sends_the_guided_schema_on_both_paths() {
    let factory = Factory::runtime(Preset::Vllm);
    let (whole, rebuilt, bodies) = both_paths(
        &factory,
        completion(&payloads::valid_plan().to_string(), "stop"),
        plan_as_streamed_content(),
    )
    .await;

    assert_eq!(bodies.len(), 2);
    for body in &bodies {
        assert_eq!(body["guided_json"]["required"][0], "acts");
        assert!(
            body["guided_json"]
                .to_string()
                .contains(payloads::SCHEMA_MARKER),
            "the schema itself must travel"
        );
        assert!(body.get("grammar").is_none(), "that is the other dialect");
        assert!(body.get("response_format").is_none());
    }
    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.finish, whole.finish);
    assert_eq!(parse_plan(&rebuilt), parse_plan(&whole));
}

#[tokio::test]
async fn llama_cpp_sends_a_compiled_grammar_on_both_paths() {
    let factory = Factory::runtime(Preset::LlamaCpp);
    let (whole, rebuilt, bodies) = both_paths(
        &factory,
        completion(&payloads::valid_plan().to_string(), "stop"),
        plan_as_streamed_content(),
    )
    .await;

    assert_eq!(bodies.len(), 2);
    for body in &bodies {
        let grammar = body["grammar"].as_str().expect("a grammar");
        assert!(grammar.starts_with("root ::= "), "{grammar}");
        // Every property of the schema is named in the grammar, the marker
        // included: that is what proves the schema reached the wire.
        for name in ["acts", "operation", "target", payloads::SCHEMA_MARKER] {
            assert!(grammar.contains(name), "{name} missing from {grammar}");
        }
        assert!(body.get("guided_json").is_none(), "that is vLLM's field");
        assert!(body.get("response_format").is_none());
    }
    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.finish, whole.finish);
    assert_eq!(parse_plan(&rebuilt), parse_plan(&whole));
}

#[tokio::test]
async fn both_self_hosted_runtimes_pass_the_whole_suite_through_their_grammar() {
    for preset in [Preset::Vllm, Preset::LlamaCpp] {
        let report = run(Factory::runtime(preset)).await;
        assert!(report.passed(), "{preset}:\n{report}");
        let (_, failed, skipped) = report.counts();
        assert_eq!(failed, 0, "{preset}:\n{report}");
        assert_eq!(skipped, 0, "{preset}:\n{report}");
    }
}

#[tokio::test]
async fn a_schema_the_grammar_cannot_express_is_refused_before_the_wire() {
    let server = MockServer::start().await;
    Fixtures.mount(&server, Scenario::ValidStructured).await;
    let provider = Factory::runtime(Preset::LlamaCpp)
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    // A pattern is a value constraint no grammar can carry. Sending the
    // request without it would leave the profile claiming an enforcement the
    // wire never had.
    let request = payloads::structured_request().with_output(OutputSpec::json(
        "patterned",
        json!({
            "type": "object",
            "properties": {"code": {"type": "string", "pattern": "^[A-Z]+$"}},
            "required": ["code"]
        }),
    ));
    let error = provider.generate(request).await.expect_err("refused");
    assert!(error.to_string().contains("grammar_schema"), "{error}");
    assert_eq!(error.retry_class(), RetryClass::Fallback);
    assert!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .is_empty(),
        "nothing may reach the endpoint"
    );
}

// ---------------------------------------------------------------------------
// Streaming is incremental, not a buffered answer released at the end
// ---------------------------------------------------------------------------

/// Serves one server-sent-event response by hand, `gap` apart per chunk.
///
/// wiremock writes a body in one piece, which cannot tell an adapter that
/// streams from one that buffers and flushes at the end. This writes chunked
/// HTTP itself, so a test can watch a delta arrive while the answer is still
/// being written.
///
/// Returns the base URL and the writer task, which the caller aborts.
async fn serve_events_slowly(
    events: Vec<String>,
    gap: Duration,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let address = listener.local_addr().expect("a local address");
    let writer = tokio::spawn(async move {
        let Ok((mut socket, _peer)) = listener.accept().await else {
            return;
        };
        // Read to the end of the request head; the body follows and is not
        // needed to answer.
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match socket.read(&mut byte).await {
                Ok(0) | Err(_) => return,
                Ok(_) => head.push(byte[0]),
            }
        }
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                        Transfer-Encoding: chunked\r\n\r\n";
        if socket.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        for event in events {
            let framed = format!("{:x}\r\n{event}\r\n", event.len());
            if socket.write_all(framed.as_bytes()).await.is_err() {
                return;
            }
            let _ = socket.flush().await;
            tokio::time::sleep(gap).await;
        }
        let _ = socket.write_all(b"0\r\n\r\n").await;
        let _ = socket.flush().await;
    });
    (format!("http://{address}"), writer)
}

#[tokio::test]
async fn a_streamed_answer_arrives_in_pieces_while_it_is_still_being_written() {
    // An adapter that buffered the whole body and emitted one delta at the end
    // would satisfy the reconstruction row and give an adopter nothing.
    let gap = Duration::from_millis(150);
    let chunk = |delta: Value, finish: Value| {
        sse(&json!({"id": "chatcmpl-1", "model": MODEL,
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}))
    };
    let events = vec![
        chunk(json!({"role": "assistant", "content": "Ho "}), Value::Null),
        chunk(json!({"content": "preparato "}), Value::Null),
        chunk(json!({"content": "la modifica."}), Value::Null),
        format!("{}data: [DONE]\n\n", chunk(json!({}), json!("stop"))),
    ];
    let (base_url, writer) = serve_events_slowly(events, gap).await;

    let provider = Factory::openai()
        .build(&base_url, ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let started = Instant::now();
    let mut stream = provider
        .stream(payloads::narration_request())
        .await
        .expect("stream");
    let mut arrivals: Vec<(String, Duration)> = Vec::new();
    let mut finished_at = None;
    while let Some(item) = stream.next().await {
        match item.expect("no stream failure") {
            StreamEvent::TextDelta { text } => arrivals.push((text, started.elapsed())),
            StreamEvent::Finish { .. } => finished_at = Some(started.elapsed()),
            _ => {}
        }
    }
    writer.abort();

    // Three fragments on the wire, three deltas out — not one concatenation.
    let fragments: Vec<&str> = arrivals.iter().map(|(text, _)| text.as_str()).collect();
    assert_eq!(fragments, vec!["Ho ", "preparato ", "la modifica."]);
    // The first one is delivered long before the last is written.
    assert!(
        arrivals[0].1 < gap,
        "the first delta waited {:?}, which is the whole answer buffered",
        arrivals[0].1
    );
    // And the last one only after the endpoint had written it.
    assert!(arrivals[2].1 >= gap * 2, "{:?}", arrivals[2].1);
    assert!(finished_at.expect("a finish arrives") >= arrivals[2].1);
}

#[tokio::test]
async fn a_stream_that_dies_mid_answer_is_a_typed_failure_not_a_short_one() {
    let server = MockServer::start().await;
    let body = sse(&json!({
        "id": "chatcmpl-1",
        "choices": [{"index": 0, "delta": {"content": "meta "}, "finish_reason": null}]
    }));
    Mock::given(method("POST"))
        .and(path_regex(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider = Factory::openai()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::narration_request();
    let stream = provider.stream(request.clone()).await.expect("stream");
    let error = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "openai", MODEL),
    )
    .await
    .expect_err("a truncated stream is not a short answer");
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("stream_ended_without_finish".to_owned())
    );
}
