//! The conformance suite of spec §20.8, run against this adapter.
//!
//! This file is the deliverable that proves the crate. It runs the whole suite
//! from `turnframe_provider::conformance` — the twenty feature rows and the
//! thirteen per-status rows — against a wiremock server speaking the Messages
//! API, three times over:
//!
//! * the **Anthropic** profile, with every capability declared — nothing may be
//!   skipped, because a skipped row is not a pass;
//! * a **measured proxy** profile: a different base URL, a bearer credential
//!   and the same declaration, which must pass the same rows;
//! * a **modest** profile that declares `prompt_only` and no tools, whose
//!   fixtures answer with JSON in a text block rather than in a forced tool —
//!   it must *pass* while skipping the rows it cannot honestly claim, because
//!   honesty is not a failure.
//!
//! Everything the fixtures return is Anthropic framing around the suite's own
//! corpus: the schema, the payloads and the planted credential all come from
//! `payloads`, so this adapter is measured on the same thing every other
//! adapter is.
//!
//! **Conformance is per provider-model pair.** These runs say something about
//! this adapter against these fixtures. They say nothing about Sonnet versus
//! Haiku, and nothing at all about a proxy nobody has measured.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::conformance::{
    Check, CheckStatus, ConformanceReport, ProviderFactory, RowSupport, Scenario, WireFixtures,
    payloads, run_all,
};
use turnframe_provider::error::{ProviderError, ProviderErrorKind};
use turnframe_provider::prelude::*;
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::secret::ApiKey;
use turnframe_provider::stream::{StreamAccumulator, reconstruct};
use turnframe_provider_anthropic::profile::{
    ANTHROPIC_BASE_URL, API_KEY_HEADER, BETA_HEADER, DEFAULT_ANTHROPIC_VERSION, VERSION_HEADER,
};
use turnframe_provider_anthropic::{AnthropicProvider, AuthScheme, EndpointProfile};
use wiremock::matchers::{body_string_contains, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The model every run is configured with.
const MODEL: &str = "claude-sonnet-4-5-20250929";

/// Matches the Messages path.
const MESSAGES_PATH: &str = r".*/v1/messages$";

/// The prose the streaming scenario answers with, whole and in fragments.
const NARRATION: &str = "Ho preparato la modifica.";

/// The id every fixture's tool call carries, where the suite does not name one.
const TOOL_USE_ID: &str = "toolu_turnframe_conformance";

/// Prompt tokens the fixtures report as *fresh*.
///
/// Anthropic's `input_tokens` leaves the cached ones out, and the normalized
/// `input` is the whole prompt, so this is the suite's prompt minus its cached
/// part. Getting it from the constants rather than writing 12 is the point: the
/// usage row compares against the gross figure.
const FRESH_PROMPT_TOKENS: u64 = payloads::USAGE_INPUT_TOKENS - payloads::USAGE_CACHED_TOKENS;

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Builds this adapter against a mock server, for one profile.
struct Factory {
    profile: EndpointProfile,
    capabilities: Option<ProviderCapabilities>,
}

/// The capability set a measured Messages endpoint earns.
fn measured() -> ProviderCapabilities {
    ProviderCapabilities::minimal()
        .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
        .with_tool_calling(ToolCallingCapability::Parallel)
        .with_vision(true)
        .with_streaming(true)
        .with_prompt_caching(true)
        .with_preserves_call_ids(true)
        .with_max_context_tokens(200_000)
}

impl Factory {
    fn anthropic() -> Self {
        Self {
            profile: EndpointProfile::anthropic(),
            capabilities: None,
        }
    }

    /// A proxy someone measured: bearer auth, its own base URL, same behaviour.
    fn proxy() -> Self {
        Self {
            profile: EndpointProfile::compatible("claude-proxy").with_auth(AuthScheme::Bearer),
            capabilities: Some(measured()),
        }
    }

    /// An endpoint nobody has measured: the schema is described, not enforced.
    fn modest() -> Self {
        Self {
            profile: EndpointProfile::compatible("claude-unmeasured"),
            capabilities: Some(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::PromptOnly),
            ),
        }
    }
}

impl ProviderFactory for Factory {
    type Provider = AnthropicProvider;

    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        let mut builder = AnthropicProvider::builder(self.profile.clone())
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

/// How a structured payload comes back.
///
/// A profile that declares the forced-tool transport is answered with a
/// `tool_use` block, which is what such an endpoint really sends; a
/// `prompt_only` one is answered with the JSON in a text block, which is all a
/// non-enforcing endpoint can manage. Measuring both is the point: the runtime
/// reads the document out of either shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuredShape {
    ToolUse,
    Text,
}

