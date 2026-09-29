//! The conformance suite of spec §20.8, run against this adapter.
//!
//! This file is the deliverable that proves the crate. It runs the whole suite
//! from `turnframe_provider::conformance` — the twenty feature rows and the
//! thirteen per-status rows — against a wiremock server speaking the Bedrock
//! Runtime wire format, twice over:
//!
//! * a **measured** profile, with every capability declared — nothing may be
//!   skipped, because a skipped row is not a pass;
//! * a **modest** profile that declares `prompt_only` and no tools, whose
//!   fixtures answer with JSON in a text block rather than in a forced tool —
//!   it must *pass* while skipping the two rows it cannot honestly claim,
//!   because honesty is not a failure. It still declares streaming, because
//!   `ConverseStream` backs every Converse model.
//!
//! # How the SDK is pointed at a mock
//!
//! The harness hands the factory a base URL and a dummy credential, so the
//! traveler is built with `endpoint_url` pointing at wiremock and **static test
//! credentials** whose secret access key *is* the planted dummy key. That is
//! deliberate: it puts the credential exactly where a real deployment's secret
//! lives — inside the SigV4 signer — so the redaction row has something real to
//! hunt for. Retries, timeouts and stalled-stream protection are switched off in
//! the SDK, because the normalized layer owns retry policy and the suite's
//! timing rows measure this adapter rather than the SDK's backoff.
//!
//! The streaming fixture is a hand-encoded AWS **event stream**: length-prefixed
//! frames with a CRC-checked prelude and message, exactly as `ConverseStream`
//! puts them on the wire, so the SDK's own decoder is what parses them.
//!
//! **Conformance is per provider-model pair.** These runs say something about
//! this adapter against these fixtures. They say nothing about Llama versus
//! Claude behind the same Converse call.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_bedrockruntime::config::retry::RetryConfig;
use aws_sdk_bedrockruntime::config::timeout::TimeoutConfig;
use aws_sdk_bedrockruntime::config::{
    BehaviorVersion, Credentials, Region, StalledStreamProtectionConfig,
};
use serde_json::{Value, json};
use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::conformance::{
    Check, CheckStatus, ConformanceReport, ProviderFactory, Scenario, WireFixtures, payloads,
    run_all,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::prelude::*;
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::secret::ApiKey;
use turnframe_provider::stream::{StreamAccumulator, StreamEvent, reconstruct};
use turnframe_provider_bedrock::BedrockProvider;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The model every run is configured with.
const MODEL: &str = "anthropic.claude-sonnet-4-5-20250929-v1:0";

/// The region the test traveler signs for. Never contacted.
const REGION: &str = "us-east-1";

/// Matches the `Converse` path.
const CONVERSE_PATH: &str = r".*/converse$";

/// Matches the `ConverseStream` path.
const CONVERSE_STREAM_PATH: &str = r".*/converse-stream$";

/// The prose the streaming scenario answers with, whole and in fragments.
const NARRATION: &str = "Ho preparato la modifica.";

/// The two fragments the streamed twin sends it in.
const NARRATION_FRAGMENTS: [&str; 2] = ["Ho preparato ", "la modifica."];

/// Prompt tokens the fixtures report as *fresh*.
///
/// Converse's `inputTokens` leaves the cached ones out, and the normalized
/// `input` is the whole prompt, so this is the suite's prompt minus its cached
/// part.
const FRESH_PROMPT_TOKENS: u64 = payloads::USAGE_INPUT_TOKENS - payloads::USAGE_CACHED_TOKENS;

/// The request id every fixture answers with, in the header the SDK reads.
const AWS_REQUEST_ID: &str = "8f2c1d5e-turnframe-conformance";

/// The id every fixture's tool call carries, where the suite does not name one.
const TOOL_USE_ID: &str = "tooluse_turnframe_conformance";

/// The content type Bedrock labels an event-stream body with.
const EVENT_STREAM_CONTENT_TYPE: &str = "application/vnd.amazon.eventstream";

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// The capability set a measured Converse model earns.
fn measured() -> ProviderCapabilities {
    BedrockProvider::converse_defaults()
        .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
        .with_tool_calling(ToolCallingCapability::Parallel)
        .with_vision(true)
        .with_documents(true)
        .with_prompt_caching(true)
        .with_max_context_tokens(200_000)
}

/// A model nobody has measured for structured output: the schema is described
/// in the prompt, not enforced, and no tool is declared.
fn modest() -> ProviderCapabilities {
    BedrockProvider::converse_defaults()
        .with_structured_output(StructuredOutputCapability::PromptOnly)
}

/// Builds this adapter against a mock server, for one declaration.
struct Factory {
    capabilities: ProviderCapabilities,
}

impl ProviderFactory for Factory {
    type Provider = BedrockProvider;

    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        // The planted credential goes where a Bedrock secret really goes: into
        // the SigV4 signer, as the secret access key. It never reaches the wire
        // — only a signature derived from it does — and it must never reach a
        // rendering of the adapter either.
        let credentials = Credentials::new(
            "AKIATURNFRAMECONFORMANCE",
            api_key.expose(),
            Some("turnframe-conformance-session-token".to_owned()),
            None,
            "turnframe-conformance",
        );
        let config = aws_sdk_bedrockruntime::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(REGION))
            .credentials_provider(credentials)
            .endpoint_url(base_url)
            // The normalized layer owns retry policy (spec §20.7), and the
            // suite's timing rows measure this adapter, not the SDK's backoff.
            .retry_config(RetryConfig::disabled())
            .timeout_config(TimeoutConfig::disabled())
            .stalled_stream_protection(StalledStreamProtectionConfig::disabled())
            .build();
        BedrockProvider::builder()
            .service_config(config)
            .model(MODEL)
            .capabilities(self.capabilities.clone())
            .region(REGION)
            .build()
            .map_err(ProviderError::from)
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// How a structured payload comes back.
///
/// A profile that declares the forced-tool transport is answered with a
/// `toolUse` block, which is what such a model really sends; a `prompt_only`
/// one is answered with the JSON in a text block, which is all a non-enforcing
/// model can manage. Measuring both is the point: the runtime reads the
/// document out of either shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuredShape {
    ToolUse,
    Text,
}

