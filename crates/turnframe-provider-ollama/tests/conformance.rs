//! The conformance suite of spec §20.8, run against this adapter.
//!
//! This file is the deliverable that proves the crate. It runs the whole suite
//! from `turnframe_provider::conformance` — the twenty feature rows and the
//! thirteen per-status rows — against a wiremock server speaking Ollama's
//! native `/api/chat` format, three times over:
//!
//! * a **bare local daemon**, with no credential anywhere, declaring what a
//!   model measured against it earned. It passes every row it can produce and
//!   declares the three credential rows **unproven**, with reasons, because a
//!   daemon that authenticates nothing cannot put them on the wire;
//! * the **same daemon behind an authenticating proxy**, with a bearer token.
//!   It must pass every row with nothing skipped, because a proxy really does
//!   answer 401 for a bad token, 401 for an expired one and 429 for a spent
//!   balance;
//! * a **small unmeasured model** on the bare daemon, declaring only
//!   [`baseline`] — `json_object`, no tools. It must *pass* while skipping the
//!   rows it cannot honestly claim, because honesty is not a failure.
//!
//! Everything the fixtures return is Ollama framing around the suite's own
//! corpus: the schema, the payloads and the planted credential all come from
//! `payloads`, so this adapter is measured on the same thing every other
//! adapter is.
//!
//! **Conformance is per provider-model pair.** These runs say something about
//! this adapter against these fixtures. They say nothing about `qwen3:8b`
//! versus `smollm2:135m` behind the same daemon — which is exactly why the
//! third run exists.
//!
//! # What a bare daemon cannot be measured on
//!
//! [`WireFixtures::status_support`] lets a fixture say, in words, that its
//! endpoint cannot produce a **per-status** row, and
//! [`WireFixtures::feature_support`] now says the same about the feature rows a
//! deployment can genuinely lack. A daemon started with `ollama serve`
//! authenticates nobody, meters nothing and filters nothing, so the bare run
//! declares `authentication_failure`, `rate_limit` and `refusal` unproven,
//! along with the five per-status rows that mirror them. The proxied run proves
//! all eight, which is the only deployment where they are reachable at all.

// The compatibility table is printed on purpose: `cargo test -p
// turnframe-provider-ollama -- --nocapture` is how an adopter gets the table
// for their own daemon and their own model into their own documentation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

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
use turnframe_provider::error::{ProviderError, ProviderErrorKind};
use turnframe_provider::prelude::*;
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::secret::ApiKey;
use turnframe_provider::stream::{StreamAccumulator, reconstruct};
use turnframe_provider_ollama::declarations::baseline;
use turnframe_provider_ollama::{MODEL_NOT_PULLED_CODE, OllamaProvider};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The model the measured runs are configured with, tagged as `ollama list`
/// prints it.
const MODEL: &str = "qwen3:8b";

/// A model nobody has measured, which is the point of the third run.
const SMALL_MODEL: &str = "smollm2:135m";

/// The native chat route. Ollama serves it at a fixed path.
const CHAT_PATH: &str = "/api/chat";

/// The prose the streaming scenario answers with, whole and in fragments.
const NARRATION: &str = "Ho preparato la modifica.";

/// The counts every fixture reports, so the two paths can be compared on them.
///
/// Taken from the suite's own corpus: the usage row checks the reported prompt
/// against it, and a local runtime reports the whole prompt because it has no
/// cache to have served part of it from.
const PROMPT_TOKENS: u64 = payloads::USAGE_INPUT_TOKENS;

/// Generated tokens every fixture reports.
const EVAL_TOKENS: u64 = payloads::USAGE_OUTPUT_TOKENS;

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Builds this adapter against a mock server, for one deployment shape.
struct Factory {
    model: &'static str,
    capabilities: ProviderCapabilities,
    /// Whether this deployment has a credential at all. A bare daemon does not,
    /// and the factory then ignores the suite's key entirely — which is the
    /// point: the credential-less path is what most adopters run.
    authenticated: bool,
}