struct Fixtures {
    shape: StructuredShape,
    /// Whether this endpoint implements the streaming route at all.
    ///
    /// The unmeasured reimplementation does not, and says so in words rather
    /// than letting `streaming: false` make three rows disappear from the
    /// table.
    streams: bool,
}

impl Fixtures {
    const fn tool_use() -> Self {
        Self {
            shape: StructuredShape::ToolUse,
            streams: true,
        }
    }

    const fn text() -> Self {
        Self {
            shape: StructuredShape::Text,
            streams: false,
        }
    }

    /// The answer a structured scenario comes back with, in this fixture's
    /// shape.
    fn structured(&self, payload: &Value) -> Value {
        match self.shape {
            StructuredShape::ToolUse => message(
                json!([{
                    "type": "tool_use",
                    "id": TOOL_USE_ID,
                    "name": payloads::SCHEMA_NAME,
                    "input": payload
                }]),
                "tool_use",
            ),
            StructuredShape::Text => text_message(&payload.to_string(), "end_turn"),
        }
    }
}

/// A completed Messages answer.
fn message(content: Value, stop_reason: &str) -> Value {
    json!({
        "id": "msg_01Turnframe",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        // The Messages API's `input_tokens` **excludes** the cached ones, so
        // these are the numbers whose gross total is the suite's prompt:
        // 12 fresh + 30 read from the cache = payloads::USAGE_INPUT_TOKENS.
        "usage": {
            "input_tokens": FRESH_PROMPT_TOKENS,
            "output_tokens": payloads::USAGE_OUTPUT_TOKENS,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": payloads::USAGE_CACHED_TOKENS
        }
    })
}

/// A completed answer carrying one text block.
fn text_message(text: &str, stop_reason: &str) -> Value {
    message(json!([{"type": "text", "text": text}]), stop_reason)
}

/// An error body in the shape the Messages API returns.
fn api_error(error_type: &str, message: &str) -> Value {
    json!({
        "type": "error",
        "request_id": "req_011CTurnframe",
        "error": {"type": error_type, "message": message}
    })
}

/// One server-sent event, named the way the Messages API names it.
fn sse(payload: &Value) -> String {
    let name = payload["type"].as_str().unwrap_or("message");
    format!("event: {name}\ndata: {payload}\n\n")
}

/// The streamed twin of the narration answer.
fn narration_stream() -> String {
    let mut body = String::new();
    body.push_str(&sse(&json!({
        "type": "message_start",
        "message": {
            "id": "msg_01Turnframe", "type": "message", "role": "assistant",
            "model": MODEL, "content": [], "stop_reason": null,
            "usage": {"input_tokens": FRESH_PROMPT_TOKENS, "output_tokens": 1,
                      "cache_creation_input_tokens": 0,
                      "cache_read_input_tokens": payloads::USAGE_CACHED_TOKENS}
        }
    })));
    body.push_str(&sse(&json!({
        "type": "content_block_start", "index": 0,
        "content_block": {"type": "text", "text": ""}
    })));
    body.push_str(&sse(&json!({"type": "ping"})));
    for fragment in ["Ho preparato ", "la modifica."] {
        body.push_str(&sse(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": fragment}
        })));
    }
    body.push_str(&sse(&json!({"type": "content_block_stop", "index": 0})));
    body.push_str(&sse(&json!({
        "type": "message_delta",
        "delta": {"stop_reason": "end_turn", "stop_sequence": null},
        "usage": {"output_tokens": payloads::USAGE_OUTPUT_TOKENS}
    })));
    body.push_str(&sse(&json!({"type": "message_stop"})));
    body
}

