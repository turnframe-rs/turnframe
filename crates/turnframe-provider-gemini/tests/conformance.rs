//! The conformance suite of spec §20.8, run against this adapter.
//!
//! This file is the deliverable that proves the crate. It runs the whole
//! twenty-row feature suite from `turnframe_provider::conformance`, and its
//! thirteen per-status rows, against a
//! wiremock server speaking Gemini's `generateContent` shapes, three times
//! over:
//!
//! * the **Gemini developer API** profile, with every capability declared —
//!   nothing may be skipped, because a skipped row is not a pass;
//! * the **Vertex AI** profile, whose project-shaped path and per-call bearer
//!   token are a different surface for the same behaviour, and which must pass
//!   the same rows without skipping one either;
//! * a **modest** profile that declares `json_object` and no tool calling —
//!   which must *pass* while skipping the rows it cannot honestly claim,
//!   because honesty is not a failure.
//!
//! Everything the fixtures return is Gemini framing around the suite's own
//! corpus: the schema, the payloads and the planted credential all come from
//! `payloads`, so this adapter is measured on the same thing every other
//! adapter is.
//!
//! **Conformance is per provider-model pair.** These runs say something about
//! this adapter against these fixtures. They say nothing about
//! `gemini-2.5-flash` versus `gemini-2.5-pro`, and nothing about the same model
//! id served from a region nobody has measured.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// A conformance report is the evidence for a capability declaration, so the
// runs print theirs: `cargo test -- --nocapture` leaves the whole table in the
// CI log, and evidence nobody can read is not evidence.
#![allow(clippy::print_stdout)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::conformance::{
    Check, CheckStatus, ConformanceReport, ProviderFactory, Scenario, WireFixtures, payloads,
    run_all,
};
use turnframe_provider::error::{ProviderError, ProviderErrorKind};
use turnframe_provider::prelude::*;
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::secret::ApiKey;
use turnframe_provider::stream::{StreamAccumulator, reconstruct};
use turnframe_provider_gemini::credential::{StaticToken, TokenError, TokenFn};
use turnframe_provider_gemini::profile::EndpointProfile;
use turnframe_provider_gemini::{ConfigError, GeminiProvider, SchemaError, translate_schema};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The model every run is configured with. URL-safe, because both route shapes
/// put it in the path.
const MODEL: &str = "gemini-2.5-flash";

/// Matches the whole-answer method of both route shapes.
const GENERATE_PATH: &str = r".*:generateContent$";

/// Matches the streamed method of both route shapes.
const STREAM_PATH: &str = r".*:streamGenerateContent$";

/// The prose the streaming scenario answers with, whole and in fragments.
const NARRATION: &str = "Ho preparato la modifica.";

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Which surface a run exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The developer API: an API key in `x-goog-api-key`.
    Developer,
    /// Vertex AI: a bearer token fetched per call.
    Vertex,
}

/// Builds this adapter against a mock server, for one profile.
struct Factory {
    surface: Surface,
    capabilities: Option<ProviderCapabilities>,
}

impl Factory {
    const fn developer() -> Self {
        Self {
            surface: Surface::Developer,
            capabilities: None,
        }
    }

    const fn vertex() -> Self {
        Self {
            surface: Surface::Vertex,
            capabilities: None,
        }
    }

    /// A model nobody has measured: `json_object`, no tools, streaming.
    fn modest() -> Self {
        Self {
            surface: Surface::Developer,
            capabilities: Some(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::JsonObject)
                    .with_tool_calling(ToolCallingCapability::None)
                    .with_streaming(true),
            ),
        }
    }

    /// A profile that carries the schema through a **forced function call**
    /// instead of `responseSchema`: the other transport the same endpoint has.
    fn forced_function() -> Self {
        Self {
            surface: Surface::Developer,
            capabilities: Some(
                EndpointProfile::gemini()
                    .capabilities()
                    .clone()
                    .with_structured_output(StructuredOutputCapability::NativeFunctionSchema),
            ),
        }
    }

    fn profile(&self) -> EndpointProfile {
        match self.surface {
            Surface::Developer => EndpointProfile::gemini(),
            Surface::Vertex => EndpointProfile::vertex_ai("aurora-prod", "europe-west4"),
        }
    }
}

impl ProviderFactory for Factory {
    type Provider = GeminiProvider;

    fn build(&self, base_url: &str, api_key: ApiKey) -> Result<Self::Provider, ProviderError> {
        let mut builder = GeminiProvider::builder(self.profile())
            .base_url(base_url)
            .model(MODEL);
        builder = match self.surface {
            Surface::Developer => builder.api_key(api_key),
            // The same planted credential, through the door Vertex uses: a
            // token source consulted once per request.
            Surface::Vertex => builder.token_source(Arc::new(StaticToken::new(api_key))),
        };
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

/// A completed `generateContent` answer carrying `text`.
fn completion(text: &str, finish: &str) -> Value {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": text}]},
            "finishReason": finish,
            "index": 0,
            "safetyRatings": [
                {"category": "HARM_CATEGORY_HATE_SPEECH", "probability": "NEGLIGIBLE"}
            ],
            "avgLogprobs": -0.31
        }],
        // `promptTokenCount` already contains the cached tokens, which is the
        // contract the usage row holds every adapter to.
        "usageMetadata": {
            "promptTokenCount": payloads::USAGE_INPUT_TOKENS,
            "candidatesTokenCount": payloads::USAGE_OUTPUT_TOKENS,
            "totalTokenCount": payloads::USAGE_INPUT_TOKENS + payloads::USAGE_OUTPUT_TOKENS,
            "cachedContentTokenCount": payloads::USAGE_CACHED_TOKENS
        },
        "modelVersion": MODEL,
        "responseId": "resp-turnframe-1"
    })
}

