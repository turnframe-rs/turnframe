//! The conformance macros, used the way an adapter crate uses them
//! (spec §20.8, §27.5).
//!
//! A macro that declares a test is only worth anything if the test it declares
//! can fail, so this file drives it in four directions against a toy adapter:
//! a correct adapter passes every row the suite runs, an adapter that flattens
//! its error mapping is caught, a deployment that genuinely cannot produce a row
//! is reported as *unproven* with its reason, and one that stays quiet about a
//! row it cannot produce fails it.
//!
//! The adapter is deliberately real: it speaks HTTP to a wiremock server, maps
//! statuses and bodies onto the normalized error family, reports the counts the
//! vendor sent on both the whole and the streamed path, and honours the request
//! deadline. A double that answered from memory would exercise the macros but
//! not the harness underneath them.
//!
//! The row count is never written down here. It is asserted against
//! [`Check::run_order`], so a suite that grows a row breaks this file with a
//! count mismatch instead of leaving the kit quietly proving less than it did.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelKey, ProviderKey, RequestId};
use turnframe_provider::prelude::{ApiKey, ModelRequest, ModelResponse, ToolCall};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::response::{FinishReason, TokenUsage};
use turnframe_provider::stream::{ModelStream, StreamEvent};
use turnframe_test::providers::conformance::{
    Check, CheckStatus, ProviderFactory, RowSupport, Scenario, StatusRow, StatusSupport,
    WireFixtures, accept, payloads,
};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The single endpoint the toy vendor exposes.
const ENDPOINT: &str = "/v1/generate";

// ---------------------------------------------------------------------------
// A minimal HTTP adapter for a made-up vendor.
// ---------------------------------------------------------------------------

struct ToyAdapter {
    base_url: String,
    api_key: ApiKey,
    client: reqwest::Client,
    capabilities: ProviderCapabilities,
    /// The one defect this file needs: map every failure onto `Transport`,
    /// losing the classification the retry policy reads.
    flatten_errors: bool,
}

impl ToyAdapter {
    fn wire_body(&self, request: &ModelRequest, stream: bool) -> Value {
        json!({
            "request_id": request.request_id.to_string(),
            "stream": stream,
            // The schema really travels, which is what the profile's
            // `NativeJsonSchema` declaration claims.
            "schema": request.output.schema().cloned().unwrap_or(Value::Null),
            "tools": request.tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>(),
            "messages": request
                .messages
                .iter()
                .map(|message| json!({"role": message.role.as_str(), "text": message.text()}))
                .collect::<Vec<_>>(),
        })
    }

    fn map_error(&self, status: u16, retry_after: Option<u64>, body: &str) -> ProviderError {
        if self.flatten_errors {
            return ProviderError::transport("http_error");
        }
        // The two signals no status code carries. This vendor, like most real
        // ones, reuses 401 for a lapsed token and 429 for a spent balance, so
        // the body is read before the status line: an adapter that branched on
        // the status alone would tell a caller holding a refresher that its key
        // is bad, and would sleep on a Retry-After that will never help.
        if body.contains("token_expired") {
            return ProviderError::credential_expired();
        }
        if body.contains("insufficient_quota") {
            return ProviderError::quota_exhausted(Some("credit_balance"));
        }
        match status {
            401 => ProviderError::authentication(),
            403 => ProviderError::authorization(),
            404 => ProviderError::model_not_found(),
            408 => ProviderError::timeout(),
            429 => ProviderError::rate_limited(retry_after.map(Duration::from_secs)),
            400 if body.contains("context_length_exceeded") => {
                ProviderError::context_overflow(None, None)
            }
            400 => ProviderError::invalid_request("bad_request"),
            500..=599 => ProviderError::server(Some(status)),
            other => ProviderError::other(format!("http_{other}")),
        }
    }