/// The template each scenario answers with, once streaming is out of the way.
fn template(fixtures: &Fixtures, scenario: Scenario) -> ResponseTemplate {
    match scenario {
        Scenario::ValidStructured => {
            ResponseTemplate::new(200).set_body_json(fixtures.structured(&payloads::valid_plan()))
        }
        // Not JSON at all, so it can only come back as prose — which is exactly
        // the shape a model that ignored its transport produces.
        Scenario::MalformedJson => ResponseTemplate::new(200)
            .set_body_json(text_message(payloads::MALFORMED_JSON, "end_turn")),
        Scenario::UnknownField => ResponseTemplate::new(200)
            .set_body_json(fixtures.structured(&payloads::plan_with_unknown_field())),
        Scenario::MissingField => ResponseTemplate::new(200)
            .set_body_json(fixtures.structured(&payloads::plan_with_missing_field())),
        Scenario::MultipleActs => {
            ResponseTemplate::new(200).set_body_json(fixtures.structured(&payloads::two_act_plan()))
        }
        Scenario::ToolCallIds => ResponseTemplate::new(200).set_body_json(message(
            json!([{
                "type": "tool_use",
                "id": payloads::EXPECTED_CALL_ID,
                "name": payloads::TOOL_NAME,
                "input": {"target": "tok_1"}
            }]),
            "tool_use",
        )),
        Scenario::StreamingReconstruction => {
            ResponseTemplate::new(200).set_body_json(text_message(NARRATION, "end_turn"))
        }
        // Mounted for the usage row: `message` already reports the fresh half
        // and the cached half of the same prompt, which is what it reads.
        Scenario::CachedUsage => {
            ResponseTemplate::new(200).set_body_json(text_message(NARRATION, "end_turn"))
        }
        // The shape an exhausted or filtered answer really takes: a successful
        // status with an empty content array, not an empty HTTP body.
        Scenario::EmptyOutput => {
            ResponseTemplate::new(200).set_body_json(message(json!([]), "end_turn"))
        }
        // A safety intervention surfaces as a *successful* message whose stop
        // reason says the model declined — the Messages API has no error type
        // for it. The same shape serves the content-filter row.
        Scenario::Refusal | Scenario::ContentFilter => ResponseTemplate::new(200)
            .set_body_json(text_message(payloads::REFUSAL_TEXT, "refusal")),
        Scenario::SlowResponse => ResponseTemplate::new(200)
            .set_body_json(text_message("troppo tardi", "end_turn"))
            .set_delay(payloads::SLOW_RESPONSE_DELAY),
        Scenario::RateLimited => ResponseTemplate::new(429)
            .insert_header(
                "retry-after",
                payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
            )
            .set_body_json(api_error(
                "rate_limit_error",
                "Number of request tokens has exceeded your per-minute rate limit",
            )),
        Scenario::Authentication => ResponseTemplate::new(401)
            .set_body_json(api_error("authentication_error", "invalid x-api-key")),
        Scenario::ContextOverflow => ResponseTemplate::new(400).set_body_json(api_error(
            "invalid_request_error",
            "prompt is too long: 215048 tokens > 199999 maximum",
        )),
        Scenario::Authorization => ResponseTemplate::new(403).set_body_json(api_error(
            "permission_error",
            "Your API key does not have permission to use the specified resource",
        )),
        Scenario::ModelNotFound => ResponseTemplate::new(404)
            .set_body_json(api_error("not_found_error", "model: claude-nonexistent")),
        Scenario::RequestTimeout => ResponseTemplate::new(408)
            .set_body_json(api_error("timeout_error", "Request timed out")),
        // A 400 no reasonable adapter could mistake for a context problem.
        Scenario::InvalidRequest => ResponseTemplate::new(400).set_body_json(api_error(
            "invalid_request_error",
            "tools.0.input_schema: Field required",
        )),
        Scenario::ServerError => ResponseTemplate::new(500)
            .set_body_json(api_error("api_error", "Internal server error")),
        Scenario::ServiceUnavailable => ResponseTemplate::new(503)
            .set_body_json(api_error("api_error", "Service temporarily unavailable")),
        // A key that was valid: 401, like a wrong one, and only the message
        // tells them apart.
        Scenario::ExpiredCredential => ResponseTemplate::new(401).set_body_json(api_error(
            "authentication_error",
            "This API key has expired. Please generate a new key.",
        )),
        // A spent balance on a 429 — the status of a rate limit, and nothing a
        // caller can wait out.
        Scenario::QuotaExhausted => ResponseTemplate::new(429)
            .insert_header(
                "retry-after",
                payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
            )
            .set_body_json(api_error(
                "rate_limit_error",
                "Your credit balance is too low to access the Anthropic API. \
                 Please go to Plans & Billing to upgrade or purchase credits.",
            )),
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
        _ => ResponseTemplate::new(200).set_body_json(message(json!([]), "end_turn")),
    }
}