/// A Google API error envelope.
fn api_error(code: u16, status: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message, "status": status}})
}

/// One server-sent event extra.
fn sse(payload: &Value) -> String {
    format!("data: {payload}\n\n")
}

/// The streamed twin of [`completion`] for the narration answer.
///
/// Gemini sends no `[DONE]` sentinel: the last chunk is the one carrying a
/// `finishReason`, and the connection then closes.
fn narration_stream() -> String {
    let slice = |text: &str, finish: Option<&str>| {
        let mut candidate = json!({
            "content": {"role": "model", "parts": [{"text": text}]},
            "index": 0
        });
        if let Some(finish) = finish {
            candidate["finishReason"] = json!(finish);
        }
        json!({"candidates": [candidate], "modelVersion": MODEL,
               "responseId": "resp-turnframe-1"})
    };
    let mut body = String::new();
    body.push_str(&sse(&slice("Ho preparato ", None)));
    let mut last = slice("la modifica.", Some("STOP"));
    last["usageMetadata"] = json!({
        "promptTokenCount": payloads::USAGE_INPUT_TOKENS,
        "candidatesTokenCount": payloads::USAGE_OUTPUT_TOKENS,
        "totalTokenCount": payloads::USAGE_INPUT_TOKENS + payloads::USAGE_OUTPUT_TOKENS,
        "cachedContentTokenCount": payloads::USAGE_CACHED_TOKENS
    });
    body.push_str(&sse(&last));
    body
}