    /// Reads the vendor's counts.
    ///
    /// `input` is the whole prompt and `cached` the slice of it the vendor
    /// served from its own cache, already inside `input` — which is the
    /// contract the usage row holds an adapter to.
    fn map_usage(&self, reported: Option<&Value>) -> TokenUsage {
        let count = |field: &str| {
            reported
                .and_then(|usage| usage.get(field))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        TokenUsage::new(count("input"), count("output")).with_cached_input(count("cached"))
    }

    fn map_response(&self, request_id: RequestId, body: &Value) -> ModelResponse {
        let finish = match body.get("finish").and_then(Value::as_str) {
            Some("tool_calls") => FinishReason::ToolCalls,
            Some("refusal") => FinishReason::Refusal,
            Some("content_filter") => FinishReason::ContentFilter,
            Some("length") => FinishReason::MaxTokens,
            _ => FinishReason::Stop,
        };
        let mut response = ModelResponse::new(request_id, self.provider_key(), self.model_key())
            .with_finish(finish)
            .with_usage(self.map_usage(body.get("usage")));
        if let Some(text) = body.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            response = response.with_text(text);
        }
        for call in body
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let arguments = call
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
            response = response.with_tool_call(ToolCall::new(
                CallId::new(call.get("id").and_then(Value::as_str).unwrap_or_default()),
                call.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                arguments,
            ));
        }
        response
    }

    /// One HTTP call under the request's own deadline.
    async fn call(&self, request: &ModelRequest, stream: bool) -> Result<Value, ProviderError> {
        let sent = self
            .client
            .post(format!("{}{ENDPOINT}", self.base_url))
            .bearer_auth(self.api_key.expose())
            .json(&self.wire_body(request, stream))
            .send();
        let response = match tokio::time::timeout(request.timeout, sent).await {
            Err(_elapsed) => return Err(ProviderError::timeout()),
            Ok(Err(_transport)) => return Err(ProviderError::transport("send_failed")),
            Ok(Ok(response)) => response,
        };
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let text = match tokio::time::timeout(request.timeout, response.text()).await {
            Err(_elapsed) => return Err(ProviderError::timeout()),
            Ok(Err(_transport)) => return Err(ProviderError::transport("read_failed")),
            Ok(Ok(text)) => text,
        };
        if status >= 400 {
            return Err(self
                .map_error(status, retry_after, &text)
                .with_model(&self.reference()));
        }
        if text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&text)
            .map_err(|_| ProviderError::malformed("body_not_json").with_model(&self.reference()))
    }
}

impl fmt::Debug for ToyAdapter {
    /// Renders the key through [`ApiKey`], which masks it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToyAdapter")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModelProvider for ToyAdapter {
    fn provider_key(&self) -> ProviderKey {
        ProviderKey::from("toy")
    }

    fn model_key(&self) -> ModelKey {
        ModelKey::from("toy-1")
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        let body = self.call(&request, false).await?;
        Ok(self.map_response(request.request_id, &body))
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        if !self.capabilities.streaming {
            return Err(ProviderError::unsupported("streaming").with_model(&self.reference()));
        }
        let body = self.call(&request, true).await?;
        let mut events = Vec::new();
        for event in body
            .get("events")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            match event.get("type").and_then(Value::as_str) {
                Some("text") => events.push(StreamEvent::text(
                    event
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )),
                // The counts ride on their own frame, the way most vendors send
                // them. Dropping this arm is invisible to reassembly and makes
                // the cost of a turn depend on which path served it, which is
                // the whole reason the suite compares the two.
                Some("usage") => events.push(StreamEvent::Usage {
                    usage: self.map_usage(Some(event)),
                }),
                Some("finish") => events.push(StreamEvent::Finish {
                    reason: FinishReason::Stop,
                }),
                _ => return Err(ProviderError::malformed("unknown_stream_event")),
            }
        }
        Ok(ModelStream::from_events(events))
    }
}

// ---------------------------------------------------------------------------
// Factory and fixtures: the two pieces an adapter crate supplies.
// ---------------------------------------------------------------------------

