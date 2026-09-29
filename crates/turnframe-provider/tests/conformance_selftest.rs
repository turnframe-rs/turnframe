//! Self-test of the provider conformance harness.
//!
//! The harness in [`turnframe_provider::conformance`] is the thing every
//! adapter is judged by, so a defect in it would pass every adapter silently.
//! This file makes that impossible by running the suite against a tiny
//! in-crate adapter — a real HTTP client talking to a real wiremock server —
//! in two directions:
//!
//! * a **correct** adapter must produce a report with no failures;
//! * a set of **deliberately broken** adapters must each be caught on exactly
//!   the row that describes their defect.
//!
//! The second half is the one that matters. A harness that reports "all
//! passed" for an adapter that leaks its API key, never sends the schema it
//! claims to enforce, or executes the parseable subset of a malformed plan is
//! worse than no harness at all.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::conformance::{
    Check, CheckStatus, ConformanceReport, ProviderFactory, RowSupport, Scenario, StatusRow,
    StatusSupport, WireFixtures, payloads, run_all,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelKey, ProviderKey, RequestId};
use turnframe_provider::prelude::*;
use turnframe_provider::request::{ContentPart, OutputSpec};
use turnframe_provider::stream::ModelStream;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The single endpoint the toy vendor exposes.
const ENDPOINT: &str = "/v1/generate";

// ---------------------------------------------------------------------------
// A toy adapter, correct by default and breakable on demand.
// ---------------------------------------------------------------------------

/// The ways this adapter can be made wrong, one per harness row under test.
#[derive(Debug, Clone, Copy, Default)]
struct Defects {
    /// Declare `NativeJsonSchema` but never put the schema on the wire.
    omit_schema: bool,
    /// Render the configured credential in `Debug`.
    leak_key_in_debug: bool,
    /// Ignore what the server said and always report the valid plan.
    invent_a_valid_plan: bool,
    /// Declare `preserves_call_ids` but renumber them.
    rename_call_ids: bool,
    /// Map every failure onto `Transport`, losing the classification.
    flatten_errors: bool,
    /// Treat a 403 as a rejected credential, losing the entitlement/credential
    /// distinction an operator needs.
    confuse_403_with_401: bool,
    /// Report a context-length 400 as a generic bad request, so the runtime
    /// re-sends a prompt that can never fit.
    context_length_as_bad_request: bool,
    /// Read the status and not the body, so an expired token looks like a bad
    /// key and a spent quota looks like a rate limit.
    trust_the_status_line: bool,
    /// Collect every text fragment and emit one delta at the end. Reassembles
    /// perfectly; shows a reader nothing.
    buffer_the_stream: bool,
    /// Read the counts on the whole path and drop them on the streamed one, so
    /// the cost of a turn depends on which path served it.
    drop_stream_usage: bool,
    /// Report the prompt *minus* the cached part as the input, so the cache is
    /// larger than the prompt it is supposed to be inside.
    net_cached_usage: bool,
    /// Read every 401 as an expiry. Not a defect on the feature row — a
    /// rejected credential really is a credential problem — but a defect on the
    /// per-status row that exists to tell the two apart.
    every_401_is_an_expiry: bool,
}

/// A minimal HTTP adapter for a made-up vendor.
struct ToyAdapter {
    base_url: String,
    api_key: ApiKey,
    client: reqwest::Client,
    capabilities: ProviderCapabilities,
    defects: Defects,
}

impl ToyAdapter {
    fn new(base_url: &str, api_key: ApiKey, defects: Defects) -> Self {
        Self {
            base_url: base_url.to_owned(),
            api_key,
            client: reqwest::Client::new(),
            capabilities: toy_capabilities(),
            defects,
        }
    }

    /// Builds the vendor request body from a normalized request.
    fn wire_body(&self, request: &ModelRequest, stream: bool) -> Value {
        let schema = if self.defects.omit_schema {
            Value::Null
        } else {
            request.output.schema().cloned().unwrap_or(Value::Null)
        };
        json!({
            "request_id": request.request_id.to_string(),
            "stream": stream,
            "schema": schema,
            "tools": request.tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>(),
            "messages": request
                .messages
                .iter()
                .map(|message| json!({"role": message.role.as_str(), "text": message.text()}))
                .collect::<Vec<_>>(),
        })
    }

    /// Maps a vendor status and body onto the normalized error family.
    fn map_error(&self, status: u16, retry_after: Option<u64>, body: &str) -> ProviderError {
        if self.defects.flatten_errors {
            return ProviderError::transport("http_error");
        }
        // The two signals a status code cannot carry. They are read from the
        // body first, because this vendor — like most real ones — reuses 401
        // for an expired token and 429 for a spent quota.
        if !self.defects.trust_the_status_line {
            if body.contains("token_expired") {
                return ProviderError::credential_expired();
            }
            if body.contains("insufficient_quota") {
                return ProviderError::quota_exhausted(Some("credit_balance"));
            }
        }
        match status {
            401 if self.defects.every_401_is_an_expiry => ProviderError::credential_expired(),
            401 => ProviderError::authentication(),
            403 if self.defects.confuse_403_with_401 => ProviderError::authentication(),
            403 => ProviderError::authorization(),
            408 => ProviderError::timeout(),
            429 => ProviderError::rate_limited(retry_after.map(Duration::from_secs)),
            400 if body.contains("context_length_exceeded") => {
                if self.defects.context_length_as_bad_request {
                    ProviderError::invalid_request("bad_request")
                } else {
                    ProviderError::context_overflow(None, None)
                }
            }
            400 => ProviderError::invalid_request("bad_request"),
            404 => ProviderError::model_not_found(),
            500..=599 => ProviderError::server(Some(status)),
            other => ProviderError::other(format!("http_{other}")),
        }
    }