/// The template each scenario answers with.
fn template(scenario: Scenario) -> ResponseTemplate {
    match scenario {
        Scenario::ValidStructured => ResponseTemplate::new(200)
            .set_body_json(completion(&payloads::valid_plan().to_string(), "STOP")),
        Scenario::MalformedJson => {
            ResponseTemplate::new(200).set_body_json(completion(payloads::MALFORMED_JSON, "STOP"))
        }
        Scenario::UnknownField => ResponseTemplate::new(200).set_body_json(completion(
            &payloads::plan_with_unknown_field().to_string(),
            "STOP",
        )),
        Scenario::MissingField => ResponseTemplate::new(200).set_body_json(completion(
            &payloads::plan_with_missing_field().to_string(),
            "STOP",
        )),
        Scenario::MultipleActs => ResponseTemplate::new(200)
            .set_body_json(completion(&payloads::two_act_plan().to_string(), "STOP")),
        // Gemini's `functionCall` carries no id, which is why the profile
        // declares `preserves_call_ids: false` and the adapter synthesizes one.
        Scenario::ToolCallIds => ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"functionCall": {
                        "name": payloads::TOOL_NAME,
                        "args": {"target": "tok_1"}
                    }}]
                },
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {
                "promptTokenCount": 12, "candidatesTokenCount": 4, "totalTokenCount": 16
            },
            "modelVersion": MODEL
        })),
        Scenario::StreamingReconstruction => {
            ResponseTemplate::new(200).set_body_json(completion(NARRATION, "STOP"))
        }
        // Mounted for the usage row: `completion` already reports a prompt with
        // a cached slice inside it, which is what that row reads.
        Scenario::CachedUsage => {
            ResponseTemplate::new(200).set_body_json(completion(NARRATION, "STOP"))
        }
        // A candidate that finished cleanly with nothing in it: the shape an
        // exhausted answer really takes, not an empty HTTP body.
        Scenario::EmptyOutput => ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": []},
                            "finishReason": "STOP", "index": 0}],
            "usageMetadata": {"promptTokenCount": 12, "totalTokenCount": 12},
            "modelVersion": MODEL
        })),
        // Gemini refuses by blocking: no content, a safety finish reason and
        // the prompt feedback that says why.
        Scenario::Refusal => ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "finishReason": "SAFETY",
                "index": 0,
                "safetyRatings": [
                    {"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "probability": "HIGH",
                     "blocked": true}
                ]
            }],
            "promptFeedback": {"blockReason": "SAFETY"},
            "usageMetadata": {"promptTokenCount": 12, "totalTokenCount": 12},
            "modelVersion": MODEL
        })),
        Scenario::SlowResponse => ResponseTemplate::new(200)
            .set_body_json(completion("troppo tardi", "STOP"))
            .set_delay(payloads::SLOW_RESPONSE_DELAY),
        Scenario::RateLimited => ResponseTemplate::new(429)
            .insert_header(
                "retry-after",
                payloads::RETRY_AFTER_SECONDS.to_string().as_str(),
            )
            .set_body_json(json!({"error": {
                "code": 429,
                "message": "Quota exceeded for quota metric 'Generate Content API requests'.",
                "status": "RESOURCE_EXHAUSTED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                    "violations": [{
                        "quotaId": "GenerateRequestsPerMinutePerProjectPerModel",
                        "quotaMetric": "generativelanguage.googleapis.com/generate_requests"
                    }]
                }]
            }})),
        Scenario::Authentication => ResponseTemplate::new(401).set_body_json(api_error(
            401,
            "UNAUTHENTICATED",
            "API key not valid. Please pass a valid API key.",
        )),
        Scenario::ContextOverflow => ResponseTemplate::new(400).set_body_json(api_error(
            400,
            "INVALID_ARGUMENT",
            "The input token count (1250000) exceeds the maximum number of tokens \
             allowed (1048576).",
        )),
        // ------------------------------------------------------------------
        // The per-status rows. Google carries the meaning in `error.status`,
        // its gRPC canonical name, which is more specific than the HTTP status
        // it rides on — and for the last two it is the *only* thing that tells
        // the row apart from its neighbour.
        // ------------------------------------------------------------------
        Scenario::Authorization => ResponseTemplate::new(403).set_body_json(api_error(
            403,
            "PERMISSION_DENIED",
            "The caller does not have permission to access the model.",
        )),
        Scenario::ModelNotFound => ResponseTemplate::new(404).set_body_json(api_error(
            404,
            "NOT_FOUND",
            "models/gemini-does-not-exist is not found for API version v1beta.",
        )),
        Scenario::RequestTimeout => ResponseTemplate::new(408).set_body_json(api_error(
            408,
            "DEADLINE_EXCEEDED",
            "The request deadline passed before the operation completed.",
        )),
        // A 400 that is genuinely a bad request: Google's REST layer rejects a
        // body carrying a field its proto does not declare, and that is the
        // most common way to get one.
        Scenario::InvalidRequest => ResponseTemplate::new(400).set_body_json(api_error(
            400,
            "INVALID_ARGUMENT",
            "Invalid JSON payload received. Unknown name \"temprature\" at \
             'generation_config'.",
        )),
        Scenario::ServerError => ResponseTemplate::new(500).set_body_json(api_error(
            500,
            "INTERNAL",
            "An internal error has occurred. Please retry.",
        )),
        Scenario::ServiceUnavailable => ResponseTemplate::new(503).set_body_json(api_error(
            503,
            "UNAVAILABLE",
            "The model is overloaded. Please try again later.",
        )),
        // A safety block arrives over a 200, not over an error status: the call
        // succeeded and the answer was withheld.
        Scenario::ContentFilter => template(Scenario::Refusal),
        // An expired token and a wrong key share HTTP 401 and Google's
        // `UNAUTHENTICATED`. Only the message and the challenge header say
        // which, which is the whole point of the row.
        Scenario::ExpiredCredential => ResponseTemplate::new(401)
            .insert_header(
                "www-authenticate",
                "Bearer error=\"invalid_token\", error_description=\"Invalid Credentials\"",
            )
            .set_body_json(api_error(
                401,
                "UNAUTHENTICATED",
                "Request had invalid authentication credentials. Expected OAuth 2 access \
                 token. The access token has expired.",
            )),
        // A spent quota and a rate limit share HTTP 429 and Google's
        // `RESOURCE_EXHAUSTED`. The `QuotaFailure` detail names the window: a
        // quota counted per *day* does not reopen in three seconds, so the
        // `Retry-After` the response still carries must not be honoured.
        Scenario::QuotaExhausted => ResponseTemplate::new(429)
            .insert_header("retry-after", "3")
            .set_body_json(json!({"error": {
                "code": 429,
                "message": "You exceeded your current quota. Please check your plan and \
                            billing details.",
                "status": "RESOURCE_EXHAUSTED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                    "violations": [{
                        "quotaId": "GenerateRequestsPerDayPerProjectPerModel-FreeTier",
                        "quotaMetric":
                            "generativelanguage.googleapis.com/generate_content_requests"
                    }]
                }]
            }})),
        // The credential is echoed back where a careless proxy puts it: in a
        // debug envelope field and in a response header. Neither is read by the
        // adapter, and neither may survive into any rendering of it.
        Scenario::SecretInBody => ResponseTemplate::new(200)
            .insert_header(
                "x-upstream-authorization",
                format!("Bearer {}", payloads::DUMMY_API_KEY).as_str(),
            )
            .set_body_json(json!({
                "candidates": [{
                    "content": {"role": "model",
                                "parts": [{"text": payloads::valid_plan().to_string()}]},
                    "finishReason": "STOP",
                    "index": 0
                }],
                "modelVersion": MODEL,
                "_debug": {"x-goog-api-key": payloads::DUMMY_API_KEY}
            })),
        // `Scenario` is growable. A scenario this fixture has not learned to
        // frame answers with a failure nothing maps onto, so a new row fails
        // loudly instead of quietly looking like a valid answer.
        _ => ResponseTemplate::new(418).set_body_json(api_error(
            418,
            "UNKNOWN",
            "this fixture does not model that scenario yet",
        )),
    }
}