struct Fixtures {
    shape: StructuredShape,
}

impl Fixtures {
    const fn tool_use() -> Self {
        Self {
            shape: StructuredShape::ToolUse,
        }
    }

    const fn text() -> Self {
        Self {
            shape: StructuredShape::Text,
        }
    }

    /// The answer a structured scenario comes back with, in this fixture's
    /// shape.
    fn structured(&self, payload: &Value) -> Value {
        match self.shape {
            StructuredShape::ToolUse => converse(
                json!([{
                    "toolUse": {
                        "toolUseId": TOOL_USE_ID,
                        "name": payloads::SCHEMA_NAME,
                        "input": payload
                    }
                }]),
                "tool_use",
            ),
            StructuredShape::Text => text_answer(&payload.to_string(), "end_turn"),
        }
    }
}

/// The token counts every successful fixture reports.
///
/// Converse's `inputTokens` **excludes** the cached ones, so these are the
/// numbers whose gross total is the suite's prompt: the fresh half plus
/// [`payloads::USAGE_CACHED_TOKENS`] read from the cache.
fn usage() -> Value {
    json!({
        "inputTokens": FRESH_PROMPT_TOKENS,
        "outputTokens": payloads::USAGE_OUTPUT_TOKENS,
        "totalTokens": payloads::USAGE_INPUT_TOKENS + payloads::USAGE_OUTPUT_TOKENS,
        "cacheReadInputTokens": payloads::USAGE_CACHED_TOKENS
    })
}

/// A completed Converse answer.
fn converse(content: Value, stop_reason: &str) -> Value {
    json!({
        "output": {"message": {"role": "assistant", "content": content}},
        "stopReason": stop_reason,
        "usage": usage(),
        "metrics": {"latencyMs": 12}
    })
}

/// A completed answer carrying one text block.
fn text_answer(text: &str, stop_reason: &str) -> Value {
    converse(json!([{"text": text}]), stop_reason)
}

/// An error body in the shape the Bedrock Runtime returns.
fn api_error(message: &str) -> Value {
    json!({"message": message})
}

/// An error response: the status, the AWS error code in its header, and the
/// message in the body — exactly how the SDK reads one.
fn error_response(status: u16, error_type: &str, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("x-amzn-errortype", error_type)
        .insert_header("x-amzn-requestid", "req-turnframe-conformance")
        .set_body_json(api_error(message))
}