    /// Maps a vendor answer onto the normalized response.
    fn map_response(
        &self,
        request_id: RequestId,
        body: &Value,
    ) -> Result<ModelResponse, ProviderError> {
        let finish = match body.get("finish").and_then(Value::as_str) {
            Some("tool_calls") => FinishReason::ToolCalls,
            Some("refusal") => FinishReason::Refusal,
            Some("content_filter") => FinishReason::ContentFilter,
            Some("length") => FinishReason::MaxTokens,
            _ => FinishReason::Stop,
        };
        let mut response = ModelResponse::new(request_id, self.provider_key(), self.model_key())
            .with_finish(finish)
            .with_usage(self.usage(body.get("usage")));
        if let Some(id) = body.get("id").and_then(Value::as_str) {
            response = response.with_raw_id(id);
        }
        if self.defects.invent_a_valid_plan {
            // The defect: report what the caller wanted rather than what came
            // back. The harness must notice on every rejection row.
            return Ok(response.with_text(payloads::valid_plan().to_string()));
        }
        if let Some(text) = body.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            response = response.with_text(text);
        }
        for (index, call) in body
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let id = if self.defects.rename_call_ids {
                CallId::new(format!("renamed-{index}"))
            } else {
                CallId::new(call.get("id").and_then(Value::as_str).unwrap_or_default())
            };
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let arguments = match call.get("arguments").and_then(Value::as_str) {
                Some(raw) => serde_json::from_str(raw)
                    .map_err(|_| ProviderError::malformed("tool_arguments_not_json"))?,
                None => Value::Object(serde_json::Map::new()),
            };
            response = response.with_tool_call(ToolCall::new(id, name, arguments));
        }
        Ok(response)
    }

    /// Reads the vendor's counts.
    ///
    /// The contract the whole system reads usage by: `input` is the entire
    /// prompt and `cached` is the slice of it the vendor served from its cache.
    fn usage(&self, reported: Option<&Value>) -> TokenUsage {
        let count = |field: &str| {
            reported
                .and_then(|usage| usage.get(field))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let (input, cached) = (count("input"), count("cached"));
        if self.defects.net_cached_usage {
            // The defect: report what was actually computed rather than the
            // whole prompt, leaving a cache bigger than the input it is in.
            return TokenUsage::new(input.saturating_sub(cached), count("output"))
                .with_cached_input(cached);
        }
        TokenUsage::new(input, count("output")).with_cached_input(cached)
    }

    /// Performs one HTTP call under the request's own deadline.
    async fn call(&self, request: &ModelRequest, stream: bool) -> Result<Value, ProviderError> {
        let url = format!("{}{ENDPOINT}", self.base_url);
        let sent = self
            .client
            .post(url)
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("ToyAdapter");
        out.field("base_url", &self.base_url);
        if self.defects.leak_key_in_debug {
            // The defect: a plain string field holding the credential.
            out.field("api_key", &self.api_key.expose());
        } else {
            out.field("api_key", &self.api_key);
        }
        out.finish_non_exhaustive()
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
        if body.as_object().is_some_and(serde_json::Map::is_empty) {
            // An empty answer is a normalized empty response, not a plan.
            return Ok(ModelResponse::new(
                request.request_id,
                self.provider_key(),
                self.model_key(),
            ));
        }
        self.map_response(request.request_id, &body)
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        let body = self.call(&request, true).await?;
        let mut events = Vec::new();
        // The identifier and the dropped features travel in the stream itself,
        // so the streamed path reports what the whole path reports without the
        // caller having to seed either one.
        if let Some(id) = body.get("id").and_then(Value::as_str) {
            events.push(StreamEvent::response_id(id));
        }
        for feature in body
            .get("dropped")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
        {
            events.push(StreamEvent::warning(ResponseWarning::FeatureDropped {
                feature: feature.to_owned(),
            }));
        }
        let mut buffered = String::new();
        for event in body
            .get("events")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let text = || {
                event
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            match event.get("type").and_then(Value::as_str) {
                // The defect: hold every fragment and release one delta at the
                // end. It reassembles into the same answer, which is exactly
                // why reassembly alone never proved anything about streaming.
                Some("text") if self.defects.buffer_the_stream => buffered.push_str(&text()),
                Some("text") => events.push(StreamEvent::text(text())),
                Some("usage") if self.defects.drop_stream_usage => {}
                Some("usage") => events.push(StreamEvent::Usage {
                    usage: self.usage(Some(event)),
                }),
                Some("finish") => {
                    if !buffered.is_empty() {
                        events.push(StreamEvent::text(std::mem::take(&mut buffered)));
                    }
                    events.push(StreamEvent::Finish {
                        reason: FinishReason::Stop,
                    });
                }
                _ => return Err(ProviderError::malformed("unknown_stream_event")),
            }
        }
        Ok(ModelStream::from_events(events))
    }
}