/// The declaration a model earns after a passing run against this daemon.
///
/// Raised from [`baseline`] field by field, which is the only honest way to get
/// here: the daemon backs the streaming and the JSON, the *model* has to earn
/// the schema, the tools and the vision. `preserves_call_ids` stays false and
/// the builder would refuse it otherwise, because Ollama's chat format has no
/// call id to preserve.
fn measured() -> ProviderCapabilities {
    baseline()
        .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
        .with_tool_calling(ToolCallingCapability::Parallel)
        .with_vision(true)
        .with_max_context_tokens(32_768)
}

impl Factory {
    /// The ordinary case: `ollama serve` on this machine, no credential.
    fn local() -> Self {
        Self {
            model: MODEL,
            capabilities: measured(),
            authenticated: false,
        }
    }

    /// The same daemon behind a proxy that authenticates.
    fn proxied() -> Self {
        Self {
            model: MODEL,
            capabilities: measured(),
            authenticated: true,
        }
    }

    /// A small model nobody has measured, on the bare daemon.
    fn small_model() -> Self {
        Self {
            model: SMALL_MODEL,
            capabilities: baseline(),
            authenticated: false,
        }
    }
}

impl ProviderFactory for Factory {
    type Provider = OllamaProvider;

    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        let mut builder = OllamaProvider::at(base_url)
            .model(self.model)
            .capabilities(self.capabilities.clone());
        if self.authenticated {
            builder = builder.bearer_token(api_key);
        }
        builder.build().map_err(ProviderError::from)
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The vendor half of the suite: Ollama's own wire shapes.
struct Fixtures {
    /// Whether the endpoint under test sits behind something that
    /// authenticates. A bare daemon does not, and three per-status rows are
    /// declared unproven for it.
    authenticated: bool,
}

impl Fixtures {
    /// The fixtures of a bare daemon.
    fn local() -> Self {
        Self {
            authenticated: false,
        }
    }

    /// The fixtures of a daemon behind an authenticating proxy.
    fn proxied() -> Self {
        Self {
            authenticated: true,
        }
    }
}

/// A completed `/api/chat` answer carrying `content`.
///
/// The duration fields are real: the daemon sends four of them and this crate
/// reads none, which is what the tolerant envelope is for.
fn chat(content: &str, done_reason: &str) -> Value {
    json!({
        "model": MODEL,
        "created_at": "2026-09-05T10:00:00.000000Z",
        "message": {"role": "assistant", "content": content},
        "done": true,
        "done_reason": done_reason,
        "total_duration": 5_191_566_416_u64,
        "load_duration": 2_154_458_u64,
        "prompt_eval_count": PROMPT_TOKENS,
        "prompt_eval_duration": 383_809_000_u64,
        "eval_count": EVAL_TOKENS,
        "eval_duration": 4_799_921_000_u64
    })
}

/// The daemon's error body: a bare string under `error`, and nothing else.
fn api_error(message: &str) -> Value {
    json!({ "error": message })
}

/// One newline-delimited frame.
fn frame(payload: &Value) -> String {
    format!("{payload}\n")
}

/// The streamed twin of [`chat`] for the narration answer.
///
/// Three frames of prose and a fourth carrying `done` and the counts, which is
/// how the daemon really sends it: the counts exist only on the last frame, so
/// an adapter that dropped it would report no usage at all.
fn narration_stream() -> String {
    let mut body = String::new();
    for fragment in ["Ho ", "preparato ", "la modifica."] {
        body.push_str(&frame(&json!({
            "model": MODEL,
            "created_at": "2026-09-05T10:00:00.000000Z",
            "message": {"role": "assistant", "content": fragment},
            "done": false
        })));
    }
    body.push_str(&frame(&json!({
        "model": MODEL,
        "created_at": "2026-09-05T10:00:01.000000Z",
        "message": {"role": "assistant", "content": ""},
        "done": true,
        "done_reason": "stop",
        "total_duration": 5_191_566_416_u64,
        "prompt_eval_count": PROMPT_TOKENS,
        "eval_count": EVAL_TOKENS
    })));
    body
}

/// The answer a tool-calling model gives, in Ollama's shape: a function, an
/// arguments **object**, and no id anywhere.
fn tool_call_answer() -> Value {
    json!({
        "model": MODEL,
        "created_at": "2026-09-05T10:00:00.000000Z",
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": {
                    "name": payloads::TOOL_NAME,
                    "arguments": {"target": "tok_1"}
                }
            }]
        },
        "done": true,
        "done_reason": "stop",
        "prompt_eval_count": 12,
        "eval_count": 4
    })
}