#[async_trait]
impl WireFixtures for Fixtures {
    async fn mount(&self, server: &MockServer, scenario: Scenario) {
        if scenario == Scenario::StreamingReconstruction {
            // The two calls are told apart by the *method suffix* in the path,
            // which is how Gemini distinguishes them — there is no flag in the
            // body to match on.
            Mock::given(method("POST"))
                .and(path_regex(STREAM_PATH))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(narration_stream(), "text/event-stream"),
                )
                .mount(server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path_regex(GENERATE_PATH))
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

/// Asserts a run proved every row of the table with nothing skipped.
///
/// The report is printed so `cargo test -- --nocapture` leaves the whole table
/// in the CI log: a run that passes is the evidence for the declaration, and
/// evidence nobody can read is not evidence.
fn assert_complete(report: &ConformanceReport, provider: &str) {
    println!("{report}");
    assert!(report.passed(), "{report}");
    assert_eq!(report.provider.as_str(), provider, "{report}");
    assert_eq!(report.model.as_str(), MODEL, "{report}");
    // Every row of the table, feature rows and per-status rows alike.
    let expected = Check::run_order();
    assert_eq!(report.results.len(), expected.len(), "{report}");
    let (passed, failed, skipped) = report.counts();
    assert_eq!(failed, 0, "{report}");
    assert_eq!(
        skipped, 0,
        "a fully capable profile proves every row:\n{report}"
    );
    assert_eq!(passed, expected.len(), "{report}");
    let order: Vec<Check> = report.results.iter().map(|result| result.check).collect();
    assert_eq!(order, expected, "{report}");
    // And a compatibility table published from it claims nothing the run did
    // not prove.
    let table = report.compatibility_table();
    assert!(!table.contains("unproven |"), "{table}");
    assert!(table.contains("0 unproven"), "{table}");
}

#[tokio::test]
async fn the_developer_api_profile_passes_every_row_without_skipping_one() {
    let report = run(Factory::developer()).await;
    assert_complete(&report, "gemini");
}

#[tokio::test]
async fn the_vertex_profile_passes_the_same_rows_through_a_different_surface() {
    let report = run(Factory::vertex()).await;
    assert_complete(&report, "vertex-ai");
}

#[tokio::test]
async fn a_modest_profile_passes_by_skipping_what_it_cannot_claim() {
    let report = run(Factory::modest()).await;
    println!("{report}");
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

// ---------------------------------------------------------------------------
// What actually went on the wire
// ---------------------------------------------------------------------------

/// Builds a provider against a fresh server with `scenario` mounted.
async fn staged(factory: &Factory, scenario: Scenario) -> (MockServer, GeminiProvider) {
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
async fn a_schema_enforcing_profile_puts_the_translated_schema_on_the_wire() {
    let factory = Factory::developer();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;
    let config = &body["generationConfig"];
    assert_eq!(config["responseMimeType"], "application/json");
    // Translated into Gemini's dialect, not copied.
    assert_eq!(config["responseSchema"]["type"], "OBJECT");
    assert_eq!(config["responseSchema"]["title"], payloads::SCHEMA_NAME);
    assert_eq!(
        config["responseSchema"]["properties"]["acts"]["type"],
        "ARRAY"
    );
    assert!(
        body.to_string().contains(payloads::SCHEMA_MARKER),
        "the schema itself must travel, not just its name"
    );
    // The framing instruction is a field, never a turn in the conversation.
    assert!(body["systemInstruction"]["parts"][0]["text"].is_string());
    assert_eq!(body["contents"][0]["role"], "user");
}

#[tokio::test]
async fn a_json_object_profile_does_not_quietly_upgrade_itself() {
    let factory = Factory::modest();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let body = first_body(&server).await;
    // The weaker transport, and the schema described in the instruction.
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert!(body["generationConfig"].get("responseSchema").is_none());
    let instruction = body["systemInstruction"]["parts"][0]["text"]
        .as_str()
        .expect("a system instruction");
    assert!(instruction.contains("JSON"), "{instruction}");
    assert!(
        instruction.contains(payloads::SCHEMA_MARKER),
        "{instruction}"
    );
}

#[tokio::test]
async fn a_schema_gemini_cannot_express_is_refused_and_never_weakened() {
    let factory = Factory::developer();
    let (server, provider) = staged(&factory, Scenario::ValidStructured).await;

    // A `oneOf` whose branches can both match one document has no equivalent
    // in the dialect. Mapping it onto `anyOf` would widen what the model may
    // return under a `NativeJsonSchema` declaration — the silent downgrade rule
    // 9 forbids. A union whose branches are provably exclusive is a different
    // case and does translate; this one is not that.
    let hostile = json!({
        "type": "object",
        "properties": {"act": {"oneOf": [{"type": "object"}, {"type": "object"}]}},
        "required": ["act"]
    });
    let request = payloads::structured_request()
        .with_output(OutputSpec::json("hostile_plan", hostile.clone()));
    let error = provider
        .generate(request)
        .await
        .expect_err("a schema that cannot be enforced is not sent");

    assert!(matches!(
        error.kind(),
        ProviderErrorKind::Unsupported { .. }
    ));
    // `Fallback`, not `Fatal`: the router may offer a candidate that can carry
    // it, which is not a downgrade.
    assert_eq!(error.retry_class(), RetryClass::Fallback);
    let rendered = error.to_string();
    assert!(rendered.contains("response_schema:oneOf"), "{rendered}");
    assert!(rendered.contains("_properties_act"), "{rendered}");

    // The safety property: nothing reached the wire at all. A weakened schema
    // would have produced a 200 the caller could not tell apart from an
    // enforced one.
    let requests = server.received_requests().await.expect("recording enabled");
    assert!(
        requests.is_empty(),
        "a schema that cannot be enforced must not be sent in a weaker form"
    );

    // And the same refusal is available before deployment, naming the keyword
    // and the pointer.
    let refused = translate_schema(&hostile).expect_err("oneOf");
    assert!(matches!(refused, SchemaError::UnsupportedValue { .. }));
    assert_eq!(refused.keyword(), "oneOf");
    assert_eq!(refused.pointer(), "/properties/act");
}

#[tokio::test]
async fn each_surface_routes_and_authenticates_the_way_google_expects() {
    let developer = Factory::developer();
    let (server, provider) = staged(&developer, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let requests = server.received_requests().await.expect("recording enabled");
    assert!(
        requests[0]
            .url
            .path()
            .ends_with(&format!("/models/{MODEL}:generateContent")),
        "{}",
        requests[0].url
    );
    assert!(requests[0].headers.contains_key("x-goog-api-key"));
    assert!(!requests[0].headers.contains_key("authorization"));
    // The developer API's `?key=` form is never used: a URL reaches access logs.
    assert!(requests[0].url.query().is_none(), "{}", requests[0].url);

    let vertex = Factory::vertex();
    let (server, provider) = staged(&vertex, Scenario::ValidStructured).await;
    provider
        .generate(payloads::structured_request())
        .await
        .expect("a valid fixture");
    let requests = server.received_requests().await.expect("recording enabled");
    assert!(
        requests[0].url.path().ends_with(&format!(
            "/projects/aurora-prod/locations/europe-west4/publishers/google/models/\
             {MODEL}:generateContent"
        )),
        "{}",
        requests[0].url
    );
    // Vertex authenticates with a bearer token, fetched for this one call.
    let authorization = requests[0].headers["authorization"]
        .to_str()
        .expect("a header value");
    assert!(authorization.starts_with("Bearer "), "{authorization}");
    assert!(!requests[0].headers.contains_key("x-goog-api-key"));
    // And Vertex is the only surface that takes labels.
    assert!(provider.endpoint_profile().quirks().send_labels);
}

#[tokio::test]
async fn the_streamed_answer_equals_the_whole_one_field_for_field() {
    for factory in [Factory::developer(), Factory::vertex()] {
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
        // Usage arrives on the last chunk, so the two agree on it too.
        assert_eq!(rebuilt.usage, whole.usage);
        assert_eq!(rebuilt.usage.input, payloads::USAGE_INPUT_TOKENS);
        assert_eq!(rebuilt.usage.cached_input, payloads::USAGE_CACHED_TOKENS);
        // The service's own `responseId` rides in the stream, so the rebuilt
        // answer carries the identifier the whole one does without a seed.
        assert_eq!(rebuilt.raw_id, whole.raw_id);
        assert_eq!(rebuilt.raw_id.as_deref(), Some("resp-turnframe-1"));
        // The reassembled answer says it was reassembled; that is the one
        // honest difference between the paths.
        assert!(rebuilt.warnings.contains(&ResponseWarning::Reconstructed));
        assert!(!whole.warnings.contains(&ResponseWarning::Reconstructed));
    }
}

#[tokio::test]
async fn the_streamed_path_reports_the_feature_it_dropped_just_as_the_whole_one_does() {
    // The service takes five stop sequences. A sixth is dropped, and until the
    // stream had a warning event the whole call said so and the streamed call
    // said nothing about the same request.
    let factory = Factory::developer();
    let (_server, provider) = staged(&factory, Scenario::StreamingReconstruction).await;
    let request = payloads::narration_request().with_stop(
        ["a", "b", "c", "d", "e", "f"]
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
        StreamAccumulator::new(request.request_id, "gemini", MODEL),
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
    let server = MockServer::start().await;
    let mut body = String::new();
    // Gemini delivers a function call whole, in one chunk, rather than slicing
    // its arguments the way a JSON-string argument list gets sliced elsewhere.
    body.push_str(&sse(&json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"functionCall": {
                "name": payloads::TOOL_NAME,
                "args": {"target": "tok_1"}
            }}]},
            "index": 0
        }],
        "modelVersion": MODEL
    })));
    body.push_str(&sse(&json!({
        "candidates": [{"content": {"role": "model", "parts": []},
                        "finishReason": "STOP", "index": 0}],
        "usageMetadata": {"promptTokenCount": 12, "candidatesTokenCount": 4,
                          "totalTokenCount": 16},
        "modelVersion": MODEL
    })));

    Mock::given(method("POST"))
        .and(path_regex(STREAM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(GENERATE_PATH))
        .respond_with(template(Scenario::ToolCallIds))
        .mount(&server)
        .await;

    let provider = Factory::developer()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::tool_request();
    let whole = provider.generate(request.clone()).await.expect("whole");
    let stream = provider.stream(request.clone()).await.expect("stream");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "gemini", MODEL),
    )
    .await
    .expect("reassembles");

    assert_eq!(rebuilt.content, whole.content);
    assert_eq!(rebuilt.finish, whole.finish);
    // The id is synthesized identically on both paths, which is what makes
    // `preserves_call_ids: false` a workable declaration rather than a hole.
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
    let body = sse(&json!({
        "candidates": [{"content": {"role": "model", "parts": [{"text": "meta "}]},
                        "index": 0}],
        "modelVersion": MODEL
    }));
    Mock::given(method("POST"))
        .and(path_regex(STREAM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider = Factory::developer()
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let request = payloads::narration_request();
    let stream = provider.stream(request.clone()).await.expect("stream");
    let error = reconstruct(
        stream,
        StreamAccumulator::new(request.request_id, "gemini", MODEL),
    )
    .await
    .expect_err("a truncated stream is not a short answer");
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("stream_ended_without_finish".to_owned())
    );
}