struct ToyFactory {
    streaming: bool,
    flatten_errors: bool,
}

impl ToyFactory {
    /// An adapter that does everything the suite exercises, correctly.
    fn correct() -> Self {
        Self {
            streaming: true,
            flatten_errors: false,
        }
    }

    /// An adapter that declares no streaming, so one row cannot be exercised.
    fn without_streaming() -> Self {
        Self {
            streaming: false,
            flatten_errors: false,
        }
    }

    /// An adapter that maps every vendor failure onto one transport error.
    fn with_flattened_errors() -> Self {
        Self {
            streaming: true,
            flatten_errors: true,
        }
    }
}

impl ProviderFactory for ToyFactory {
    type Provider = ToyAdapter;

    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        Ok(ToyAdapter {
            base_url: base_url.to_owned(),
            api_key,
            client: reqwest::Client::new(),
            capabilities: ProviderCapabilities::minimal()
                .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
                .with_tool_calling(ToolCallingCapability::Parallel)
                .with_streaming(self.streaming)
                // The vendor reports a cache figure, so the profile says so and
                // the usage row holds the adapter to reading it rather than to
                // reporting a zero.
                .with_prompt_caching(true)
                .with_preserves_call_ids(true)
                .with_max_context_tokens(128_000),
            flatten_errors: self.flatten_errors,
        })
    }
}

struct ToyFixtures;

fn ok_body(content: &str, finish: &str) -> Value {
    json!({
        "id": "toy_resp_1",
        "content": content,
        "finish": finish,
        "usage": {"input": 42, "output": 7}
    })
}