/// The template each scenario answers `Converse` with.
fn template(fixtures: &Fixtures, scenario: Scenario) -> ResponseTemplate {
    match scenario {
        Scenario::ValidStructured => {
            ResponseTemplate::new(200).set_body_json(fixtures.structured(&payloads::valid_plan()))
        }
        // Not JSON at all, so it can only come back as prose — which is exactly
        // the shape a model that ignored its transport produces.
        Scenario::MalformedJson => ResponseTemplate::new(200)
            .set_body_json(text_answer(payloads::MALFORMED_JSON, "end_turn")),
        Scenario::UnknownField => ResponseTemplate::new(200)
            .set_body_json(fixtures.structured(&payloads::plan_with_unknown_field())),
        Scenario::MissingField => ResponseTemplate::new(200)
            .set_body_json(fixtures.structured(&payloads::plan_with_missing_field())),
        Scenario::MultipleActs => {
            ResponseTemplate::new(200).set_body_json(fixtures.structured(&payloads::two_act_plan()))
        }
        Scenario::ToolCallIds => ResponseTemplate::new(200).set_body_json(converse(
            json!([{
                "toolUse": {
                    "toolUseId": payloads::EXPECTED_CALL_ID,
                    "name": payloads::TOOL_NAME,
                    "input": {"target": "tok_1"}
                }
            }]),
            "tool_use",
        )),
        Scenario::StreamingReconstruction => {
            ResponseTemplate::new(200).set_body_json(text_answer(NARRATION, "end_turn"))
        }
        // Mounted for the usage row: `usage` already reports the fresh half and
        // the cached half of the same prompt, which is what that row reads.
        Scenario::CachedUsage => {
            ResponseTemplate::new(200).set_body_json(text_answer(NARRATION, "end_turn"))
        }
        // The shape an exhausted or filtered answer really takes: a successful
        // status with an empty content array, not an empty HTTP body.
        Scenario::EmptyOutput => {
            ResponseTemplate::new(200).set_body_json(converse(json!([]), "end_turn"))
        }
        // A model family that declines passes its own label through Converse.
        Scenario::Refusal => {
            ResponseTemplate::new(200).set_body_json(text_answer(payloads::REFUSAL_TEXT, "refusal"))
        }
        // A guardrail is a filter, not a refusal: a different stop reason, and
        // a different normalized answer.
        Scenario::ContentFilter => ResponseTemplate::new(200)
            .set_body_json(text_answer(payloads::REFUSAL_TEXT, "guardrail_intervened")),
        Scenario::SlowResponse => ResponseTemplate::new(200)
            .set_body_json(text_answer("troppo tardi", "end_turn"))
            .set_delay(payloads::SLOW_RESPONSE_DELAY),
        Scenario::RateLimited => error_response(
            429,
            "ThrottlingException",
            "Too many requests, please wait before trying again.",
        )
        .insert_header(
            "retry-after",
            payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
        ),
        // A gateway in front of Bedrock answers 401 for a credential it does
        // not recognize; AWS itself answers 403 with the same code.
        Scenario::Authentication => error_response(
            401,
            "UnrecognizedClientException",
            "The security token included in the request is invalid.",
        ),
        Scenario::ContextOverflow => error_response(
            400,
            "ValidationException",
            "Input is too long for requested model: 215048 tokens > 199999 maximum",
        ),
        Scenario::Authorization => error_response(
            403,
            "AccessDeniedException",
            "You don't have access to the model with the specified model ID.",
        ),
        Scenario::ModelNotFound => error_response(
            404,
            "ResourceNotFoundException",
            "The provided model identifier is invalid.",
        ),
        Scenario::RequestTimeout => error_response(
            408,
            "ModelTimeoutException",
            "The request took too long to process.",
        ),
        // A 400 no reasonable adapter could mistake for a context problem.
        Scenario::InvalidRequest => error_response(
            400,
            "ValidationException",
            "1 validation error detected: Value at 'toolConfig.tools' failed to satisfy constraint",
        ),
        Scenario::ServerError => error_response(
            500,
            "InternalServerException",
            "An internal server error occurred.",
        ),
        Scenario::ServiceUnavailable => error_response(
            503,
            "ServiceUnavailableException",
            "The service isn't currently available.",
        ),
        // A session token that was valid: the same 403 family a wrong key
        // produces, told apart only by the code.
        Scenario::ExpiredCredential => error_response(
            403,
            "ExpiredTokenException",
            "The security token included in the request is expired.",
        ),
        // A spent quota on a 429 — the status of a rate limit, and nothing a
        // caller can wait out.
        Scenario::QuotaExhausted => error_response(
            429,
            "ServiceQuotaExceededException",
            "Your request exceeded the service quota for this account.",
        )
        .insert_header(
            "retry-after",
            payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
        ),
        // The credential is echoed back where a careless proxy puts it: in a
        // debug envelope field and in a response header. Neither is read by the
        // adapter, and neither may survive into any rendering of it.
        Scenario::SecretInBody => {
            let mut body = fixtures.structured(&payloads::valid_plan());
            body["_debug"] =
                json!({"authorization": format!("Bearer {}", payloads::DUMMY_API_KEY)});
            ResponseTemplate::new(200)
                .insert_header(
                    "x-upstream-authorization",
                    format!("Bearer {}", payloads::DUMMY_API_KEY).as_str(),
                )
                .set_body_json(body)
        }
        // The scenario set is growable; a scenario this fixture does not model
        // answers with an empty message, which every check reads as a failure
        // rather than as a pass.
        _ => ResponseTemplate::new(200).set_body_json(converse(json!([]), "end_turn")),
    }
}