#[tokio::test]
async fn a_failing_call_never_renders_the_configured_credential() {
    for (factory, provider_key) in [
        (Factory::developer(), "gemini"),
        (Factory::vertex(), "vertex-ai"),
    ] {
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
            // Nor does anything else the service said: the prose of the error
            // body and the response headers are read, classified and dropped.
            assert!(
                !rendering.contains("API key not valid"),
                "the response body surfaced: {rendering}"
            );
        }
        // What survives is the kind, the keys and a sanitized machine code.
        assert_eq!(
            error.to_string(),
            format!(
                "provider call failed: authentication [{provider_key}/{MODEL}] \
                 code=UNAUTHENTICATED"
            )
        );
        assert_eq!(error.retry_class(), RetryClass::Fallback);
    }
}

#[tokio::test]
async fn an_expired_vertex_token_is_its_own_kind_and_points_at_the_refresher() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(GENERATE_PATH))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header(
                    "www-authenticate",
                    "Bearer error=\"invalid_token\", error_description=\"Invalid Credentials\"",
                )
                .set_body_json(api_error(
                    401,
                    "UNAUTHENTICATED",
                    "Request had invalid authentication credentials. Expected OAuth 2 access \
                     token. The access token has expired.",
                )),
        )
        .mount(&server)
        .await;

    // A token source that hands out a stale token, as a real one does the
    // moment its cached token ages out.
    let provider = GeminiProvider::vertex_ai("aurora-prod", "europe-west4")
        .base_url(server.uri())
        .model(MODEL)
        .token_source(Arc::new(TokenFn::new(|| async {
            Ok(ApiKey::new(payloads::DUMMY_API_KEY))
        })))
        .build()
        .expect("builds");

    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("an expired token is a failure");

    // Its own kind, not a generic authentication failure: a refresh fixes it.
    assert!(matches!(error.kind(), ProviderErrorKind::CredentialExpired));
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("credential_expired".to_owned())
    );
    // `Fallback`, never `Fatal`: a caller holding a refresher retries the same
    // profile, and a caller without one moves to the next candidate.
    assert_eq!(error.retry_class(), RetryClass::Fallback);
    assert!(!error.retry_class().allows_same_provider());
    assert!(error.retry_class().allows_another_candidate());
    assert!(
        !error.to_string().contains(&payloads::DUMMY_API_KEY[..20]),
        "{error}"
    );

    // And a source that cannot mint one at all is an authentication failure,
    // because there is nothing to refresh.
    let broken = GeminiProvider::vertex_ai("aurora-prod", "europe-west4")
        .base_url(server.uri())
        .model(MODEL)
        .token_source(Arc::new(TokenFn::new(|| async {
            Err::<ApiKey, _>(TokenError::failed("metadata_server_unreachable"))
        })))
        .build()
        .expect("builds");
    let error = broken
        .generate(payloads::narration_request())
        .await
        .expect_err("no token at all");
    assert!(matches!(error.kind(), ProviderErrorKind::Authentication));
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("metadata_server_unreachable".to_owned())
    );
}