#[async_trait]
impl WireFixtures for ToyFixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        let template = match scenario {
            Scenario::ValidStructured => ResponseTemplate::new(200)
                .set_body_json(ok_body(&payloads::valid_plan().to_string(), "stop")),
            Scenario::MalformedJson => {
                ResponseTemplate::new(200).set_body_json(ok_body(payloads::MALFORMED_JSON, "stop"))
            }
            Scenario::UnknownField => ResponseTemplate::new(200).set_body_json(ok_body(
                &payloads::plan_with_unknown_field().to_string(),
                "stop",
            )),
            Scenario::MissingField => ResponseTemplate::new(200).set_body_json(ok_body(
                &payloads::plan_with_missing_field().to_string(),
                "stop",
            )),
            Scenario::MultipleActs => ResponseTemplate::new(200)
                .set_body_json(ok_body(&payloads::two_act_plan().to_string(), "stop")),
            Scenario::ToolCallIds => ResponseTemplate::new(200).set_body_json(json!({
                "id": "toy_resp_1",
                "content": "",
                "finish": "tool_calls",
                "tool_calls": [{
                    "id": payloads::EXPECTED_CALL_ID,
                    "name": payloads::TOOL_NAME,
                    "arguments": "{\"target\": \"tok_1\"}"
                }]
            })),
            Scenario::StreamingReconstruction => {
                // Two mocks on one endpoint: the streaming one matches first on
                // the flag the adapter puts in the body.
                //
                // The prose arrives in two events and the counts on a third.
                // Both halves of that matter: one event would leave an
                // adapter that buffers the body indistinguishable from one
                // that streams it, and no usage frame would leave the
                // streamed path with nothing to agree with the whole path
                // about.
                Mock::given(method("POST"))
                    .and(path(ENDPOINT))
                    .and(body_string_contains("\"stream\":true"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "id": "toy_resp_1",
                        "events": [
                            {"type": "text", "text": "Ho preparato "},
                            {"type": "text", "text": "la modifica."},
                            {"type": "usage", "input": 42, "output": 7},
                            {"type": "finish"}
                        ]
                    })))
                    .mount(server)
                    .await;
                ResponseTemplate::new(200)
                    .set_body_json(ok_body("Ho preparato la modifica.", "stop"))
            }
            // A cache hit: the whole prompt, and the part of it the vendor
            // did not have to recompute. The answer is ordinary prose —
            // only the counts are under test.
            Scenario::CachedUsage => ResponseTemplate::new(200).set_body_json(json!({
                "id": "toy_resp_1",
                "content": "Ho preparato la modifica.",
                "finish": "stop",
                "usage": {
                    "input": payloads::USAGE_INPUT_TOKENS,
                    "output": payloads::USAGE_OUTPUT_TOKENS,
                    "cached": payloads::USAGE_CACHED_TOKENS
                }
            })),
            Scenario::EmptyOutput => ResponseTemplate::new(200).set_body_string(""),
            Scenario::Refusal => {
                ResponseTemplate::new(200).set_body_json(ok_body(payloads::REFUSAL_TEXT, "refusal"))
            }
            Scenario::SlowResponse => ResponseTemplate::new(200)
                .set_body_json(ok_body("too late", "stop"))
                .set_delay(payloads::SLOW_RESPONSE_DELAY),
            Scenario::RateLimited => ResponseTemplate::new(429)
                .insert_header(
                    "retry-after",
                    payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
                )
                .set_body_json(json!({"error": {"code": "rate_limit_exceeded"}})),
            Scenario::Authentication => ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"code": "invalid_api_key"}})),
            Scenario::ContextOverflow => ResponseTemplate::new(400)
                .set_body_json(json!({"error": {"code": "context_length_exceeded"}})),
            // The scenarios behind the per-status rows: one wire failure
            // each, so the mapping is proved failure by failure rather than
            // inferred from the four the older rows happened to cover.
            Scenario::Authorization => ResponseTemplate::new(403)
                .set_body_json(json!({"error": {"code": "model_not_entitled"}})),
            Scenario::ModelNotFound => ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "unknown_model"}})),
            Scenario::RequestTimeout => ResponseTemplate::new(408)
                .set_body_json(json!({"error": {"code": "request_timeout"}})),
            Scenario::InvalidRequest => ResponseTemplate::new(400)
                .set_body_json(json!({"error": {"code": "invalid_argument"}})),
            Scenario::ServerError => {
                ResponseTemplate::new(500).set_body_json(json!({"error": {"code": "internal"}}))
            }
            Scenario::ServiceUnavailable => {
                ResponseTemplate::new(503).set_body_json(json!({"error": {"code": "overloaded"}}))
            }
            // This vendor answers 200 and says the filter stopped it, which the
            // row accepts as a finish reason rather than as a failure.
            Scenario::ContentFilter => {
                ResponseTemplate::new(200).set_body_json(ok_body("", "content_filter"))
            }
            // A 401 that a refresh would fix, and a 429 that no waiting
            // will. Both wear the status of the row next to them, so only
            // the body tells them apart — which is exactly what these two
            // rows exist to prove the adapter reads.
            Scenario::ExpiredCredential => ResponseTemplate::new(401).set_body_json(
                json!({"error": {"code": "token_expired", "message": "the bearer token lapsed"}}),
            ),
            Scenario::QuotaExhausted => ResponseTemplate::new(429)
                .insert_header("retry-after", "60")
                .set_body_json(json!({
                    "error": {"code": "insufficient_quota", "message": "credit balance is zero"}
                })),
            Scenario::SecretInBody => ResponseTemplate::new(200).set_body_json(json!({
                "id": "toy_resp_1",
                "content": payloads::valid_plan().to_string(),
                "finish": "stop",
                // A careless vendor echoes the credential back; the adapter
                // must not carry it into anything it renders.
                "debug": {"authorization": format!("Bearer {}", payloads::DUMMY_API_KEY)}
            })),
            _ => ResponseTemplate::new(200).set_body_json(ok_body("", "stop")),
        };
        Mock::given(method("POST"))
            .and(path(ENDPOINT))
            .respond_with(template)
            .mount(server)
            .await;
    }
}