/// The template each scenario answers with, once streaming is out of the way.
fn template(scenario: Scenario) -> ResponseTemplate {
    match scenario {
        Scenario::ValidStructured => ResponseTemplate::new(200)
            .set_body_json(chat(&payloads::valid_plan().to_string(), "stop")),
        Scenario::MalformedJson => {
            ResponseTemplate::new(200).set_body_json(chat(payloads::MALFORMED_JSON, "stop"))
        }
        Scenario::UnknownField => ResponseTemplate::new(200).set_body_json(chat(
            &payloads::plan_with_unknown_field().to_string(),
            "stop",
        )),
        Scenario::MissingField => ResponseTemplate::new(200).set_body_json(chat(
            &payloads::plan_with_missing_field().to_string(),
            "stop",
        )),
        Scenario::MultipleActs => ResponseTemplate::new(200)
            .set_body_json(chat(&payloads::two_act_plan().to_string(), "stop")),
        Scenario::ToolCallIds => ResponseTemplate::new(200).set_body_json(tool_call_answer()),
        Scenario::StreamingReconstruction => {
            ResponseTemplate::new(200).set_body_json(chat(NARRATION, "stop"))
        }
        // Mounted for the usage row. `prompt_eval_count` is the whole prompt
        // and the daemon reports no cache figure at all, which satisfies the
        // contract the row states — `cached_input` is a subset of `input`, and
        // an empty subset is a subset.
        Scenario::CachedUsage => ResponseTemplate::new(200).set_body_json(chat(NARRATION, "stop")),
        // The shape an exhausted answer really takes here: a successful body
        // with an empty content string, not an empty HTTP body.
        Scenario::EmptyOutput => ResponseTemplate::new(200).set_body_json(chat("", "stop")),
        // A bare daemon has no refusal channel at all — a model that declines
        // answers in ordinary prose. What can refuse is a policy gateway in
        // front of it, and this is its shape.
        Scenario::Refusal | Scenario::ContentFilter => ResponseTemplate::new(403).set_body_json(
            api_error("the prompt was blocked by policy before it reached the runner"),
        ),
        Scenario::SlowResponse => ResponseTemplate::new(200)
            .set_body_json(chat("troppo tardi", "stop"))
            .set_delay(payloads::SLOW_RESPONSE_DELAY),
        Scenario::RateLimited => ResponseTemplate::new(429)
            .insert_header(
                "retry-after",
                payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
            )
            .set_body_json(api_error("too many requests: slow down")),
        Scenario::Authentication => ResponseTemplate::new(401)
            .set_body_json(api_error("unauthorized: invalid bearer token")),
        // The runner's own words when the prompt will not fit the window it
        // was given.
        Scenario::ContextOverflow => ResponseTemplate::new(400).set_body_json(api_error(
            "the request exceeds the available context size: 215048 tokens > 4096 maximum. \
             try increasing the context size or enable context shift",
        )),
        Scenario::Authorization => ResponseTemplate::new(403)
            .set_body_json(api_error("this token is not permitted to use that runner")),
        // The failure of a local runtime, by a wide margin: the tag was never
        // pulled. The daemon's wording, verbatim.
        Scenario::ModelNotFound => ResponseTemplate::new(404).set_body_json(api_error(
            "model \"qwen3:8b\" not found, try pulling it first",
        )),
        Scenario::RequestTimeout => ResponseTemplate::new(408)
            .set_body_json(api_error("the upstream gave up waiting for a first token")),
        // A genuine Ollama 400 no reasonable adapter could mistake for a
        // context problem: the body did not decode.
        Scenario::InvalidRequest => ResponseTemplate::new(400).set_body_json(api_error(
            "json: cannot unmarshal string into Go struct field ChatRequest.messages \
             of type []api.Message",
        )),
        Scenario::ServerError => ResponseTemplate::new(500).set_body_json(api_error(
            "llama runner process has terminated: exit status 2",
        )),
        Scenario::ServiceUnavailable => {
            ResponseTemplate::new(503).set_body_json(api_error("no healthy upstream"))
        }
        // A token that was valid: the same 401 a wrong one gets, and only the
        // words tell them apart.
        Scenario::ExpiredCredential => ResponseTemplate::new(401).set_body_json(api_error(
            "the bearer token expired on 2026-09-01; obtain a new one",
        )),
        // A spent balance on a 429 — the status of a rate limit, and nothing a
        // caller can wait out.
        Scenario::QuotaExhausted => ResponseTemplate::new(429)
            .insert_header(
                "retry-after",
                payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
            )
            .set_body_json(api_error(
                "your credit balance is too low to run this model; top up to continue",
            )),
        // The credential is echoed back where a careless proxy puts it: in a
        // debug envelope field and in a response header. Neither is read by the
        // adapter, and neither may survive into any rendering of it.
        Scenario::SecretInBody => {
            let mut body = chat(&payloads::valid_plan().to_string(), "stop");
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
        _ => ResponseTemplate::new(200).set_body_json(chat("", "stop")),
    }
}

#[async_trait]
impl WireFixtures for Fixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        if scenario == Scenario::StreamingReconstruction {
            // Mounted first, and matched on the flag the adapter puts on the
            // wire, so the non-streamed call falls through to the twin below.
            // Ollama frames a stream as newline-delimited JSON, not as
            // server-sent events, so the media type says so too.
            Mock::given(method("POST"))
                .and(path(CHAT_PATH))
                .and(body_string_contains("\"stream\":true"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(narration_stream(), "application/x-ndjson"),
                )
                .mount(server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path(CHAT_PATH))
            .respond_with(template(scenario))
            .mount(server)
            .await;
    }

    /// The three rows a daemon that authenticates nothing cannot put on the
    /// wire.
    ///
    /// They are declared rather than skipped silently, so the report says
    /// **unproven** with a reason instead of quietly passing. The same adapter
    /// maps all three, and the proxied run proves it — which is the only
    /// deployment where they are reachable at all.
    fn status_support(&self, row: StatusRow) -> StatusSupport {
        if self.authenticated {
            return StatusSupport::Mounted;
        }
        match row {
            StatusRow::Unauthorized => StatusSupport::not_producible(
                "a daemon started with `ollama serve` authenticates nothing: /api/chat serves \
                 every request that reaches it, so this deployment never answers 401",
            ),
            StatusRow::ExpiredCredential => StatusSupport::not_producible(
                "this deployment issues and holds no credential, so it has none that can \
                 expire; an expired bearer token is reachable only through the authenticating \
                 proxy the second run measures",
            ),
            StatusRow::QuotaExhausted => StatusSupport::not_producible(
                "a local runtime meters nothing and bills nobody: there is no quota and no \
                 credit balance for /api/chat to exhaust",
            ),
            StatusRow::TooManyRequests => StatusSupport::not_producible(
                "the daemon queues requests behind the runner instead of rejecting them: \
                 there is no rate limiter in front of /api/chat to answer 429",
            ),
            StatusRow::ContentFilter => StatusSupport::not_producible(
                "nothing between the caller and the runner inspects the prompt: a bare daemon \
                 has no safety filter, and a model that declines answers in ordinary prose",
            ),
            _ => StatusSupport::Mounted,
        }
    }

    /// The three feature rows a bare daemon cannot put on the wire either.
    ///
    /// They are the counterparts of the per-status rows above: authentication,
    /// rate limiting and refusal are all properties of something standing in
    /// front of the runner, and on this deployment nothing does. Declaring them
    /// in words is what keeps the compatibility table honest — before the hook
    /// existed the bare run was measured on proxy-shaped bodies it could never
    /// have produced.
    fn feature_support(&self, check: Check) -> RowSupport {
        if self.authenticated {
            return RowSupport::Mounted;
        }
        match check {
            Check::AuthenticationFailure => RowSupport::not_producible(
                "a daemon started with `ollama serve` authenticates nothing, so no credential \
                 of any kind is ever rejected by it",
            ),
            Check::RateLimit => RowSupport::not_producible(
                "the daemon queues requests behind the runner rather than rejecting them, so \
                 it never asks a caller to wait",
            ),
            Check::Refusal => RowSupport::not_producible(
                "a bare daemon has no refusal channel: a model that declines says so in \
                 ordinary prose, and the policy shape this row wants comes from a gateway \
                 that is not present here",
            ),
            _ => RowSupport::Mounted,
        }
    }
}

// ---------------------------------------------------------------------------
// The runs
// ---------------------------------------------------------------------------

/// Every row a full run reports, so a count is checked against the suite rather
/// than against a number that would rot.
fn every_row() -> Vec<Check> {
    Check::run_order()
}

/// The per-status rows a bare daemon declares unproven.
const BARE_STATUS_ROWS: [StatusRow; 5] = [
    StatusRow::Unauthorized,
    StatusRow::ExpiredCredential,
    StatusRow::QuotaExhausted,
    StatusRow::TooManyRequests,
    StatusRow::ContentFilter,
];

/// The feature rows a bare daemon declares unproven, for the same reasons.
const BARE_FEATURE_ROWS: [Check; 3] = [
    Check::AuthenticationFailure,
    Check::RateLimit,
    Check::Refusal,
];

/// How many rows a bare-daemon run leaves unproven.
const BARE_UNPROVEN: usize = BARE_STATUS_ROWS.len() + BARE_FEATURE_ROWS.len();

async fn run(factory: Factory, fixtures: Fixtures) -> ConformanceReport {
    run_all(&factory, &fixtures).await
}

/// Asserts the report says nothing it did not demonstrate.
fn assert_shape(report: &ConformanceReport) {
    let order: Vec<Check> = report.results.iter().map(|result| result.check).collect();
    assert_eq!(order, every_row(), "{report}");
    for result in &report.results {
        if result.status == CheckStatus::Skipped {
            let reason = result.detail.as_deref().unwrap_or_default();
            assert!(
                !reason.trim().is_empty(),
                "{} is unproven without a reason, which is indistinguishable \
                 from a dodged failure:\n{report}",
                result.check
            );
        }
    }
}

#[tokio::test]
async fn a_bare_local_daemon_passes_every_row_it_can_produce() {
    let report = run(Factory::local(), Fixtures::local()).await;
    assert!(report.passed(), "{report}");
    assert_eq!(report.provider.as_str(), "ollama");
    assert_eq!(report.model.as_str(), MODEL);
    assert_shape(&report);

    let (passed, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    // Exactly the rows something in front of the runner would have produced are
    // unproven, and each says why.
    assert_eq!(skipped, BARE_UNPROVEN, "{report}");
    assert_eq!(passed, every_row().len() - BARE_UNPROVEN);
    let declared = BARE_STATUS_ROWS
        .into_iter()
        .map(Check::StatusMapping)
        .chain(BARE_FEATURE_ROWS);
    for check in declared {
        let result = report.result(check).expect("the row ran");
        assert_eq!(result.status, CheckStatus::Skipped, "{check}:\n{report}");
        let reason = result.detail.as_deref().unwrap_or_default();
        assert!(reason.contains("unproven"), "{reason}");
    }
    // Streaming is not among them: the daemon streams by default, so the row
    // is proven rather than declared away.
    assert_eq!(
        report
            .result(Check::StreamingReconstruction)
            .expect("the row ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
}

#[tokio::test]
async fn the_same_daemon_behind_a_proxy_proves_the_credential_rows_too() {
    let report = run(Factory::proxied(), Fixtures::proxied()).await;
    assert!(report.passed(), "{report}");
    assert_shape(&report);
    let (passed, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(
        skipped, 0,
        "a proxied instance can produce every row, and a skip is not a pass:\n{report}"
    );
    assert_eq!(passed, every_row().len());
}

#[tokio::test]
async fn a_small_unmeasured_model_passes_by_skipping_what_it_cannot_claim() {
    let report = run(Factory::small_model(), Fixtures::local()).await;
    assert!(report.passed(), "honesty is not a failure:\n{report}");
    assert_eq!(report.model.as_str(), SMALL_MODEL);
    assert_shape(&report);
    for check in [
        Check::ToolAndReadRequestIds,
        Check::NoSilentCapabilityDowngrade,
    ] {
        assert_eq!(
            report.result(check).expect("the row ran").status,
            CheckStatus::Skipped,
            "{check} cannot be proven by a json_object, tool-less model:\n{report}"
        );
    }
    // Streaming belongs to the daemon, not to the model, so even here it is
    // proven rather than skipped.
    assert_eq!(
        report
            .result(Check::StreamingReconstruction)
            .expect("the row ran")
            .status,
        CheckStatus::Passed,
        "{report}"
    );
    let (_, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(skipped, BARE_UNPROVEN + 2, "{report}");
}

#[tokio::test]
async fn the_compatibility_table_reports_an_unproven_row_as_unproven() {
    let report = run(Factory::local(), Fixtures::local()).await;
    let table = report.compatibility_table();
    // Run with `--nocapture` to lift this into your own documentation.
    println!("{report}\n{table}");
    assert!(
        table.contains(&format!("{BARE_UNPROVEN} unproven")),
        "{table}"
    );
    assert!(
        table.contains("authenticates nothing"),
        "the table must carry the reason, not just the verdict:\n{table}"
    );
    // And it never claims a row the run did not demonstrate.
    assert!(
        !table.contains("| `expired_credential_kind` | proven |"),
        "{table}"
    );
}

// ---------------------------------------------------------------------------
// What actually went on the wire
// ---------------------------------------------------------------------------

/// Builds a provider against a fresh server with `scenario` mounted.
async fn staged(factory: &Factory, scenario: Scenario) -> (MockServer, OllamaProvider) {
    let server = MockServer::start().await;
    Fixtures::local().mount(&server, scenario).await;
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
async fn the_request_goes_to_the_native_route_not_the_compatibility_one() {
    let factory = Factory::local();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let requests = server.received_requests().await.expect("recording enabled");
    assert_eq!(requests[0].url.path(), CHAT_PATH);
    // A bare daemon takes no credential, and none is invented for it.
    assert!(!requests[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn a_schema_capable_model_puts_the_schema_in_the_format_field() {
    let factory = Factory::local();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;
    // `format` carries the schema itself, which is what makes the declaration
    // `NativeJsonSchema` true rather than aspirational.
    assert_eq!(body["format"], payloads::plan_schema());
    assert!(
        body.to_string().contains(payloads::SCHEMA_MARKER),
        "the schema itself must travel, not just its name"
    );
    // And the declared window travels, or the daemon would truncate the prompt
    // to its own small default without saying so.
    assert_eq!(body["options"]["num_ctx"], 32_768);
}

#[tokio::test]
async fn a_small_model_does_not_quietly_upgrade_itself() {
    let factory = Factory::small_model();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;
    // The weaker transport, and the schema described in the prompt instead.
    assert_eq!(body["format"], "json");
    let system = body["messages"][0]["content"]
        .as_str()
        .expect("a system message");
    assert!(system.contains("JSON Schema"), "{system}");
    assert!(system.contains(payloads::SCHEMA_MARKER), "{system}");
    // Nothing was declared, so no window is imposed either.
    assert!(body["options"].get("num_ctx").is_none(), "{body}");
}

#[tokio::test]
async fn a_proxied_instance_sends_its_bearer_token_and_nothing_else() {
    let factory = Factory::proxied();
    let server = MockServer::start().await;
    Fixtures::proxied()
        .mount(&server, Scenario::ValidStructured)
        .await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let requests = server.received_requests().await.expect("recording enabled");
    let authorization = requests[0]
        .headers
        .get("authorization")
        .expect("a bearer token")
        .to_str()
        .expect("ascii");
    assert_eq!(authorization, format!("Bearer {}", payloads::DUMMY_API_KEY));
    // The token reaches the wire and nowhere else.
    assert!(!format!("{provider:?}").contains(&payloads::DUMMY_API_KEY[..20]));
}

#[tokio::test]
async fn the_streamed_answer_equals_the_whole_one_field_for_field() {
    let factory = Factory::local();
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
    // The counts ride on the last frame of the stream; the two paths agree on
    // them, and a local runtime reports no cached tokens either way.
    assert_eq!(rebuilt.usage, whole.usage);
    assert_eq!(rebuilt.usage, TokenUsage::new(PROMPT_TOKENS, EVAL_TOKENS));
    assert_eq!(rebuilt.usage.cached_input, 0);
    // The reassembled answer says it was reassembled; that is the one honest
    // difference between the paths.
    assert!(rebuilt.warnings.contains(&ResponseWarning::Reconstructed));
    assert!(!whole.warnings.contains(&ResponseWarning::Reconstructed));
}

#[tokio::test]
async fn the_streamed_path_reports_the_feature_it_dropped_just_as_the_whole_one_does() {
    // `/api/chat` has no cache hint and no metadata field. Both are dropped,
    // and until the stream had a warning event the whole call said so and the
    // streamed call said nothing about the same request.
    let factory = Factory::local();
    let (_server, provider) = staged(&factory, Scenario::StreamingReconstruction).await;
    let request = payloads::narration_request()
        .with_cache_hint(CacheHint::System)
        .with_metadata("workflow", "trip")
        .expect("a label");
    let dropped = ResponseWarning::FeatureDropped {
        feature: "cache_hint".to_owned(),
    };

    let whole = provider.generate(request.clone()).await.expect("whole");
    assert!(whole.warnings.contains(&dropped), "{:?}", whole.warnings);

    let stream = provider.stream(request.clone()).await.expect("stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "ollama", MODEL),
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
async fn the_stream_delivers_prose_in_pieces_rather_than_in_one_lump() {
    let factory = Factory::local();
    let (_server, provider) = staged(&factory, Scenario::StreamingReconstruction).await;
    let stream = provider
        .stream(payloads::narration_request())
        .await
        .expect("stream");
    let deltas: Vec<String> = stream
        .collect_items()
        .await
        .into_iter()
        .filter_map(|item| match item.expect("no failure") {
            StreamEvent::TextDelta { text } => Some(text),
            _ => None,
        })
        .collect();
    // An adapter that buffered the body and emitted it at the end would
    // reassemble correctly and give an adopter nothing. Three frames, three
    // deltas.
    assert_eq!(deltas, vec!["Ho ", "preparato ", "la modifica."]);
}

#[tokio::test]
async fn a_streamed_tool_call_reassembles_into_the_non_streamed_answer() {
    let server = MockServer::start().await;
    let mut body = frame(&json!({
        "model": MODEL,
        "message": {"role": "assistant", "content": "", "tool_calls": [{
            "function": {"name": payloads::TOOL_NAME, "arguments": {"target": "tok_1"}}
        }]},
        "done": false
    }));
    body.push_str(&frame(&json!({
        "model": MODEL,
        "message": {"role": "assistant", "content": ""},
        "done": true, "done_reason": "stop",
        "prompt_eval_count": 12, "eval_count": 4
    })));

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/x-ndjson"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(template(Scenario::ToolCallIds))
        .mount(&server)
        .await;

    let provider = Factory::local()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::tool_request();
    let whole = provider.generate(request.clone()).await.expect("whole");
    let stream = provider.stream(request.clone()).await.expect("stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "ollama", MODEL),
    )
    .await
    .expect("reassembles");

    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.finish, whole.finish);
    // The id came from this adapter, identically on both paths, because the
    // wire carries none.
    assert_eq!(rebuilt.tool_calls()[0].id.as_str(), "call_0");
    assert_eq!(whole.tool_calls()[0].id.as_str(), "call_0");
    assert_eq!(
        rebuilt.tool_calls()[0].arguments,
        json!({"target": "tok_1"})
    );
    assert!(
        whole
            .warnings
            .contains(&ResponseWarning::SynthesizedCallIds)
    );
}

#[tokio::test]
async fn a_stream_that_dies_mid_answer_is_a_typed_failure_not_a_short_one() {
    let server = MockServer::start().await;
    let body = frame(&json!({
        "model": MODEL,
        "message": {"role": "assistant", "content": "meta "},
        "done": false
    }));
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/x-ndjson"))
        .mount(&server)
        .await;
    let provider = Factory::local()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::narration_request();
    let stream = provider.stream(request.clone()).await.expect("stream");
    let error = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "ollama", MODEL),
    )
    .await
    .expect_err("a truncated stream is not a short answer");
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("stream_ended_without_finish".to_owned())
    );
}

#[tokio::test]
async fn a_stream_cut_in_the_middle_of_a_frame_is_reported_as_truncated() {
    let server = MockServer::start().await;
    // A whole frame, then half of one: the connection died mid-object.
    let mut body = frame(&json!({
        "model": MODEL,
        "message": {"role": "assistant", "content": "meta "},
        "done": false
    }));
    body.push_str("{\"model\":\"qwen3:8b\",\"message\":{\"role\":\"assis");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/x-ndjson"))
        .mount(&server)
        .await;
    let provider = Factory::local()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::narration_request();
    let stream = provider.stream(request.clone()).await.expect("stream");
    let items = stream.collect_items().await;
    let error = items
        .into_iter()
        .find_map(Result::err)
        .expect("the half frame is a failure, not a silent completion");
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("stream_ended_mid_frame".to_owned())
    );
    assert!(matches!(error.kind(), ProviderErrorKind::Malformed));
}

#[tokio::test]
async fn a_model_that_was_never_pulled_says_so_instead_of_failing_generically() {
    let factory = Factory::local();
    let (_server, provider) = staged(&factory, Scenario::ModelNotFound).await;
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("the tag is not on this machine");
    assert!(matches!(error.kind(), ProviderErrorKind::ModelNotFound));
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some(MODEL_NOT_PULLED_CODE.to_owned())
    );
    assert_eq!(error.retry_class(), RetryClass::Fallback);
    // The daemon's prose is read, classified and dropped.
    assert!(!error.to_string().contains("try pulling"), "{error}");
}

#[tokio::test]
async fn a_failing_call_never_renders_the_configured_token() {
    let factory = Factory::proxied();
    let server = MockServer::start().await;
    Fixtures::proxied()
        .mount(&server, Scenario::Authentication)
        .await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a 401 is a failure");
    for rendering in [
        error.to_string(),
        format!("{error:?}"),
        format!("{provider:?}"),
    ] {
        assert!(
            !rendering.contains(&payloads::DUMMY_API_KEY[..20]),
            "the credential surfaced: {rendering}"
        );
        assert!(
            !rendering.contains("invalid bearer token"),
            "the response body surfaced: {rendering}"
        );
    }
    assert_eq!(
        error.to_string(),
        format!("provider call failed: authentication [ollama/{MODEL}]")
    );
    assert_eq!(error.retry_class(), RetryClass::Fallback);
}

#[tokio::test]
async fn a_rate_limit_keeps_its_delay_and_a_spent_balance_does_not_pretend_to_be_one() {
    let factory = Factory::proxied();
    let fixtures = Fixtures::proxied();

    let server = MockServer::start().await;
    fixtures.mount(&server, Scenario::RateLimited).await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a 429 is a failure");
    assert_eq!(
        error.retry_after(),
        Some(Duration::from_secs(payloads::RETRY_AFTER_SECONDS))
    );
    assert_eq!(error.retry_class(), RetryClass::RetryAfter);

    // The same status, the same advertised delay, and nothing to wait for.
    let server = MockServer::start().await;
    fixtures.mount(&server, Scenario::QuotaExhausted).await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a spent balance is a failure");
    assert!(matches!(
        error.kind(),
        ProviderErrorKind::QuotaExhausted { .. }
    ));
    assert_eq!(error.retry_class(), RetryClass::Fallback);
    assert_eq!(
        error.retry_after(),
        None,
        "sleeping on a delay does not refill a balance"
    );
}