#[tokio::test]
async fn a_spent_quota_is_its_own_kind_and_a_rate_limit_keeps_its_delay() {
    // The suite's own 429 fixture names a per-minute quota: a window that
    // reopens, so it is a rate limit and the delay is preserved.
    let factory = Factory::developer();
    let (_server, provider) = staged(&factory, Scenario::RateLimited).await;
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a 429 is a failure");
    assert!(matches!(
        error.kind(),
        ProviderErrorKind::RateLimited { .. }
    ));
    assert_eq!(
        error.retry_after(),
        Some(Duration::from_secs(payloads::RETRY_AFTER_SECONDS))
    );
    assert_eq!(error.retry_class(), RetryClass::RetryAfter);

    // The same status with a spent daily quota is not something to wait out:
    // waiting three seconds does not refill a day.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(GENERATE_PATH))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "3")
                .set_body_json(json!({"error": {
                    "code": 429,
                    "message": "You exceeded your current quota. Please check your plan and \
                                billing details.",
                    "status": "RESOURCE_EXHAUSTED",
                    "details": [{
                        "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                        "violations": [{
                            "quotaId": "GenerateRequestsPerDayPerProjectPerModel-FreeTier",
                            "quotaMetric":
                                "generativelanguage.googleapis.com/generate_content_requests"
                        }]
                    }]
                }})),
        )
        .mount(&server)
        .await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let error = provider
        .generate(payloads::narration_request())
        .await
        .expect_err("a spent quota is a failure");

    assert!(matches!(
        error.kind(),
        ProviderErrorKind::QuotaExhausted { .. }
    ));
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("quota_exhausted".to_owned())
    );
    // `Fallback`, not `RetryAfter`: the router moves on instead of sleeping on
    // a `Retry-After` that changes nothing.
    assert_eq!(error.retry_class(), RetryClass::Fallback);
    assert_eq!(error.retry_after(), None);
}