#[async_trait]
impl WireFixtures for Fixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        if scenario == Scenario::StreamingReconstruction {
            // Mounted first, and matched on the flag the adapter puts on the
            // wire, so the non-streamed call falls through to the twin below.
            Mock::given(method("POST"))
                .and(path_regex(MESSAGES_PATH))
                .and(body_string_contains("\"stream\":true"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(narration_stream(), "text/event-stream"),
                )
                .mount(server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path_regex(MESSAGES_PATH))
            .respond_with(template(self, scenario))
            .mount(server)
            .await;
    }

    /// The rows an endpoint with no streaming route cannot put on the wire.
    ///
    /// Declared in words, so the report calls them unproven with a reason
    /// instead of passing them over. The measured runs declare nothing, so all
    /// three are exercised there.
    fn feature_support(&self, check: Check) -> RowSupport {
        if self.streams {
            return RowSupport::Mounted;
        }
        if matches!(
            check,
            Check::StreamingReconstruction
                | Check::StreamingIncremental
                | Check::StreamingUsageAgreement
        ) {
            return RowSupport::not_producible(
                "this reimplementation of the Messages API serves /v1/messages and nothing \
                 else: it has no streaming route, so no run against it can demonstrate one",
            );
        }
        RowSupport::Mounted
    }
}

// ---------------------------------------------------------------------------
// The runs
// ---------------------------------------------------------------------------

async fn run(factory: Factory, fixtures: Fixtures) -> ConformanceReport {
    run_all(&factory, &fixtures).await
}

/// Every row a full run reports, so a count is checked against the suite rather
/// than against a number that would rot.
fn every_row() -> Vec<Check> {
    Check::run_order()
}

#[tokio::test]
async fn the_anthropic_profile_passes_every_row_without_skipping_one() {
    let report = run(Factory::anthropic(), Fixtures::tool_use()).await;
    assert!(report.passed(), "{report}");
    assert_eq!(report.provider.as_str(), "anthropic");
    assert_eq!(report.model.as_str(), MODEL);
    let (passed, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(
        skipped, 0,
        "a fully capable profile proves every row, and a skip is not a pass:\n{report}"
    );
    assert_eq!(passed, every_row().len());
    let order: Vec<Check> = report.results.iter().map(|result| result.check).collect();
    assert_eq!(order, every_row());
}

#[tokio::test]
async fn a_measured_proxy_passes_the_same_rows_through_a_different_surface() {
    let report = run(Factory::proxy(), Fixtures::tool_use()).await;
    assert!(report.passed(), "{report}");
    assert_eq!(report.provider.as_str(), "claude-proxy");
    let (_, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(skipped, 0, "{report}");
}

#[tokio::test]
async fn a_modest_profile_passes_by_skipping_what_it_cannot_claim() {
    let report = run(Factory::modest(), Fixtures::text()).await;
    assert!(report.passed(), "honesty is not a failure:\n{report}");
    for check in [
        Check::ToolAndReadRequestIds,
        Check::StreamingReconstruction,
        Check::StreamingIncremental,
        Check::StreamingUsageAgreement,
        Check::NoSilentCapabilityDowngrade,
    ] {
        assert_eq!(
            report.result(check).expect("check ran").status,
            CheckStatus::Skipped,
            "{check} cannot be proven by a prompt_only, tool-less, streamless profile:\n{report}"
        );
    }
    // The five it skips are the five it does not claim; every other row —
    // including reading the document out of a text block — is proven.
    let (_, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(skipped, 5, "{report}");
    // And each streaming row says why, in words, so a published table cannot
    // show a blank where an unimplemented feature sits.
    let table = report.compatibility_table();
    assert_eq!(table.matches("no streaming route").count(), 3, "{table}");
}

// ---------------------------------------------------------------------------
// What actually went on the wire
// ---------------------------------------------------------------------------

/// Builds a provider against a fresh server with `scenario` mounted.
async fn staged(
    factory: &Factory,
    fixtures: &Fixtures,
    scenario: Scenario,
) -> (MockServer, AnthropicProvider) {
    let server = MockServer::start().await;
    fixtures.mount(&server, scenario).await;
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
async fn a_schema_enforcing_profile_puts_the_schema_in_a_forced_tool() {
    let (server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::ValidStructured,
    )
    .await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;

    // The transport is one tool whose input schema *is* the required schema…
    assert_eq!(body["tools"].as_array().expect("tools").len(), 1);
    assert_eq!(body["tools"][0]["name"], payloads::SCHEMA_NAME);
    assert_eq!(
        body["tools"][0]["input_schema"],
        payloads::plan_schema(),
        "the schema itself must travel, not just its name"
    );
    assert!(
        body["tools"][0]["input_schema"]
            .to_string()
            .contains(payloads::SCHEMA_MARKER)
    );
    // …with the choice pinned to it, and parallel calls off so the answer is
    // one document rather than several.
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], payloads::SCHEMA_NAME);
    assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], true);
    // And nothing pretends the Messages API has a response format.
    assert!(body.get("response_format").is_none());
    assert!(!body.to_string().contains("response_format"));
    // `max_tokens` is required, so it is always there.
    assert!(body["max_tokens"].as_u64().is_some_and(|tokens| tokens > 0));
}