#[async_trait]
impl WireFixtures for Fixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        if scenario == Scenario::StreamingReconstruction {
            Mock::given(method("POST"))
                .and(path_regex(CONVERSE_STREAM_PATH))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", EVENT_STREAM_CONTENT_TYPE)
                        // The header the SDK reads the request id out of. It is
                        // the identifier a support ticket quotes, and both paths
                        // must report it.
                        .insert_header("x-amzn-requestid", AWS_REQUEST_ID)
                        .set_body_raw(narration_stream(), EVENT_STREAM_CONTENT_TYPE),
                )
                .mount(server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path_regex(CONVERSE_PATH))
            .respond_with(
                template(self, scenario).insert_header("x-amzn-requestid", AWS_REQUEST_ID),
            )
            .mount(server)
            .await;
    }
}

// ---------------------------------------------------------------------------
// The AWS event stream, encoded by hand
// ---------------------------------------------------------------------------

/// The streamed twin of the narration answer, in Bedrock's own framing.
///
/// The counts in the trailing `metadata` event are the ones the non-streamed
/// answer reports, so the two paths can be compared on usage and not only on
/// content.
fn narration_stream() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(event("messageStart", &json!({"role": "assistant"})));
    for fragment in NARRATION_FRAGMENTS {
        body.extend(event(
            "contentBlockDelta",
            &json!({"delta": {"text": fragment}, "contentBlockIndex": 0}),
        ));
    }
    body.extend(event("contentBlockStop", &json!({"contentBlockIndex": 0})));
    body.extend(event("messageStop", &json!({"stopReason": "end_turn"})));
    body.extend(event(
        "metadata",
        &json!({"usage": usage(), "metrics": {"latencyMs": 12}}),
    ));
    body
}

/// One event-stream message: three headers and a JSON payload.
fn event(event_type: &str, payload: &Value) -> Vec<u8> {
    let mut headers = Vec::new();
    header(&mut headers, ":event-type", event_type);
    header(&mut headers, ":content-type", "application/json");
    header(&mut headers, ":message-type", "event");
    frame(&headers, payload.to_string().as_bytes())
}

/// One string header: a length-prefixed name, the type tag, a length-prefixed
/// value.
fn header(out: &mut Vec<u8>, name: &str, value: &str) {
    out.push(u8::try_from(name.len()).expect("a short header name"));
    out.extend_from_slice(name.as_bytes());
    // 7 is the event-stream type tag for a string.
    out.push(7);
    out.extend_from_slice(
        &u16::try_from(value.len())
            .expect("a short value")
            .to_be_bytes(),
    );
    out.extend_from_slice(value.as_bytes());
}

/// Wraps headers and a payload in the length-and-CRC framing.
///
/// The prelude checksum covers the two lengths; the message checksum covers
/// everything before it, the prelude checksum included.
fn frame(headers: &[u8], payload: &[u8]) -> Vec<u8> {
    let total = u32::try_from(12 + headers.len() + payload.len() + 4).expect("a small frame");
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(
        &u32::try_from(headers.len())
            .expect("short headers")
            .to_be_bytes(),
    );
    out.extend_from_slice(&crc32(&out).to_be_bytes());
    out.extend_from_slice(headers);
    out.extend_from_slice(payload);
    let checksum = crc32(&out);
    out.extend_from_slice(&checksum.to_be_bytes());
    out
}

/// CRC-32, the reflected IEEE polynomial the event stream uses.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// The runs
// ---------------------------------------------------------------------------

async fn run(capabilities: ProviderCapabilities, fixtures: Fixtures) -> ConformanceReport {
    run_all(&Factory { capabilities }, &fixtures).await
}