#[tokio::test]
async fn a_safety_block_is_a_content_filter_and_never_a_generic_failure() {
    let factory = Factory::developer();
    let (_server, provider) = staged(&factory, Scenario::Refusal).await;
    let error = provider
        .generate(payloads::structured_request())
        .await
        .expect_err("a blocked prompt has no answer");
    assert!(matches!(error.kind(), ProviderErrorKind::ContentFilter));
    // `Fatal`: retrying elsewhere until a model complies is a safety bypass.
    assert_eq!(error.retry_class(), RetryClass::Fatal);
    assert!(!error.retry_class().allows_another_candidate());

    // A recitation stop is the same outcome, and reaches it the same way.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(GENERATE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"finishReason": "RECITATION", "index": 0}],
            "usageMetadata": {"promptTokenCount": 9, "totalTokenCount": 9},
            "modelVersion": MODEL
        })))
        .mount(&server)
        .await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    let error = provider
        .generate(payloads::structured_request())
        .await
        .expect_err("a recitation stop produced nothing");
    assert!(matches!(error.kind(), ProviderErrorKind::ContentFilter));
    assert_eq!(
        error.code().map(|code| code.as_str().to_owned()),
        Some("RECITATION".to_owned())
    );
}

#[tokio::test]
async fn the_role_vocabulary_and_the_part_shapes_are_what_gemini_expects() {
    let factory = Factory::developer();
    let (server, provider) = staged(&factory, Scenario::ToolCallIds).await;

    let call = ToolCall::new("call_0", payloads::TOOL_NAME, json!({"target": "tok_1"}));
    let request = payloads::tool_request()
        .with_message(Message::new(
            Role::Assistant,
            vec![ContentPart::text("controllo"), ContentPart::ToolCall(call)],
        ))
        .with_message(Message::tool_result(ToolResult::ok(
            "call_0",
            "{\"state\":\"open\"}",
        )));
    provider.generate(request).await.expect("a valid fixture");
    let body = first_body(&server).await;

    let contents = body["contents"].as_array().expect("an array");
    assert_eq!(contents[0]["role"], "user");
    // `assistant` becomes `model`, and the call is a *part* of it.
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[1]["parts"][0]["text"], "controllo");
    assert_eq!(
        contents[1]["parts"][1]["functionCall"]["name"],
        payloads::TOOL_NAME
    );
    // `tool` becomes `user`, and the result is a part addressed by *name*.
    assert_eq!(contents[2]["role"], "user");
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"]["name"],
        payloads::TOOL_NAME
    );
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"]["response"]["state"],
        "open"
    );
    // Neither of the two words the normalized layer uses reaches the wire.
    let rendered = body.to_string();
    assert!(!rendered.contains("\"assistant\""), "{rendered}");
    assert!(!rendered.contains("\"role\":\"tool\""), "{rendered}");
    // And the tools travel in Gemini's one-entry `tools` array.
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        payloads::TOOL_NAME
    );
}

// ---------------------------------------------------------------------------
// The other transport the same endpoint has: a forced function call
// ---------------------------------------------------------------------------

/// A completed answer whose only part is the forced call, carrying `args`.
///
/// This is what the endpoint returns once `functionCallingConfig` is pinned:
/// no text at all, and the document sitting in the call's arguments.
fn function_call_completion(name: &str, args: &Value) -> Value {
    json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{"functionCall": {"name": name, "args": args}}]
            },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": {
            "promptTokenCount": payloads::USAGE_INPUT_TOKENS,
            "candidatesTokenCount": payloads::USAGE_OUTPUT_TOKENS,
            "totalTokenCount": payloads::USAGE_INPUT_TOKENS + payloads::USAGE_OUTPUT_TOKENS,
            "cachedContentTokenCount": payloads::USAGE_CACHED_TOKENS
        },
        "modelVersion": MODEL,
        "responseId": "resp-turnframe-1"
    })
}

/// The streamed twin of [`function_call_completion`]. Gemini sends a function
/// call whole, in one chunk, rather than as argument fragments.
fn function_call_stream(name: &str, args: &Value) -> String {
    sse(&function_call_completion(name, args))
}