#[tokio::test]
async fn a_prompt_only_profile_does_not_quietly_upgrade_itself() {
    let (server, provider) = staged(
        &Factory::modest(),
        &Fixtures::text(),
        Scenario::ValidStructured,
    )
    .await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;

    // No tool is forced, and no tool is declared at all.
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
    // The schema is described in the system prompt instead, where nothing
    // enforces it — which is exactly what `prompt_only` claims.
    let system = body["system"].as_str().expect("a system prompt");
    assert!(system.contains("JSON Schema"), "{system}");
    assert!(system.contains(payloads::SCHEMA_MARKER), "{system}");
}

#[tokio::test]
async fn the_headers_are_the_ones_the_messages_api_requires() {
    let (server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::ValidStructured,
    )
    .await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let requests = server.received_requests().await.expect("recording enabled");
    let headers = &requests[0].headers;

    // Anthropic authenticates with `x-api-key`, never with a bearer token…
    assert!(headers.contains_key(API_KEY_HEADER));
    assert!(!headers.contains_key("authorization"));
    // …every request is versioned…
    assert_eq!(
        headers.get(VERSION_HEADER).and_then(|v| v.to_str().ok()),
        Some(DEFAULT_ANTHROPIC_VERSION)
    );
    // …no beta is opted into by default…
    assert!(!headers.contains_key(BETA_HEADER));
    // …and the stable request id travels as an idempotency hint (spec §20.7).
    assert!(headers.contains_key("idempotency-key"));
    assert!(requests[0].url.path().ends_with("/v1/messages"));

    // A proxy that wants a bearer token gets one, and only one.
    let (server, provider) = staged(
        &Factory::proxy(),
        &Fixtures::tool_use(),
        Scenario::ValidStructured,
    )
    .await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let requests = server.received_requests().await.expect("recording enabled");
    assert!(requests[0].headers.contains_key("authorization"));
    assert!(!requests[0].headers.contains_key(API_KEY_HEADER));
}

#[tokio::test]
async fn the_system_prompt_and_the_tool_result_land_where_the_api_wants_them() {
    let (server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::ValidStructured,
    )
    .await;
    let request = ModelRequest::new(ModelPurpose::Investigate)
        .with_system("Answer in Italian.")
        .with_message(Message::system("Never invent identifiers."))
        .with_message(Message::user("stato?"))
        .with_message(Message::new(
            Role::Assistant,
            vec![ContentPart::ToolCall(ToolCall::new(
                "toolu_read_1",
                payloads::TOOL_NAME,
                json!({"target": "tok_1"}),
            ))],
        ))
        .with_message(Message::tool_result(ToolResult::error(
            "toolu_read_1",
            "the case is locked",
        )))
        .with_output(OutputSpec::ToolCalls)
        .with_tools(vec![payloads::read_tool()]);
    provider.generate(request).await.expect("a valid fixture");
    let body = first_body(&server).await;

    // The system prompt is a top-level field, and the system *message* was
    // hoisted into it rather than sent as a turn the API has no role for.
    assert_eq!(
        body["system"],
        json!("Answer in Italian.\n\nNever invent identifiers.")
    );
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][0]["type"], "tool_use");
    assert_eq!(messages[1]["content"][0]["id"], "toolu_read_1");
    // A tool result belongs to a *user* message, first, with a real error flag.
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_read_1");
    assert_eq!(messages[2]["content"][0]["is_error"], true);
    assert_eq!(messages[2]["content"][0]["content"], "the case is locked");
}