/// The same fixtures, for a vendor whose endpoint never answers 408.
///
/// A row a real endpoint cannot produce is declared here, with a reason: the
/// suite then reports it as *unproven* rather than failing it, and the reason
/// travels into the report so a compatibility table can say why.
struct ToyFixturesWithoutTimeoutStatus;

#[async_trait]
impl WireFixtures for ToyFixturesWithoutTimeoutStatus {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        ToyFixtures.mount(server, scenario).await;
    }

    fn status_support(&self, row: StatusRow) -> StatusSupport {
        match row {
            StatusRow::RequestTimeout => {
                StatusSupport::not_producible("this endpoint answers 504, never 408")
            }
            _ => StatusSupport::Mounted,
        }
    }
}

/// The same fixtures, for a deployment with no streaming endpoint.
///
/// Declaring `streaming: false` on the profile is not enough and must not be:
/// the three streaming rows would then vanish from the table, and a blank there
/// reads exactly like a feature nobody implemented. The deployment says so in
/// words instead, through the feature hook, and the rows come back unproven
/// with the reason attached.
struct ToyFixturesWithoutStreaming;

/// Why this deployment cannot exercise a streaming row.
const NO_STREAMING_REASON: &str = "this deployment runs the toy vendor with its streaming route switched off, \
     so no call to it ever produces a stream";

#[async_trait]
impl WireFixtures for ToyFixturesWithoutStreaming {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        ToyFixtures.mount(server, scenario).await;
    }

    fn feature_support(&self, check: Check) -> RowSupport {
        match check {
            Check::StreamingReconstruction
            | Check::StreamingIncremental
            | Check::StreamingUsageAgreement => RowSupport::not_producible(NO_STREAMING_REASON),
            _ => RowSupport::Mounted,
        }
    }
}

// ---------------------------------------------------------------------------
// The macros under test.
// ---------------------------------------------------------------------------

// Two lines of declaration, and the adapter has a conformance test.
turnframe_test::provider_conformance_suite! {
    name: the_toy_adapter_conforms,
    factory: ToyFactory::correct(),
    fixtures: ToyFixtures,
}

// A profile that cannot stream says so through the fixtures' feature hook, and
// the gate is told which rows that leaves unproven.
turnframe_test::provider_conformance_suite! {
    name: a_profile_without_streaming_conforms_with_those_rows_declared,
    factory: ToyFactory::without_streaming(),
    fixtures: ToyFixturesWithoutStreaming,
    allow_skipped: [
        StreamingReconstruction,
        StreamingIncremental,
        StreamingUsageAgreement,
    ],
}

// A per-status row an endpoint cannot produce is named the way the variant
// reads, and nowhere else.
turnframe_test::provider_conformance_suite! {
    name: an_endpoint_that_cannot_answer_408_conforms_with_that_row_declared,
    factory: ToyFactory::correct(),
    fixtures: ToyFixturesWithoutTimeoutStatus,
    allow_skipped: [StatusMapping(RequestTimeout)],
}

// The same two declarations without a fixtures type and without an
// `allow_skipped` list that has to agree with it: the row and its reason are
// written once, and the macro tells both the harness and the gate.
turnframe_test::provider_conformance_suite! {
    name: a_deployment_declares_every_missing_row_in_one_place,
    factory: ToyFactory::without_streaming(),
    fixtures: ToyFixtures,
    not_producible: [
        StreamingReconstruction => NO_STREAMING_REASON,
        StreamingIncremental => NO_STREAMING_REASON,
        StreamingUsageAgreement => NO_STREAMING_REASON,
        StatusMapping(RequestTimeout) => "this endpoint answers 504, never 408",
    ],
}