/// A server answering both methods with the forced call, and a provider for it.
async fn staged_forced_function(factory: &Factory) -> (MockServer, GeminiProvider) {
    let server = MockServer::start().await;
    let plan = payloads::valid_plan();
    Mock::given(method("POST"))
        .and(path_regex(STREAM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            function_call_stream(payloads::SCHEMA_NAME, &plan),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(GENERATE_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(function_call_completion(payloads::SCHEMA_NAME, &plan)),
        )
        .mount(&server)
        .await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .expect("builds");
    (server, provider)
}

/// The document a response carries, whichever transport delivered it.
fn parse_plan(response: &ModelResponse) -> Value {
    let schema = CompiledSchema::compile(&payloads::plan_schema()).expect("the suite's schema");
    let plan: payloads::ConformancePlan =
        parse_structured(response, &schema).expect("a valid plan");
    serde_json::to_value(plan).expect("serializes")
}

/// Every request body the server saw, in order.
async fn all_bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .expect("recording enabled")
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("a JSON body"))
        .collect()
}

#[tokio::test]
async fn a_forced_function_pins_the_call_to_the_one_function_that_carries_the_schema() {
    let factory = Factory::forced_function();
    let (server, provider) = staged_forced_function(&factory).await;
    let request = payloads::structured_request();

    provider.generate(request.clone()).await.expect("whole");
    let stream = provider.stream(request.clone()).await.expect("stream");
    reconstruct(
        stream,
        StreamAccumulator::new(
            request.request_id,
            provider.provider_key(),
            provider.model_key(),
        ),
    )
    .await
    .expect("the streamed answer reassembles");

    let bodies = all_bodies(&server).await;
    assert_eq!(bodies.len(), 2, "one whole call and one streamed call");
    for body in &bodies {
        // One declaration, and it is the one carrying the caller's schema —
        // translated into Gemini's dialect, not copied.
        let declarations = body["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("function declarations");
        assert_eq!(declarations.len(), 1, "{body}");
        assert_eq!(declarations[0]["name"], payloads::SCHEMA_NAME);
        assert_eq!(declarations[0]["parameters"]["type"], "OBJECT");
        assert_eq!(
            declarations[0]["parameters"]["properties"]["acts"]["type"],
            "ARRAY"
        );
        assert!(
            body.to_string().contains(payloads::SCHEMA_MARKER),
            "the schema itself must travel as the function's parameters: {body}"
        );

        // The mode is pinned to that single function: `ANY` alone would let the
        // model satisfy the constraint with some other call.
        let config = &body["toolConfig"]["functionCallingConfig"];
        assert_eq!(config["mode"], "ANY", "{body}");
        assert_eq!(
            config["allowedFunctionNames"],
            json!([payloads::SCHEMA_NAME]),
            "{body}"
        );

        // And the response-schema transport is not also sent: one transport per
        // request, chosen by the declaration.
        assert!(
            body["generationConfig"].get("responseSchema").is_none(),
            "{body}"
        );
        assert!(
            body["generationConfig"].get("responseMimeType").is_none(),
            "{body}"
        );
    }
}

#[tokio::test]
async fn the_forced_function_yields_what_the_schema_transport_would_have() {
    // The point of offering both: a caller cannot tell which one served it, so
    // a router may move a stage between this adapter and one whose wire format
    // has only the function form.
    let (_forced, forced_provider) = staged_forced_function(&Factory::forced_function()).await;
    let request = payloads::structured_request();
    let through_function = forced_provider
        .generate(request.clone())
        .await
        .expect("whole");
    let stream = forced_provider
        .stream(request.clone())
        .await
        .expect("stream");
    let streamed_function = reconstruct(
        stream,
        StreamAccumulator::new(
            request.request_id,
            forced_provider.provider_key(),
            forced_provider.model_key(),
        ),
    )
    .await
    .expect("reassembles");

    let (_schema, schema_provider) = staged(&Factory::developer(), Scenario::ValidStructured).await;
    let through_schema = schema_provider
        .generate(payloads::structured_request())
        .await
        .expect("whole");

    assert_eq!(parse_plan(&through_function), parse_plan(&through_schema));
    assert_eq!(parse_plan(&streamed_function), parse_plan(&through_schema));
    assert_eq!(parse_plan(&through_function), payloads::valid_plan());
    // Usage is reported the same way whichever transport carried the document.
    assert_eq!(through_function.usage, through_schema.usage);
}

#[tokio::test]
async fn the_function_transport_cannot_be_declared_without_tool_calling() {
    // It *is* a function call, so a profile that claims it while denying tool
    // calling is describing two different models.
    let error = GeminiProvider::gemini()
        .api_key(ApiKey::new(payloads::DUMMY_API_KEY))
        .model(MODEL)
        .capabilities(
            ProviderCapabilities::minimal()
                .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
                .with_tool_calling(ToolCallingCapability::None),
        )
        .build()
        .expect_err("a forced function needs tool calling");
    assert!(
        matches!(error, ConfigError::TransportNeedsToolCalling),
        "{error}"
    );
}