#[tokio::test]
async fn the_streamed_answer_equals_the_whole_one_field_for_field() {
    let (_server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::StreamingReconstruction,
    )
    .await;
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
    // The usage halves the stream reports separately add up to what the whole
    // call reported: the fresh prompt tokens plus the ones read from the cache.
    assert_eq!(rebuilt.usage, whole.usage);
    assert_eq!(rebuilt.usage.input, payloads::USAGE_INPUT_TOKENS);
    assert_eq!(rebuilt.usage.cached_input, payloads::USAGE_CACHED_TOKENS);
    assert_eq!(rebuilt.usage.output, payloads::USAGE_OUTPUT_TOKENS);
    // The message id arrives on the first frame rather than being seeded.
    assert_eq!(rebuilt.raw_id, whole.raw_id);
    assert_eq!(rebuilt.raw_id.as_deref(), Some("msg_01Turnframe"));
    // The reassembled answer says it was reassembled; that is the one honest
    // difference between the paths.
    assert!(rebuilt.warnings.contains(&ResponseWarning::Reconstructed));
    assert!(!whole.warnings.contains(&ResponseWarning::Reconstructed));
}

#[tokio::test]
async fn the_streamed_path_reports_the_feature_it_dropped_just_as_the_whole_one_does() {
    // The Messages API takes eight stop sequences. A ninth is dropped, and
    // until the stream had a warning event the whole call said so and the
    // streamed call said nothing about the same request.
    let (_server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::StreamingReconstruction,
    )
    .await;
    let request = payloads::narration_request().with_stop(
        ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
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
        StreamAccumulator::new(request.request_id, "anthropic", MODEL),
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
async fn a_streamed_tool_call_reassembles_into_the_non_streamed_answer() {
    let arguments = "{\"target\": \"tok_1\"}";
    let server = MockServer::start().await;
    let mut body = String::new();
    body.push_str(&sse(&json!({
        "type": "message_start",
        "message": {"id": "msg_01Turnframe", "model": MODEL, "content": [],
                    "usage": {"input_tokens": FRESH_PROMPT_TOKENS, "output_tokens": 1,
                              "cache_creation_input_tokens": 0,
                              "cache_read_input_tokens": payloads::USAGE_CACHED_TOKENS}}
    })));
    body.push_str(&sse(&json!({
        "type": "content_block_start", "index": 0,
        "content_block": {"type": "tool_use", "id": payloads::EXPECTED_CALL_ID,
                          "name": payloads::TOOL_NAME, "input": {}}
    })));
    for fragment in [&arguments[..8], &arguments[8..]] {
        body.push_str(&sse(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": fragment}
        })));
    }
    body.push_str(&sse(&json!({"type": "content_block_stop", "index": 0})));
    body.push_str(&sse(&json!({
        "type": "message_delta", "delta": {"stop_reason": "tool_use"},
        "usage": {"output_tokens": payloads::USAGE_OUTPUT_TOKENS}
    })));
    body.push_str(&sse(&json!({"type": "message_stop"})));

    Mock::given(method("POST"))
        .and(path_regex(MESSAGES_PATH))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(MESSAGES_PATH))
        .respond_with(template(&Fixtures::tool_use(), Scenario::ToolCallIds))
        .mount(&server)
        .await;

    let provider = Factory::anthropic()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::tool_request();
    let whole = provider.generate(request.clone()).await.expect("whole");
    let stream = provider.stream(request.clone()).await.expect("stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "anthropic", MODEL),
    )
    .await
    .expect("reassembles");

    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.finish, whole.finish);
    assert_eq!(rebuilt.usage, whole.usage);
    assert_eq!(
        rebuilt.tool_calls()[0].id.as_str(),
        payloads::EXPECTED_CALL_ID
    );
    assert_eq!(
        rebuilt.tool_calls()[0].arguments,
        json!({"target": "tok_1"})
    );
}

#[tokio::test]
async fn a_stream_that_dies_mid_answer_is_a_typed_failure_not_a_short_one() {
    let server = MockServer::start().await;
    let body = sse(&json!({
        "type": "content_block_start", "index": 0,
        "content_block": {"type": "text", "text": "meta "}
    }));
    Mock::given(method("POST"))
        .and(path_regex(MESSAGES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider = Factory::anthropic()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::narration_request();
    let stream = provider.stream(request.clone()).await.expect("stream");
    let error = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "anthropic", MODEL),
    )
    .await
    .expect_err("a truncated stream is not a short answer");
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("stream_ended_without_finish".to_owned())
    );
}