#[test]
fn an_adapter_that_flattens_its_errors_is_caught() {
    let report = turnframe_test::provider_conformance_report!(
        factory: ToyFactory::with_flattened_errors(),
        fixtures: ToyFixtures,
    );
    assert!(
        !report.passed(),
        "an adapter that loses its error classification must not pass:\n{report}"
    );
    let gaps = accept(&report, &[]).unwrap_err();
    // Every row that distinguishes one vendor failure from another is lost.
    for check in [
        Check::RateLimit,
        Check::AuthenticationFailure,
        Check::ContextOverflow,
    ] {
        assert!(
            gaps.iter()
                .any(|gap| gap.to_string().contains(check.as_str())),
            "{check} should have caught the flattened mapping: {gaps:?}"
        );
    }
    // This is what the generated test would have panicked with.
    assert!(
        turnframe_test::providers::conformance::describe(&report, &gaps)
            .contains("check rate_limit failed")
    );
}

#[test]
fn an_undeclared_skip_is_reported_as_unproven() {
    let report = turnframe_test::provider_conformance_report!(
        factory: ToyFactory::without_streaming(),
        fixtures: ToyFixturesWithoutStreaming,
    );
    // The suite itself tolerates a declared skip...
    assert!(report.passed(), "{report}");
    // ...and the macro's default does not, because an unexercised row is not a
    // proven one.
    let gaps = accept(&report, &[]).unwrap_err();
    assert_eq!(gaps.len(), STREAMING_ROWS.len(), "{gaps:?}");
    for check in STREAMING_ROWS {
        assert!(
            gaps.iter()
                .any(|gap| gap.to_string().contains(check.as_str())),
            "{check} is unproven and should be reported as such: {gaps:?}"
        );
    }
    // The reason the deployment gave travels into the gap, so a reviewer reads
    // why the row is unproven rather than only that it is.
    assert!(gaps[0].to_string().contains("streaming route switched off"));
    assert_eq!(accept(&report, &STREAMING_ROWS), Ok(()));
    // The table a deployment like this would publish: three rows unproven, each
    // carrying the words that say why.
    println!("{}", report.compatibility_table());
}

/// The rows a deployment with no streaming endpoint leaves unproven.
const STREAMING_ROWS: [Check; 3] = [
    Check::StreamingReconstruction,
    Check::StreamingIncremental,
    Check::StreamingUsageAgreement,
];

#[test]
fn a_profile_with_no_streaming_and_nothing_to_say_fails_those_rows() {
    // Silence is not a declaration. Without the feature hook the same profile
    // used to make three rows disappear for free, and a compatibility table
    // then showed a blank where an unimplemented feature sat.
    let report = turnframe_test::provider_conformance_report!(
        factory: ToyFactory::without_streaming(),
        fixtures: ToyFixtures,
    );
    assert!(!report.passed(), "{report}");
    for check in STREAMING_ROWS {
        let result = report.result(check).expect("the row ran");
        assert_eq!(result.status, CheckStatus::Failed, "{report}");
        let detail = result.detail.clone().unwrap_or_default();
        assert!(detail.contains("feature_support"), "{detail}");
    }
}

#[test]
fn the_kit_runs_every_row_the_suite_declares() {
    let report = turnframe_test::provider_conformance_report!(
        factory: ToyFactory::correct(),
        fixtures: ToyFixtures,
    );
    // Counted against the suite's own order rather than against a number
    // written here: when the suite grows a row, this fails with a count
    // mismatch instead of the kit quietly proving less than it used to.
    let expected = Check::run_order();
    let ran: Vec<Check> = report.results.iter().map(|result| result.check).collect();
    assert_eq!(
        ran, expected,
        "the toy adapter must be measured on every row, in run order:\n{report}"
    );
    let (passed, failed, unproven) = report.counts();
    assert_eq!(
        (passed, failed, unproven),
        (expected.len(), 0, 0),
        "a correct adapter proves every row:\n{report}"
    );
    // The report a compatibility table would be copied from, so a run that
    // starts leaving rows unproven says so in the test output.
    println!("{}", report.compatibility_table());
}