/// Every row a full run reports, so a count is checked against the suite rather
/// than against a number that would rot.
fn every_row() -> Vec<Check> {
    Check::run_order()
}

#[tokio::test]
async fn a_measured_profile_passes_every_row_without_skipping_one() {
    let report = run(measured(), Fixtures::tool_use()).await;
    assert!(report.passed(), "{report}");
    assert_eq!(report.provider.as_str(), "bedrock");
    assert_eq!(report.model.as_str(), MODEL);
    let (passed, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(
        skipped, 0,
        "every Converse failure shape is producible, and a skip is not a pass:\n{report}"
    );
    assert_eq!(passed, every_row().len());
    let order: Vec<Check> = report.results.iter().map(|result| result.check).collect();
    assert_eq!(order, every_row());
}

#[tokio::test]
async fn a_modest_profile_passes_by_skipping_what_it_cannot_claim() {
    let report = run(modest(), Fixtures::text()).await;
    assert!(report.passed(), "honesty is not a failure:\n{report}");
    for check in [
        Check::ToolAndReadRequestIds,
        Check::NoSilentCapabilityDowngrade,
    ] {
        assert_eq!(
            report.result(check).expect("check ran").status,
            CheckStatus::Skipped,
            "{check} cannot be proven by a prompt_only, tool-less profile:\n{report}"
        );
    }
    // Streaming is *not* among them: ConverseStream backs every Converse model,
    // so a profile that declared no streaming would be claiming less than the
    // API gives it.
    assert_eq!(
        report
            .result(Check::StreamingReconstruction)
            .expect("check ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
    let (_, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(skipped, 2, "{report}");
}

// ---------------------------------------------------------------------------
// What actually went on the wire
// ---------------------------------------------------------------------------

/// Builds a provider against a fresh server with `scenario` mounted.
async fn staged(
    capabilities: ProviderCapabilities,
    fixtures: &Fixtures,
    scenario: Scenario,
) -> (MockServer, BedrockProvider) {
    let server = MockServer::start().await;
    fixtures.mount(&server, scenario).await;
    let provider = Factory { capabilities }
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
async fn a_schema_enforcing_profile_puts_the_schema_in_a_forced_tool() {
    let (server, provider) =
        staged(measured(), &Fixtures::tool_use(), Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;

    // The transport is one tool whose input schema *is* the required schema…
    let tools = body["toolConfig"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["toolSpec"]["name"], payloads::SCHEMA_NAME);
    assert_eq!(
        tools[0]["toolSpec"]["inputSchema"]["json"],
        payloads::plan_schema(),
        "the schema itself must travel, not just its name"
    );
    // …and the choice is pinned to it, so the document is the only answer.
    assert_eq!(
        body["toolConfig"]["toolChoice"]["tool"]["name"],
        payloads::SCHEMA_NAME
    );
    // The system prompt is not a turn.
    assert!(body["system"].is_array());
    assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
}

#[tokio::test]
async fn a_prompt_only_profile_forces_nothing_and_says_so_in_the_prompt() {
    let (server, provider) = staged(modest(), &Fixtures::text(), Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;
    assert!(
        body.get("toolConfig").is_none(),
        "a prompt_only profile declares no tool at all"
    );
    let system = body["system"][0]["text"].as_str().expect("a system block");
    assert!(system.contains(payloads::SCHEMA_MARKER));
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Collects the events a streamed narration produces.
async fn streamed_events(provider: &BedrockProvider, request: ModelRequest) -> Vec<StreamEvent> {
    let stream = provider.stream(request).await.expect("a stream");
    stream
        .collect_items()
        .await
        .into_iter()
        .map(|item| item.expect("no failure in the stream"))
        .collect()
}

#[tokio::test]
async fn the_streamed_path_reports_the_feature_it_dropped_just_as_the_whole_one_does() {
    // Converse takes four stop sequences. A fifth is dropped, and until the
    // stream had a warning event the whole call said so and the streamed call
    // said nothing about the same request.
    let (_server, provider) = staged(
        measured(),
        &Fixtures::tool_use(),
        Scenario::StreamingReconstruction,
    )
    .await;
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

    let events = streamed_events(&provider, request).await;
    assert!(
        events.contains(&StreamEvent::warning(dropped)),
        "the streamed path must say what it gave up: {events:?}"
    );
}

#[tokio::test]
async fn the_stream_delivers_prose_in_as_many_deltas_as_the_wire_sent() {
    let (_server, provider) = staged(
        measured(),
        &Fixtures::tool_use(),
        Scenario::StreamingReconstruction,
    )
    .await;
    let events = streamed_events(&provider, payloads::narration_request()).await;
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas, NARRATION_FRAGMENTS,
        "a stream that buffers the whole answer and emits it once gives an \
         adopter nothing that a plain call did not already give them"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::Usage { .. })),
        "the metadata event must reach the caller"
    );
}

#[tokio::test]
async fn the_streamed_and_whole_paths_agree_on_content_finish_and_usage() {
    let (_server, provider) = staged(
        measured(),
        &Fixtures::tool_use(),
        Scenario::StreamingReconstruction,
    )
    .await;
    let request = payloads::narration_request();
    let whole = provider
        .generate(request.clone())
        .await
        .expect("the non-streamed call");
    let stream = provider.stream(request.clone()).await.expect("a stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(
            request.request_id,
            provider.provider_key(),
            provider.model_key(),
        ),
    )
    .await
    .expect("the stream reassembles");

    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.text(), NARRATION);
    assert_eq!(rebuilt.finish, whole.finish);
    assert_eq!(
        rebuilt.usage, whole.usage,
        "the counts a streamed answer reports must be the counts the same \
         answer reports whole"
    );
    assert_eq!(
        whole.usage.input,
        payloads::USAGE_INPUT_TOKENS,
        "the cache counters are folded back in"
    );
    assert_eq!(whole.usage.cached_input, payloads::USAGE_CACHED_TOKENS);
    assert_eq!(whole.usage.output, payloads::USAGE_OUTPUT_TOKENS);
    // The AWS request id rides in the stream, so the rebuilt answer carries the
    // identifier the whole one does without anyone seeding it.
    assert_eq!(rebuilt.raw_id, whole.raw_id);
    assert_eq!(rebuilt.raw_id.as_deref(), Some(AWS_REQUEST_ID));
}

#[tokio::test]
async fn a_profile_that_declares_no_streaming_refuses_instead_of_faking_one() {
    let silent = measured().with_streaming(false);
    let (_server, provider) = staged(
        silent,
        &Fixtures::tool_use(),
        Scenario::StreamingReconstruction,
    )
    .await;
    let error = provider
        .stream(payloads::narration_request())
        .await
        .expect_err("a declaration of no streaming is honoured");
    assert!(matches!(
        error.kind(),
        turnframe_provider::error::ProviderErrorKind::Unsupported { .. }
    ));
}

// ---------------------------------------------------------------------------
// The credential
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_configured_credential_appears_in_no_rendering_of_anything() {
    let (_server, provider) =
        staged(measured(), &Fixtures::tool_use(), Scenario::SecretInBody).await;
    let response = provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let failing = staged(measured(), &Fixtures::tool_use(), Scenario::Authentication).await;
    let error = failing
        .1
        .generate(payloads::narration_request())
        .await
        .expect_err("a 401 fails");

    let renderings = [
        format!("{provider:?}"),
        format!("{response:?}"),
        format!("{:?}", provider.profile()),
        error.to_string(),
        format!("{error:?}"),
    ];
    // A prefix is enough: a truncated key is still a leak.
    let needle = &payloads::DUMMY_API_KEY[..20];
    for rendering in &renderings {
        assert!(
            !rendering.contains(needle),
            "the credential leaked into a rendering"
        );
    }
}

#[tokio::test]
async fn the_expired_session_and_the_spent_quota_are_told_apart_from_their_neighbours() {
    for (scenario, expected) in [
        (Scenario::ExpiredCredential, "credential_expired"),
        (Scenario::QuotaExhausted, "quota_exhausted"),
        (Scenario::Authentication, "authentication"),
        (Scenario::RateLimited, "rate_limited"),
        (Scenario::Authorization, "authorization"),
    ] {
        let (_server, provider) = staged(measured(), &Fixtures::tool_use(), scenario).await;
        let error = provider
            .generate(payloads::narration_request())
            .await
            .expect_err("every one of these fixtures fails");
        assert_eq!(
            error.kind().as_str(),
            expected,
            "{scenario} must map to {expected}"
        );
    }
}

#[tokio::test]
async fn a_deadline_that_passes_is_a_timeout_and_not_a_transport_failure() {
    let (_server, provider) =
        staged(measured(), &Fixtures::tool_use(), Scenario::SlowResponse).await;
    let request = payloads::narration_request().with_timeout(Duration::from_millis(120));
    let error = provider
        .generate(request)
        .await
        .expect_err("the fixture outlasts the deadline");
    assert_eq!(error.kind().as_str(), "timeout");
}