#[tokio::test]
async fn an_error_frame_mid_stream_becomes_a_typed_failure() {
    let server = MockServer::start().await;
    let mut body = sse(&json!({
        "type": "message_start",
        "message": {"id": "msg_1", "content": [], "usage": {"input_tokens": 5}}
    }));
    body.push_str(&sse(&json!({
        "type": "error",
        "error": {"type": "overloaded_error", "message": "Overloaded"}
    })));
    Mock::given(method("POST"))
        .and(path_regex(MESSAGES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider = Factory::anthropic()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::narration_request();
    let stream = provider.stream(request.clone()).await.expect("stream");
    let error = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "anthropic", MODEL),
    )
    .await
    .expect_err("an error frame is a failure");
    // Overloaded is a server error: retryable, and not a rate limit to wait out.
    assert!(
        matches!(
            error.kind(),
            ProviderErrorKind::Server { status: Some(529) }
        ),
        "{error}"
    );
    assert_eq!(error.retry_class(), RetryClass::Retry);
    assert!(error.retry_after().is_none());
}

#[tokio::test]
async fn a_failing_call_never_renders_the_configured_key() {
    let (_server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::Authentication,
    )
    .await;
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
            !rendering.contains("invalid x-api-key"),
            "the response body surfaced: {rendering}"
        );
    }
    // What survives is the kind, the keys and a sanitized machine code.
    assert_eq!(
        error.to_string(),
        format!(
            "provider call failed: authentication [anthropic/{MODEL}] code=authentication_error"
        )
    );
    assert_eq!(error.retry_class(), RetryClass::Fallback);
}

#[tokio::test]
async fn an_expired_key_and_a_spent_balance_are_told_apart_on_the_wire() {
    // A 401 whose message says the key used to work.
    let (_server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::ExpiredCredential,
    )
    .await;
    let expired = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("an expired key is a failure");
    assert!(
        matches!(expired.kind(), ProviderErrorKind::CredentialExpired),
        "an expired credential is not a rejected one: {expired}"
    );
    assert_eq!(expired.retry_class(), RetryClass::Fallback);
    assert!(
        !expired.to_string().contains("generate a new key"),
        "{expired}"
    );

    // A 429 — the status of a rate limit — that is really an empty balance.
    let (_server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::QuotaExhausted,
    )
    .await;
    let spent = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a spent balance is a failure");
    assert!(
        matches!(spent.kind(), ProviderErrorKind::QuotaExhausted { .. }),
        "a spent balance is not a rate limit to wait out: {spent}"
    );
    assert_eq!(spent.retry_class(), RetryClass::Fallback);
    // The `Retry-After` the endpoint advertised is deliberately not carried:
    // waiting three seconds does not refill a balance.
    assert!(spent.retry_after().is_none());

    // The same status with a real per-minute limit still is one.
    let (_server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::RateLimited,
    )
    .await;
    let limited = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a 429 is a failure");
    assert!(
        matches!(limited.kind(), ProviderErrorKind::RateLimited { .. }),
        "{limited}"
    );
    assert_eq!(
        limited.retry_after(),
        Some(Duration::from_secs(payloads::RETRY_AFTER_SECONDS))
    );
    assert_eq!(limited.retry_class(), RetryClass::RetryAfter);
}

#[tokio::test]
async fn a_context_overflow_carries_the_numbers_the_message_named() {
    let (_server, provider) = staged(
        &Factory::anthropic(),
        &Fixtures::tool_use(),
        Scenario::ContextOverflow,
    )
    .await;
    let error = provider
        .generate(payloads::structured_request())
        .await
        .expect_err("an overflow is a failure");
    assert_eq!(
        error.kind(),
        &ProviderErrorKind::ContextOverflow {
            needed_tokens: Some(215_048),
            limit_tokens: Some(199_999)
        }
    );
    // Fatal: the runtime shrinks the prompt rather than shopping for a window.
    assert_eq!(error.retry_class(), RetryClass::Fatal);
}

#[tokio::test]
async fn the_default_base_url_is_the_published_one() {
    let provider = AnthropicProvider::anthropic()
        .api_key(ApiKey::new(payloads::DUMMY_API_KEY))
        .model(MODEL)
        .build()
        .expect("builds");
    assert_eq!(
        provider.endpoint(),
        format!("{ANTHROPIC_BASE_URL}/v1/messages")
    );
}