// ---------------------------------------------------------------------------
// Factory and fixtures.
// ---------------------------------------------------------------------------

struct ToyFactory {
    defects: Defects,
}

impl ProviderFactory for ToyFactory {
    type Provider = ToyAdapter;

    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        Ok(ToyAdapter::new(base_url, api_key, self.defects))
    }
}

struct ToyFixtures;

/// A successful vendor answer carrying `content`.
fn ok_body(content: &str, finish: &str) -> Value {
    json!({
        "id": "toy_resp_1",
        "content": content,
        "finish": finish,
        "usage": {"input": 42, "output": 7}
    })
}

/// The capabilities the toy adapter declares, including prompt caching: the
/// vendor reports a cache figure, so the profile says so and the usage row
/// holds it to reading that figure rather than to zero.
fn toy_capabilities() -> ProviderCapabilities {
    ProviderCapabilities::minimal()
        .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
        .with_tool_calling(ToolCallingCapability::Parallel)
        .with_streaming(true)
        .with_prompt_caching(true)
        .with_preserves_call_ids(true)
        .with_max_context_tokens(128_000)
}

#[async_trait]
impl WireFixtures for ToyFixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        let template =
            match scenario {
                Scenario::ValidStructured => ResponseTemplate::new(200)
                    .set_body_json(ok_body(&payloads::valid_plan().to_string(), "stop")),
                Scenario::MalformedJson => ResponseTemplate::new(200)
                    .set_body_json(ok_body(payloads::MALFORMED_JSON, "stop")),
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
                    // Two mocks on the same endpoint: the streaming one matches
                    // first on the flag the adapter sets.
                    //
                    // The answer arrives in two events, and the counts on a
                    // third, which is what lets the suite tell a stream from a
                    // buffered body and catch a dropped final frame.
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
                // The counts a cache hit produces: the whole prompt, and the
                // part of it that did not have to be recomputed.
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
                Scenario::Refusal => ResponseTemplate::new(200)
                    .set_body_json(ok_body(payloads::REFUSAL_TEXT, "refusal")),
                Scenario::SlowResponse => ResponseTemplate::new(200)
                    .set_body_json(ok_body("too late", "stop"))
                    .set_delay(payloads::SLOW_RESPONSE_DELAY),
                Scenario::RateLimited => ResponseTemplate::new(429)
                    .insert_header(
                        "retry-after",
                        payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
                    )
                    .set_body_json(json!({"error": {"code": "rate_limit_exceeded"}})),
                Scenario::Authentication => ResponseTemplate::new(401).set_body_json(json!({
                    "error": {"code": "invalid_api_key", "message": "Incorrect API key provided"}
                })),
                Scenario::ContextOverflow => ResponseTemplate::new(400).set_body_json(json!({
                    "error": {"code": "context_length_exceeded"}
                })),
                Scenario::Authorization => ResponseTemplate::new(403).set_body_json(json!({
                    "error": {"code": "model_not_entitled"}
                })),
                Scenario::ModelNotFound => ResponseTemplate::new(404).set_body_json(json!({
                    "error": {"code": "model_not_found"}
                })),
                Scenario::RequestTimeout => ResponseTemplate::new(408).set_body_json(json!({
                    "error": {"code": "request_timeout"}
                })),
                Scenario::InvalidRequest => ResponseTemplate::new(400).set_body_json(json!({
                    "error": {"code": "unknown_parameter"}
                })),
                Scenario::ServerError => ResponseTemplate::new(500).set_body_json(json!({
                    "error": {"code": "internal_error"}
                })),
                Scenario::ServiceUnavailable => ResponseTemplate::new(503).set_body_json(json!({
                    "error": {"code": "overloaded"}
                })),
                // This toy vendor answers a filtered prompt with a successful
                // body whose finish reason says what happened, which is one of
                // the two shapes the row accepts.
                Scenario::ContentFilter => {
                    ResponseTemplate::new(200).set_body_json(ok_body("", "content_filter"))
                }
                // A 401 that is not a bad key: only the body says so.
                Scenario::ExpiredCredential => ResponseTemplate::new(401).set_body_json(json!({
                    "error": {"code": "token_expired", "message": "The bearer token has expired"}
                })),
                // A 429 that no amount of waiting will clear, carrying a
                // Retry-After the adapter must not take at face value.
                Scenario::QuotaExhausted => ResponseTemplate::new(429)
                    .insert_header("retry-after", "60")
                    .set_body_json(json!({
                        "error": {"code": "insufficient_quota", "message": "credit balance is zero"}
                    })),
                Scenario::SecretInBody => ResponseTemplate::new(200).set_body_json(json!({
                    "id": "toy_resp_1",
                    "content": payloads::valid_plan().to_string(),
                    "finish": "stop",
                    // A careless vendor echoes the credential back.
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

// ---------------------------------------------------------------------------
// The positive direction: a correct adapter passes.
// ---------------------------------------------------------------------------

async fn run(defects: Defects) -> ConformanceReport {
    run_all(&ToyFactory { defects }, &ToyFixtures).await
}

#[tokio::test]
async fn a_correct_adapter_passes_every_check() {
    let report = run(Defects::default()).await;
    assert!(report.passed(), "{report}");
    assert_eq!(report.provider.as_str(), "toy");
    assert_eq!(report.model.as_str(), "toy-1");
    assert_eq!(report.results.len(), Check::run_order().len());
    let (passed, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(
        skipped, 0,
        "a fully capable adapter skips nothing:\n{report}"
    );
    assert_eq!(passed, Check::run_order().len());
    // Nothing is unproven, so the table may claim every row.
    let table = report.compatibility_table();
    assert!(!table.contains("unproven |"), "{table}");
    assert!(
        table.contains("| `status_403_authorization` | proven |"),
        "{table}"
    );
}

#[tokio::test]
async fn every_check_reports_in_spec_order() {
    let report = run(Defects::default()).await;
    let order: Vec<Check> = report.results.iter().map(|result| result.check).collect();
    assert_eq!(order, Check::run_order());
}

// ---------------------------------------------------------------------------
// Per-status mapping: the rows that make a flattened error family visible.
// ---------------------------------------------------------------------------

/// The detail of one row, or a panic naming the report.
fn detail_of(report: &ConformanceReport, check: Check) -> String {
    report
        .result(check)
        .unwrap_or_else(|| panic!("{check} was not run:\n{report}"))
        .detail
        .clone()
        .unwrap_or_else(|| panic!("{check} carries no detail:\n{report}"))
}

#[tokio::test]
async fn an_adapter_that_reads_a_403_as_a_rejected_key_is_caught() {
    let report = run(Defects {
        confuse_403_with_401: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    let row = Check::StatusMapping(StatusRow::Forbidden);
    assert_caught(&report, row, &[]);
    let detail = detail_of(&report, row);
    // The message has to name all three: what was sent, what was required,
    // what arrived. Anything less and the adapter author guesses.
    assert!(detail.contains("HTTP 403"), "{detail}");
    assert!(detail.contains("expected authorization"), "{detail}");
    assert!(detail.contains("mapped to authentication"), "{detail}");
    // The 401 row still passes: the two are told apart, not merged.
    assert_eq!(
        report
            .result(Check::StatusMapping(StatusRow::Unauthorized))
            .expect("row ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
}

#[tokio::test]
async fn an_adapter_that_calls_a_context_length_400_a_bad_request_is_caught() {
    let report = run(Defects {
        context_length_as_bad_request: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    let row = Check::StatusMapping(StatusRow::ContextLength);
    // The §20.8 context-overflow row fails for the same reason; that is the
    // point of it, and it is the only legitimate collateral.
    assert_caught(&report, row, &[Check::ContextOverflow]);
    let detail = detail_of(&report, row);
    assert!(detail.contains("HTTP 400 (context length)"), "{detail}");
    assert!(detail.contains("expected context_overflow"), "{detail}");
    assert!(detail.contains("mapped to invalid_request"), "{detail}");
    // And the genuinely bad 400 still maps to invalid_request: the two bodies
    // carry the same status and must not be told apart by the status alone.
    assert_eq!(
        report
            .result(Check::StatusMapping(StatusRow::BadRequest))
            .expect("row ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
}

#[tokio::test]
async fn flattening_every_failure_now_fails_every_status_row_it_touches() {
    let report = run(Defects {
        flatten_errors: true,
        ..Defects::default()
    })
    .await;
    // This is the gap the per-status rows exist to close: before them, an
    // adapter that reported `transport` for every HTTP failure passed the
    // classification row, because `transport` really is retryable.
    for row in [
        StatusRow::Unauthorized,
        StatusRow::Forbidden,
        StatusRow::NotFound,
        StatusRow::RequestTimeout,
        StatusRow::TooManyRequests,
        StatusRow::ContextLength,
        StatusRow::BadRequest,
        StatusRow::InternalServerError,
        StatusRow::ServiceUnavailable,
    ] {
        let check = Check::StatusMapping(row);
        assert_eq!(
            report.result(check).expect("row ran").status,
            CheckStatus::Failed,
            "{row} accepted a flattened error:\n{report}"
        );
    }
    // The classification row still passes, which is exactly why it was never
    // enough on its own: `transport` is honestly retryable.
    assert_eq!(
        report
            .result(Check::RetryClassification)
            .expect("row ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
    // A reset legitimately *is* a transport failure, so that row is unmoved.
    assert_eq!(
        report
            .result(Check::StatusMapping(StatusRow::ConnectionReset))
            .expect("row ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
}

#[tokio::test]
async fn a_reset_socket_is_classified_rather_than_left_hanging() {
    // The suite owns this row: no mock can express a dead connection, so the
    // harness binds its own socket and closes it on the adapter.
    let report = run(Defects::default()).await;
    let row = Check::StatusMapping(StatusRow::ConnectionReset);
    assert_eq!(
        report.result(row).expect("row ran").status,
        CheckStatus::Passed,
        "{report}"
    );
}

#[tokio::test]
async fn an_adapter_that_reads_only_the_status_line_is_caught_on_both_body_rows() {
    let report = run(Defects {
        trust_the_status_line: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");

    // An expired token arrives on the same 401 as a bad key. An adapter that
    // branches on the status alone tells a caller holding a refresher that its
    // key is bad, and the caller stops instead of refreshing.
    let expired = Check::StatusMapping(StatusRow::ExpiredCredential);
    assert_eq!(
        report.result(expired).expect("row ran").status,
        CheckStatus::Failed,
        "{report}"
    );
    let detail = detail_of(&report, expired);
    assert!(detail.contains("expected credential_expired"), "{detail}");
    assert!(detail.contains("mapped to authentication"), "{detail}");
    assert!(detail.contains("refresher"), "{detail}");

    // A spent quota arrives on the same 429 as a rate limit, Retry-After and
    // all. Sleeping on it burns the turn's deadline for a balance only a human
    // can refill.
    let quota = Check::StatusMapping(StatusRow::QuotaExhausted);
    assert_eq!(
        report.result(quota).expect("row ran").status,
        CheckStatus::Failed,
        "{report}"
    );
    let detail = detail_of(&report, quota);
    assert!(detail.contains("expected quota_exhausted"), "{detail}");
    assert!(detail.contains("mapped to rate_limited"), "{detail}");
    assert!(detail.contains("waiting will not refill it"), "{detail}");

    // And the rows those two are confused with still pass: the suite is asking
    // for a distinction, not for a different blanket answer.
    for row in [StatusRow::Unauthorized, StatusRow::TooManyRequests] {
        assert_eq!(
            report
                .result(Check::StatusMapping(row))
                .expect("row ran")
                .status,
            CheckStatus::Passed,
            "{row} should be unaffected:\n{report}"
        );
    }
}

#[tokio::test]
async fn a_401_read_as_an_expiry_passes_the_credential_row_and_is_caught_where_it_matters() {
    // The authentication row asks one thing: does a rejected credential reach
    // the caller as a credential problem rather than as a network hiccup, with
    // the key nowhere in the error. All three credential kinds answer it, and a
    // vendor whose 401 prose mentions expiry would otherwise fail a row for a
    // mapping that is not what the row is about.
    let report = run(Defects {
        every_401_is_an_expiry: true,
        ..Defects::default()
    })
    .await;
    assert_eq!(
        report
            .result(Check::AuthenticationFailure)
            .expect("row ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );

    // The distinction itself is measured by the dedicated pair, and that is
    // where reading every 401 the same way is caught.
    let unauthorized = Check::StatusMapping(StatusRow::Unauthorized);
    assert_eq!(
        report.result(unauthorized).expect("row ran").status,
        CheckStatus::Failed,
        "{report}"
    );
    let detail = detail_of(&report, unauthorized);
    assert!(detail.contains("expected authentication"), "{detail}");
    assert!(detail.contains("mapped to credential_expired"), "{detail}");
    assert_eq!(
        report
            .result(Check::StatusMapping(StatusRow::ExpiredCredential))
            .expect("row ran")
            .status,
        CheckStatus::Passed,
        "the expiry body is still read correctly:\n{report}"
    );
}

#[tokio::test]
async fn the_two_body_rows_pass_when_the_adapter_reads_the_body() {
    let report = run(Defects::default()).await;
    for row in [StatusRow::ExpiredCredential, StatusRow::QuotaExhausted] {
        assert_eq!(
            report
                .result(Check::StatusMapping(row))
                .expect("row ran")
                .status,
            CheckStatus::Passed,
            "{row}:\n{report}"
        );
    }
}

/// Fixtures that mount everything but declare one row unproducible.
struct HonestlyLimitedFixtures;

#[async_trait]
impl WireFixtures for HonestlyLimitedFixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        ToyFixtures.mount(server, scenario).await;
    }

    fn status_support(&self, row: StatusRow) -> StatusSupport {
        match row {
            StatusRow::RequestTimeout => {
                StatusSupport::not_producible("this endpoint answers 504, never 408")
            }
            StatusRow::QuotaExhausted => StatusSupport::not_producible(
                "this endpoint is billed per seat and has no per-request quota signal",
            ),
            _ => StatusSupport::Mounted,
        }
    }
}

/// Fixtures that dodge a row without saying why.
struct UnexplainedFixtures;

#[async_trait]
impl WireFixtures for UnexplainedFixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        ToyFixtures.mount(server, scenario).await;
    }

    fn status_support(&self, row: StatusRow) -> StatusSupport {
        match row {
            StatusRow::ServiceUnavailable => StatusSupport::not_producible("   "),
            _ => StatusSupport::Mounted,
        }
    }
}

#[tokio::test]
async fn a_row_an_endpoint_cannot_produce_is_unproven_not_passed() {
    let report = run_all(
        &ToyFactory {
            defects: Defects::default(),
        },
        &HonestlyLimitedFixtures,
    )
    .await;
    assert!(
        report.passed(),
        "an honest limit is not a failure:\n{report}"
    );
    let row = Check::StatusMapping(StatusRow::RequestTimeout);
    assert_eq!(
        report.result(row).expect("row ran").status,
        CheckStatus::Skipped,
        "{report}"
    );
    let detail = detail_of(&report, row);
    assert!(detail.contains("HTTP 408"), "{detail}");
    assert!(detail.contains("answers 504"), "{detail}");
    assert!(detail.contains("unproven"), "{detail}");
    // A compatibility table built from this run cannot claim the row.
    let table = report.compatibility_table();
    assert!(
        table.contains("| `status_408_timeout` | unproven |"),
        "{table}"
    );
    // A quota signal the vendor cannot produce is unproven the same way: a
    // published table must not claim a mapping the run never saw.
    assert!(
        table.contains("| `quota_exhausted_kind` | unproven |"),
        "{table}"
    );
    assert!(table.contains("2 unproven"), "{table}");
    assert!(report.to_string().contains("2 row(s) unproven"), "{report}");
}

#[tokio::test]
async fn a_row_dodged_without_a_reason_fails_instead_of_skipping() {
    let report = run_all(
        &ToyFactory {
            defects: Defects::default(),
        },
        &UnexplainedFixtures,
    )
    .await;
    assert!(!report.passed(), "{report}");
    let row = Check::StatusMapping(StatusRow::ServiceUnavailable);
    assert_caught(&report, row, &[]);
    let detail = detail_of(&report, row);
    assert!(detail.contains("HTTP 503"), "{detail}");
    assert!(detail.contains("without a reason"), "{detail}");
}

// ---------------------------------------------------------------------------
// The negative direction: each defect is caught on its own row.
// ---------------------------------------------------------------------------

/// Asserts `check` failed and that no other check was collaterally broken
/// beyond those in `also_expected`.
fn assert_caught(report: &ConformanceReport, check: Check, also_expected: &[Check]) {
    let result = report
        .result(check)
        .unwrap_or_else(|| panic!("{check} was not run:\n{report}"));
    assert_eq!(
        result.status,
        CheckStatus::Failed,
        "the harness missed the planted defect on {check}:\n{report}"
    );
    assert!(result.detail.is_some(), "a failure must say why:\n{report}");
    for failure in report.failures() {
        assert!(
            failure.check == check || also_expected.contains(&failure.check),
            "unexpected collateral failure on {}:\n{report}",
            failure.check
        );
    }
}

#[tokio::test]
async fn an_adapter_that_never_sends_the_schema_is_caught() {
    let report = run(Defects {
        omit_schema: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    assert_caught(&report, Check::NoSilentCapabilityDowngrade, &[]);
}

#[tokio::test]
async fn an_adapter_that_leaks_its_key_in_debug_is_caught() {
    let report = run(Defects {
        leak_key_in_debug: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    assert_caught(&report, Check::SecretRedaction, &[]);
}

#[tokio::test]
async fn an_adapter_that_invents_a_valid_plan_is_caught_on_every_rejection_row() {
    let report = run(Defects {
        invent_a_valid_plan: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    // The defect is "always report a parseable plan", so every row that exists
    // to prove a bad payload is rejected must fail. `empty_output` is not among
    // them: this adapter answers an empty body before the defect can apply, and
    // the harness reports what actually happened rather than what was planted.
    for check in [
        Check::MalformedJson,
        Check::UnknownFields,
        Check::MissingRequiredFields,
    ] {
        let result = report.result(check).expect("check ran");
        assert_eq!(
            result.status,
            CheckStatus::Failed,
            "{check} accepted an invented plan:\n{report}"
        );
    }
    // And the rows that legitimately expect a valid plan still pass.
    assert_eq!(
        report
            .result(Check::ValidStructuredResponse)
            .expect("check ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
}

#[tokio::test]
async fn an_adapter_that_renames_call_ids_while_claiming_otherwise_is_caught() {
    let report = run(Defects {
        rename_call_ids: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    assert_caught(&report, Check::ToolAndReadRequestIds, &[]);
    let detail = report
        .result(Check::ToolAndReadRequestIds)
        .and_then(|result| result.detail.clone())
        .unwrap_or_default();
    assert!(
        detail.contains("lower the declaration"),
        "the failure must point at the declaration, not the test: {detail}"
    );
}

#[tokio::test]
async fn an_adapter_that_flattens_its_errors_is_caught_on_the_classification_rows() {
    let report = run(Defects {
        flatten_errors: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    // Every row whose failure arrives as an HTTP status must notice. `timeout`
    // is not one of them: the deadline is enforced locally, so it survives a
    // defect in status mapping — which is worth knowing, and is why the suite
    // exercises each family separately instead of one representative error.
    for check in [
        Check::RateLimit,
        Check::AuthenticationFailure,
        Check::ContextOverflow,
    ] {
        let result = report.result(check).expect("check ran");
        assert_eq!(
            result.status,
            CheckStatus::Failed,
            "{check} accepted a flattened error:\n{report}"
        );
    }
    assert_eq!(
        report.result(Check::Timeout).expect("check ran").status,
        CheckStatus::Passed,
        "a local deadline is unaffected by status mapping:\n{report}"
    );
}

#[tokio::test]
async fn a_broken_factory_fails_the_status_rows_too() {
    let report = run_all(&BrokenFactory, &ToyFixtures).await;
    for row in StatusRow::ALL {
        assert_eq!(
            report
                .result(Check::StatusMapping(row))
                .expect("row ran")
                .status,
            CheckStatus::Failed,
            "{row} must not pass when nothing could be built:\n{report}"
        );
    }
}

// ---------------------------------------------------------------------------
// The harness's own edge cases.
// ---------------------------------------------------------------------------

/// A factory that refuses to build anything.
struct BrokenFactory;

impl ProviderFactory for BrokenFactory {
    type Provider = ToyAdapter;

    fn build(&self, _base_url: &str, _api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        Err(ProviderError::invalid_request("misconfigured"))
    }
}

#[tokio::test]
async fn an_adapter_that_cannot_be_built_fails_every_row_without_panicking() {
    let report = run_all(&BrokenFactory, &ToyFixtures).await;
    assert!(!report.passed());
    assert_eq!(report.failures().len(), Check::run_order().len());
    assert_eq!(report.provider.as_str(), "unknown");
    assert!(report.to_string().contains("could not be built"));
}

/// A profile that declares nothing beyond text.
struct MinimalFactory;

/// The same adapter, but honest about being able to do very little.
struct MinimalAdapter(ToyAdapter);

impl fmt::Debug for MinimalAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

#[async_trait]
impl ModelProvider for MinimalAdapter {
    fn provider_key(&self) -> ProviderKey {
        self.0.provider_key()
    }

    fn model_key(&self) -> ModelKey {
        self.0.model_key()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::PromptOnly)
    }

    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        self.0.generate(request).await
    }
}

impl ProviderFactory for MinimalFactory {
    type Provider = MinimalAdapter;

    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        Ok(MinimalAdapter(ToyAdapter::new(
            base_url,
            api_key,
            Defects::default(),
        )))
    }
}

/// The toy vendor deployed with its streaming route switched off.
///
/// A profile that declares no streaming can no longer make the three streaming
/// rows disappear by staying quiet: it says so here, in words, and the report
/// calls them unproven.
struct NoStreamingFixtures;

#[async_trait]
impl WireFixtures for NoStreamingFixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        ToyFixtures.mount(server, scenario).await;
    }

    fn feature_support(&self, check: Check) -> RowSupport {
        if matches!(
            check,
            Check::StreamingReconstruction
                | Check::StreamingIncremental
                | Check::StreamingUsageAgreement
        ) {
            return RowSupport::not_producible(
                "this deployment runs the toy vendor with its streaming route switched off",
            );
        }
        RowSupport::Mounted
    }
}

#[tokio::test]
async fn an_honest_but_limited_profile_skips_rather_than_fails() {
    let report = run_all(&MinimalFactory, &NoStreamingFixtures).await;
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
            "{check} should be skipped for a prompt-only, tool-less, streamless profile:\n{report}"
        );
    }
    assert_eq!(report.skipped().len(), 5, "{report}");
    // And the table says why, for all three streaming rows.
    let table = report.compatibility_table();
    assert_eq!(
        table.matches("streaming route switched off").count(),
        3,
        "{table}"
    );
}

#[tokio::test]
async fn a_profile_with_no_streaming_and_nothing_to_say_fails_the_streaming_rows() {
    // The gap this closes: declaring `streaming: false` used to skip the row
    // for free, so a compatibility table showed a blank where an unimplemented
    // feature sat.
    let report = run_all(&MinimalFactory, &ToyFixtures).await;
    assert!(!report.passed(), "{report}");
    for check in [
        Check::StreamingReconstruction,
        Check::StreamingIncremental,
        Check::StreamingUsageAgreement,
    ] {
        let result = report.result(check).expect("check ran");
        assert_eq!(
            result.status,
            CheckStatus::Failed,
            "{check} was skipped for free:\n{report}"
        );
        let detail = result.detail.clone().unwrap_or_default();
        assert!(detail.contains("declares no streaming"), "{detail}");
        assert!(detail.contains("feature_support"), "{detail}");
    }
}

#[tokio::test]
async fn a_feature_row_declared_without_a_reason_fails_instead_of_skipping() {
    struct Silent;

    #[async_trait]
    impl WireFixtures for Silent {
        async fn mount(&self, server: &MockServer, scenario: Scenario) {
            ToyFixtures.mount(server, scenario).await;
        }

        fn feature_support(&self, check: Check) -> RowSupport {
            match check {
                Check::Refusal => RowSupport::not_producible(""),
                _ => RowSupport::Mounted,
            }
        }
    }

    let report = run_all(
        &ToyFactory {
            defects: Defects::default(),
        },
        &Silent,
    )
    .await;
    assert!(!report.passed(), "{report}");
    assert_caught(&report, Check::Refusal, &[]);
    let detail = detail_of(&report, Check::Refusal);
    assert!(detail.contains("without a reason"), "{detail}");
}

#[tokio::test]
async fn a_row_that_describes_the_adapter_cannot_be_declared_away() {
    struct Dodger;

    #[async_trait]
    impl WireFixtures for Dodger {
        async fn mount(&self, server: &MockServer, scenario: Scenario) {
            ToyFixtures.mount(server, scenario).await;
        }

        fn feature_support(&self, check: Check) -> RowSupport {
            match check {
                Check::SecretRedaction => {
                    RowSupport::not_producible("we would rather not be measured on this")
                }
                _ => RowSupport::Mounted,
            }
        }
    }

    let report = run_all(
        &ToyFactory {
            defects: Defects::default(),
        },
        &Dodger,
    )
    .await;
    assert!(!report.passed(), "{report}");
    assert_caught(&report, Check::SecretRedaction, &[]);
    let detail = detail_of(&report, Check::SecretRedaction);
    assert!(detail.contains("cannot be declared away"), "{detail}");
}

// ---------------------------------------------------------------------------
// Streaming is a promise about delivery and about counts, not only about
// reassembly.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_adapter_that_buffers_the_stream_reassembles_and_is_still_caught() {
    let report = run(Defects {
        buffer_the_stream: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    assert_caught(&report, Check::StreamingIncremental, &[]);
    let detail = detail_of(&report, Check::StreamingIncremental);
    assert!(detail.contains("one delta"), "{detail}");
    assert!(detail.contains("buffered"), "{detail}");
    // The row it used to hide behind still passes, which is the whole point:
    // the answer is right and the delivery is useless.
    assert_eq!(
        report
            .result(Check::StreamingReconstruction)
            .expect("row ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
}

#[tokio::test]
async fn an_adapter_that_drops_the_final_usage_frame_is_caught() {
    let report = run(Defects {
        drop_stream_usage: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    assert_caught(&report, Check::StreamingUsageAgreement, &[]);
    let detail = detail_of(&report, Check::StreamingUsageAgreement);
    assert!(detail.contains("input=0"), "{detail}");
    assert!(detail.contains("input=42"), "{detail}");
    // Reassembly and incrementality are both unaffected: the counts are the
    // only thing lost, and only this row looks at them.
    for check in [Check::StreamingReconstruction, Check::StreamingIncremental] {
        assert_eq!(
            report.result(check).expect("row ran").status,
            CheckStatus::Passed,
            "{report}"
        );
    }
}

#[tokio::test]
async fn an_adapter_reporting_a_net_input_count_is_caught() {
    let report = run(Defects {
        net_cached_usage: true,
        ..Defects::default()
    })
    .await;
    assert!(!report.passed(), "{report}");
    assert_caught(&report, Check::TokenUsageContract, &[]);
    let detail = detail_of(&report, Check::TokenUsageContract);
    assert!(detail.contains("cached_input=30"), "{detail}");
    assert!(detail.contains("input=12"), "{detail}");
    assert!(detail.contains("subset"), "{detail}");
}

#[tokio::test]
async fn the_correct_adapter_reports_the_gross_prompt_and_the_cache_inside_it() {
    let server = MockServer::start().await;
    ToyFixtures.mount(&server, Scenario::CachedUsage).await;
    let adapter = ToyAdapter::new(
        &server.uri(),
        ApiKey::new(payloads::DUMMY_API_KEY),
        Defects::default(),
    );
    let usage = adapter
        .generate(payloads::narration_request())
        .await
        .expect("the cached fixture")
        .usage;
    assert_eq!(usage.input, payloads::USAGE_INPUT_TOKENS);
    assert_eq!(usage.cached_input, payloads::USAGE_CACHED_TOKENS);
    assert!(usage.cached_input < usage.input, "cached is a subset");
}

#[tokio::test]
async fn the_stream_carries_the_identifier_and_the_dropped_feature_itself() {
    // Neither had anywhere to go before: the identifier had to be seeded and
    // the warning was simply lost, so the same call was honest whole and silent
    // streamed.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(ENDPOINT))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "toy_resp_stream",
            "dropped": ["stop_sequences"],
            "events": [
                {"type": "text", "text": "ciao"},
                {"type": "usage", "input": 42, "output": 7},
                {"type": "finish"}
            ]
        })))
        .mount(&server)
        .await;
    let adapter = ToyAdapter::new(
        &server.uri(),
        ApiKey::new(payloads::DUMMY_API_KEY),
        Defects::default(),
    );
    let request = payloads::narration_request();
    let stream = adapter.stream(request.clone()).await.expect("stream");
    let rebuilt = turnframe_provider::stream::reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "toy", "toy-1"),
    )
    .await
    .expect("reassembly");
    assert_eq!(rebuilt.raw_id.as_deref(), Some("toy_resp_stream"));
    assert!(rebuilt.warnings.contains(&ResponseWarning::FeatureDropped {
        feature: "stop_sequences".to_owned(),
    }));
    assert_eq!(rebuilt.usage, TokenUsage::new(42, 7));
}

// ---------------------------------------------------------------------------
// The corpus itself, as the harness sends it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_structured_request_carries_the_schema_marker_on_the_wire() {
    let server = MockServer::start().await;
    ToyFixtures.mount(&server, Scenario::ValidStructured).await;
    let adapter = ToyAdapter::new(
        &server.uri(),
        ApiKey::new(payloads::DUMMY_API_KEY),
        Defects::default(),
    );
    adapter
        .generate(payloads::structured_request())
        .await
        .expect("valid fixture");
    let requests = server.received_requests().await.expect("recording enabled");
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains(payloads::SCHEMA_MARKER), "{body}");
    assert!(
        matches!(
            payloads::structured_request().output,
            OutputSpec::Json { .. }
        ),
        "the corpus request must ask for structured output"
    );
}

#[tokio::test]
async fn the_toy_adapter_reassembles_a_stream_into_the_whole_answer() {
    let server = MockServer::start().await;
    ToyFixtures
        .mount(&server, Scenario::StreamingReconstruction)
        .await;
    let adapter = ToyAdapter::new(
        &server.uri(),
        ApiKey::new(payloads::DUMMY_API_KEY),
        Defects::default(),
    );
    let request = payloads::narration_request();
    let whole = adapter
        .generate(request.clone())
        .await
        .expect("whole answer");
    let stream = adapter.stream(request.clone()).await.expect("stream");
    let rebuilt = turnframe_provider::stream::reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "toy", "toy-1"),
    )
    .await
    .expect("reassembly");
    assert_eq!(rebuilt.text(), whole.text());
    assert_eq!(rebuilt.content, whole.content);
    assert!(matches!(rebuilt.content[0], ContentPart::Text { .. }));
}
