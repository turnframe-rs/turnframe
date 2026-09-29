//! The feature checks of the suite: the seventeen of spec §20.8, plus the two
//! streaming rows and the usage-contract row that reassembly alone cannot
//! prove.
//!
//! Each one mounts a scenario, drives the adapter and returns a verdict.
//! Nothing here panics or unwraps: a check that cannot decide reports a
//! failure with a reason, so a broken adapter produces a full report rather
//! than a stack trace.

use std::time::Duration;

use wiremock::MockServer;

use super::payloads;
use super::report::{Check, CheckResult, CheckStatus};
use super::{ProviderFactory, Scenario, WireFixtures};
use crate::capabilities::ProviderCapabilities;
use crate::error::{ProviderError, ProviderErrorKind, RetryClass};
use crate::provider::ModelProvider;
use crate::purpose::ModelPurpose;
use crate::response::{FinishReason, ModelResponse};
use crate::secret::{ApiKey, DefaultRedactor, Redactor};
use crate::stream::{StreamAccumulator, StreamEvent};
use crate::structured::{CompiledSchema, StructuredOutputError, parse_structured};

/// What a check produces internally: `Ok(())` passes, `Err(reason)` fails,
/// `Ok` with a skip reason is expressed by returning [`Outcome::Skipped`].
pub(super) enum Outcome {
    Passed,
    Failed(String),
    Skipped(String),
}

impl Outcome {
    pub(super) fn into_result(self, check: Check) -> CheckResult {
        match self {
            Self::Passed => CheckResult::passed(check),
            Self::Failed(detail) => CheckResult::failed(check, detail),
            Self::Skipped(reason) => CheckResult::skipped(check, reason),
        }
    }

    fn failed(detail: impl Into<String>) -> Self {
        Self::Failed(detail.into())
    }
}

/// Everything a check needs, threaded through the run.
pub(super) struct Context {
    /// Error families observed by the failure checks, for
    /// [`Check::RetryClassification`].
    pub(super) observed: Vec<(Check, ProviderErrorKind)>,
    /// The capabilities the adapter declares.
    pub(super) capabilities: ProviderCapabilities,
}

impl Context {
    pub(super) fn new(capabilities: ProviderCapabilities) -> Self {
        Self {
            observed: Vec::new(),
            capabilities,
        }
    }
}

/// Compiles the suite's schema, or explains why it could not.
fn schema() -> Result<CompiledSchema, String> {
    CompiledSchema::compile(&payloads::plan_schema())
        .map_err(|error| format!("the suite's own schema failed to compile: {error}"))
}

/// Starts a server with the fixture for `scenario` mounted, and builds the
/// adapter against it.
pub(super) async fn stage<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    scenario: Scenario,
) -> Result<(MockServer, F::Provider), String> {
    let server = MockServer::start().await;
    fixtures.mount(&server, scenario).await;
    let provider = factory
        .build(&server.uri(), ApiKey::new(payloads::DUMMY_API_KEY))
        .map_err(|error| format!("adapter could not be built: {error}"))?;
    Ok((server, provider))
}

/// Runs `scenario` and reports whichever of the two acceptable shapes came
/// back: a typed failure, or a response that does not parse into a plan.
async fn expect_rejected<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    scenario: Scenario,
    expected: &[&str],
) -> Outcome {
    let schema = match schema() {
        Ok(schema) => schema,
        Err(reason) => return Outcome::failed(reason),
    };
    let (_server, provider) = match stage(factory, fixtures, scenario).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    match provider.generate(payloads::structured_request()).await {
        Err(error) => {
            // A typed transport failure is an acceptable answer here.
            let _ = error;
            Outcome::Passed
        }
        Ok(response) => match parse_structured::<payloads::ConformancePlan>(&response, &schema) {
            Ok(plan) => Outcome::failed(format!(
                "a rejected payload parsed into {} act(s); a valid subset must never be accepted",
                plan.acts.len()
            )),
            Err(error) => {
                if expected.is_empty() || expected.contains(&error.as_str()) {
                    Outcome::Passed
                } else {
                    Outcome::failed(format!(
                        "rejected as {}, expected one of {}",
                        error.as_str(),
                        expected.join("|")
                    ))
                }
            }
        },
    }
}

/// Runs `scenario` and checks the adapter mapped it onto one of `expected`.
async fn expect_error_kind<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    scenario: Scenario,
    check: Check,
    expected: &[&str],
    context: &mut Context,
    request: crate::request::ModelRequest,
) -> Outcome {
    let (_server, provider) = match stage(factory, fixtures, scenario).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    match provider.generate(request).await {
        Ok(response) => Outcome::failed(format!(
            "expected {}, got a response finishing as {}",
            expected.join("|"),
            response.finish
        )),
        Err(error) => {
            let observed = error.kind().as_str();
            context.observed.push((check, error.kind().clone()));
            if expected.contains(&observed) {
                Outcome::Passed
            } else {
                Outcome::failed(format!(
                    "mapped to {observed}, expected one of {}",
                    expected.join("|")
                ))
            }
        }
    }
}

/// A schema-conformant body becomes a normalized response with every field
/// intact.
pub(super) async fn valid_structured_response<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Outcome {
    let schema = match schema() {
        Ok(schema) => schema,
        Err(reason) => return Outcome::failed(reason),
    };
    let (_server, provider) = match stage(factory, fixtures, Scenario::ValidStructured).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    let request = payloads::structured_request();
    let request_id = request.request_id;
    let response = match provider.generate(request).await {
        Ok(response) => response,
        Err(error) => return Outcome::failed(format!("a valid body failed: {error}")),
    };
    if response.request_id != request_id {
        return Outcome::failed("the response does not carry the request id it answers");
    }
    if response.provider != provider.provider_key() {
        return Outcome::failed("the response is not labelled with the provider key");
    }
    match parse_structured::<payloads::ConformancePlan>(&response, &schema) {
        Ok(plan) => {
            let expected = payloads::valid_plan();
            match serde_json::to_value(&plan) {
                Ok(actual) if actual == expected => Outcome::Passed,
                Ok(_) => Outcome::failed("the parsed plan does not equal the fixture's plan"),
                Err(error) => {
                    Outcome::failed(format!("the parsed plan is not serializable: {error}"))
                }
            }
        }
        Err(error) => Outcome::failed(format!("a valid body did not parse: {error}")),
    }
}

/// A body that is not JSON is a typed failure, never a partial parse.
pub(super) async fn malformed_json<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Outcome {
    expect_rejected(
        factory,
        fixtures,
        Scenario::MalformedJson,
        &["not_json", "no_output"],
    )
    .await
}

/// A field the schema forbids rejects the whole response, both acts included.
pub(super) async fn unknown_fields<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Outcome {
    expect_rejected(
        factory,
        fixtures,
        Scenario::UnknownField,
        &["unknown_field"],
    )
    .await
}

/// A missing required field rejects the whole response; no default is invented.
pub(super) async fn missing_required_fields<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Outcome {
    expect_rejected(
        factory,
        fixtures,
        Scenario::MissingField,
        &["missing_field"],
    )
    .await
}

/// A multi-act plan round-trips in order, as one proposal.
pub(super) async fn multiple_acts<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Outcome {
    let schema = match schema() {
        Ok(schema) => schema,
        Err(reason) => return Outcome::failed(reason),
    };
    let (_server, provider) = match stage(factory, fixtures, Scenario::MultipleActs).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    let response = match provider.generate(payloads::structured_request()).await {
        Ok(response) => response,
        Err(error) => return Outcome::failed(format!("a two-act body failed: {error}")),
    };
    match parse_structured::<payloads::ConformancePlan>(&response, &schema) {
        Ok(plan) if plan.acts.len() == 2 => {
            if plan.acts[0].operation == "set_travel_date" && plan.acts[1].operation == "set_amount"
            {
                Outcome::Passed
            } else {
                Outcome::failed("the two acts came back out of order")
            }
        }
        Ok(plan) => Outcome::failed(format!("expected 2 acts, parsed {}", plan.acts.len())),
        Err(error) => Outcome::failed(format!("a two-act body did not parse: {error}")),
    }
}

/// Ids survive, and the `preserves_call_ids` declaration matches reality.
pub(super) async fn tool_and_read_request_ids<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &Context,
) -> Outcome {
    if !context.capabilities.supports_tools() {
        return Outcome::Skipped("the profile declares no tool calling".to_owned());
    }
    let (_server, provider) = match stage(factory, fixtures, Scenario::ToolCallIds).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    let response = match provider.generate(payloads::tool_request()).await {
        Ok(response) => response,
        Err(error) => return Outcome::failed(format!("a tool-call body failed: {error}")),
    };
    let calls = response.tool_calls();
    let Some(call) = calls.first() else {
        return Outcome::failed("a tool-call body produced no tool call");
    };
    if call.name != payloads::TOOL_NAME {
        return Outcome::failed(format!(
            "the tool name came back as {:?}, expected {:?}",
            call.name,
            payloads::TOOL_NAME
        ));
    }
    if context.capabilities.preserves_call_ids {
        if call.id.as_str() == payloads::EXPECTED_CALL_ID {
            Outcome::Passed
        } else {
            Outcome::failed(
                "the profile declares preserves_call_ids but the provider's id did not survive; \
                 lower the declaration rather than the test",
            )
        }
    } else if call.id.is_empty() {
        Outcome::failed("a synthesized call id must still be non-empty")
    } else {
        Outcome::Passed
    }
}

/// The refusal a streaming row returns when the profile declares no streaming.
///
/// It is a failure and not a skip on purpose. Before this, a profile could make
/// three rows disappear by declaring `streaming: false`, and a compatibility
/// table then showed a blank where an unimplemented feature sat. Saying so in
/// words costs a line and cannot be done by accident.
fn streaming_undeclared(check: Check) -> Outcome {
    Outcome::failed(format!(
        "the profile declares no streaming, so {check} could not be exercised; \
         a deployment that genuinely has no streaming endpoint declares this row \
         not producible, with a reason, through WireFixtures::feature_support — \
         an unexplained blank in a compatibility table is indistinguishable from \
         an unimplemented feature"
    ))
}

/// Runs the streaming scenario both ways, for the three rows that need both.
struct BothPaths {
    whole: ModelResponse,
    rebuilt: ModelResponse,
    deltas: Vec<String>,
}

/// Drives one exchange whole and streamed, or explains what went wrong.
async fn both_paths<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Result<BothPaths, String> {
    let (_server, provider) = stage(factory, fixtures, Scenario::StreamingReconstruction).await?;
    let request = payloads::narration_request();
    let whole = provider
        .generate(request.clone())
        .await
        .map_err(|error| format!("the non-streamed call failed: {error}"))?;
    let stream = provider
        .stream(request.clone())
        .await
        .map_err(|error| format!("the profile declares streaming but stream() failed: {error}"))?;

    let mut seed = StreamAccumulator::new(
        request.request_id,
        provider.provider_key(),
        provider.model_key(),
    );
    let mut deltas = Vec::new();
    let items = stream.collect_items().await;
    for item in items {
        let event = item.map_err(|error| format!("the stream failed: {error}"))?;
        if let StreamEvent::TextDelta { text } = &event {
            deltas.push(text.clone());
        }
        seed.push(event)
            .map_err(|error| format!("the stream did not reassemble: {error}"))?;
    }
    let rebuilt = seed
        .finish()
        .map_err(|error| format!("the stream did not reassemble: {error}"))?;
    Ok(BothPaths {
        whole,
        rebuilt,
        deltas,
    })
}

/// The reassembled stream equals the non-streamed answer.
pub(super) async fn streaming_reconstruction<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &Context,
) -> Outcome {
    if !context.capabilities.streaming {
        return streaming_undeclared(Check::StreamingReconstruction);
    }
    let paths = match both_paths(factory, fixtures).await {
        Ok(paths) => paths,
        Err(reason) => return Outcome::failed(reason),
    };
    // Warnings and latency legitimately differ between the two paths; the
    // answer itself must not.
    if paths.rebuilt.content != paths.whole.content {
        return Outcome::failed("the reassembled content differs from the non-streamed content");
    }
    if paths.rebuilt.finish != paths.whole.finish {
        return Outcome::failed(format!(
            "the reassembled finish reason is {}, the non-streamed one is {}",
            paths.rebuilt.finish, paths.whole.finish
        ));
    }
    Outcome::Passed
}

/// An answer that arrives as several wire events produces several deltas.
///
/// [`Scenario::StreamingReconstruction`] requires the streamed half to deliver
/// the answer in at least two events, so an adapter that reads the whole body
/// and emits one delta at the end is visible here and nowhere else: it
/// reassembles perfectly, and it gives an adopter a spinner.
pub(super) async fn streaming_incremental<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &Context,
) -> Outcome {
    if !context.capabilities.streaming {
        return streaming_undeclared(Check::StreamingIncremental);
    }
    let paths = match both_paths(factory, fixtures).await {
        Ok(paths) => paths,
        Err(reason) => return Outcome::failed(reason),
    };
    let whole_text = paths.whole.text();
    if whole_text.trim().is_empty() {
        return Outcome::failed(
            "the streaming fixture answered with no prose at all, so incremental delivery \
             cannot be observed; answer it with the same text on both paths",
        );
    }
    match paths.deltas.len() {
        0 => Outcome::failed(
            "the stream carried the answer without a single text delta, so nothing could \
             have been shown while it was written",
        ),
        1 => Outcome::failed(format!(
            "the fixture sent the answer in several wire events and the stream produced \
             one delta of {} characters: the answer was buffered and released at the end, \
             which reassembles correctly and shows the reader nothing",
            paths.deltas[0].chars().count()
        )),
        _ => {
            let joined: String = paths.deltas.concat();
            if joined == whole_text {
                Outcome::Passed
            } else {
                Outcome::failed(
                    "the concatenated deltas do not equal the non-streamed text, so the \
                     pieces are not the same answer",
                )
            }
        }
    }
}

/// The two paths report the same token usage for the same answer.
///
/// Usage rides on a final frame in most vendors — an OpenAI chunk after the
/// finish, a Converse `metadata` event, an Ollama `done` frame — so it is the
/// first thing a streaming implementation drops, and the loss is invisible
/// until someone compares two bills.
pub(super) async fn streaming_usage_agreement<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &Context,
) -> Outcome {
    if !context.capabilities.streaming {
        return streaming_undeclared(Check::StreamingUsageAgreement);
    }
    let paths = match both_paths(factory, fixtures).await {
        Ok(paths) => paths,
        Err(reason) => return Outcome::failed(reason),
    };
    if paths.whole.usage.is_unreported() {
        return Outcome::failed(
            "the non-streamed call reported no usage at all, so the two paths cannot be \
             compared; the streaming fixture must report counts on both halves",
        );
    }
    if paths.rebuilt.usage == paths.whole.usage {
        Outcome::Passed
    } else {
        Outcome::failed(format!(
            "the streamed path reported input={} output={} cached={} and the whole path \
             reported input={} output={} cached={} for the same answer",
            paths.rebuilt.usage.input,
            paths.rebuilt.usage.output,
            paths.rebuilt.usage.cached_input,
            paths.whole.usage.input,
            paths.whole.usage.output,
            paths.whole.usage.cached_input,
        ))
    }
}

/// The reported usage keeps the contract the whole system reads it by.
///
/// `input` is the entire prompt and `cached_input` is the slice of it the
/// provider served from its cache — not a second figure standing beside it. The
/// fixture reports a prompt of [`payloads::USAGE_INPUT_TOKENS`] tokens of which
/// [`payloads::USAGE_CACHED_TOKENS`] were cached, and the cached majority is
/// deliberate: an adapter reporting the net figure would report an input
/// smaller than the cache it claims is inside it, which is the failure this row
/// exists to name.
pub(super) async fn token_usage_contract<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &Context,
) -> Outcome {
    let (_server, provider) = match stage(factory, fixtures, Scenario::CachedUsage).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    let response = match provider.generate(payloads::narration_request()).await {
        Ok(response) => response,
        Err(error) => return Outcome::failed(format!("the cached-usage fixture failed: {error}")),
    };
    let usage = response.usage;
    if usage.cached_input > usage.input {
        return Outcome::failed(format!(
            "reported cached_input={} over input={}: input is the whole prompt and \
             cached_input is a subset of it, so a cache larger than the prompt means the \
             net figure was reported instead of the gross one",
            usage.cached_input, usage.input
        ));
    }
    if usage.input != payloads::USAGE_INPUT_TOKENS {
        return Outcome::failed(format!(
            "the fixture reported a prompt of {} tokens and the adapter reported input={}",
            payloads::USAGE_INPUT_TOKENS,
            usage.input
        ));
    }
    if context.capabilities.prompt_caching && usage.cached_input != payloads::USAGE_CACHED_TOKENS {
        return Outcome::failed(format!(
            "the profile declares prompt caching and the fixture reported {} cached prompt \
             tokens, but the adapter reported cached_input={}",
            payloads::USAGE_CACHED_TOKENS,
            usage.cached_input
        ));
    }
    Outcome::Passed
}

/// An empty body is a typed failure or an empty response, never an empty plan.
pub(super) async fn empty_output<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Outcome {
    let schema = match schema() {
        Ok(schema) => schema,
        Err(reason) => return Outcome::failed(reason),
    };
    let (_server, provider) = match stage(factory, fixtures, Scenario::EmptyOutput).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    match provider.generate(payloads::structured_request()).await {
        Err(_) => Outcome::Passed,
        Ok(response) => match parse_structured::<payloads::ConformancePlan>(&response, &schema) {
            Ok(_) => Outcome::failed("an empty body produced a plan"),
            Err(StructuredOutputError::NoOutput | StructuredOutputError::NotJson { .. }) => {
                Outcome::Passed
            }
            Err(other) => Outcome::failed(format!(
                "an empty body was rejected as {}, expected no_output",
                other.as_str()
            )),
        },
    }
}

/// A refusal is its own outcome, distinguishable from malformed output.
pub(super) async fn refusal<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &mut Context,
) -> Outcome {
    let (_server, provider) = match stage(factory, fixtures, Scenario::Refusal).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    match provider.generate(payloads::structured_request()).await {
        Err(error) => {
            let kind = error.kind().clone();
            let matched = matches!(
                kind,
                ProviderErrorKind::Refusal | ProviderErrorKind::ContentFilter
            );
            context.observed.push((Check::Refusal, kind));
            if matched {
                Outcome::Passed
            } else {
                Outcome::failed("a refusal was mapped to a transport failure")
            }
        }
        Ok(response) => {
            if response.finish == FinishReason::Refusal {
                match response.single_json() {
                    Err(StructuredOutputError::Refusal) => Outcome::Passed,
                    _ => Outcome::failed("a refusal did not surface as a refusal when parsed"),
                }
            } else {
                Outcome::failed(format!(
                    "a refusal came back finishing as {}, not as a refusal",
                    response.finish
                ))
            }
        }
    }
}

/// A deadline that passes produces a timeout, not a transport error.
pub(super) async fn timeout<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &mut Context,
) -> Outcome {
    let request = payloads::narration_request().with_timeout(payloads::SHORT_TIMEOUT);
    expect_error_kind(
        factory,
        fixtures,
        Scenario::SlowResponse,
        Check::Timeout,
        &["timeout"],
        context,
        request,
    )
    .await
}

/// A rate limit maps to its variant and keeps the `Retry-After` hint.
pub(super) async fn rate_limit<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &mut Context,
) -> Outcome {
    let (_server, provider) = match stage(factory, fixtures, Scenario::RateLimited).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    match provider.generate(payloads::narration_request()).await {
        Ok(_) => Outcome::failed("a 429 produced a response"),
        Err(error) => {
            let kind = error.kind().clone();
            context.observed.push((Check::RateLimit, kind.clone()));
            match kind {
                ProviderErrorKind::RateLimited { retry_after } => match retry_after {
                    Some(delay) if delay == Duration::from_secs(payloads::RETRY_AFTER_SECONDS) => {
                        Outcome::Passed
                    }
                    Some(delay) => Outcome::failed(format!(
                        "Retry-After came back as {}s, the fixture sent {}s",
                        delay.as_secs(),
                        payloads::RETRY_AFTER_SECONDS
                    )),
                    None => Outcome::failed("the Retry-After hint was dropped"),
                },
                other => Outcome::failed(format!("a 429 mapped to {}", other.as_str())),
            }
        }
    }
}

/// Rejected credentials are non-retryable, and the key stays out of the error.
pub(super) async fn authentication_failure<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &mut Context,
) -> Outcome {
    let (_server, provider) = match stage(factory, fixtures, Scenario::Authentication).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    match provider.generate(payloads::narration_request()).await {
        Ok(_) => Outcome::failed("a 401 produced a response"),
        Err(error) => {
            let kind = error.kind().clone();
            context
                .observed
                .push((Check::AuthenticationFailure, kind.clone()));
            // Any of the three credential families is a correct answer here.
            // The row is about a rejected credential reaching the caller as a
            // credential problem rather than as a network hiccup; *which* of
            // the three it is is decided by the body, and the dedicated
            // [`StatusRow::ExpiredCredential`](crate::conformance::StatusRow::ExpiredCredential)
            // row is what proves the adapter tells them apart. Insisting on
            // `authentication` here would fail an adapter whose vendor words
            // its 401 as an expiry — a mapping that is right, on a row that was
            // never about the distinction.
            if !matches!(
                kind,
                ProviderErrorKind::Authentication
                    | ProviderErrorKind::Authorization
                    | ProviderErrorKind::CredentialExpired
            ) {
                return Outcome::failed(format!("a 401 mapped to {}", kind.as_str()));
            }
            if let Some(leak) = find_secret(&[error.to_string(), format!("{error:?}")]) {
                return Outcome::failed(leak);
            }
            Outcome::Passed
        }
    }
}

/// A context overflow gets its own variant.
pub(super) async fn context_overflow<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &mut Context,
) -> Outcome {
    expect_error_kind(
        factory,
        fixtures,
        Scenario::ContextOverflow,
        Check::ContextOverflow,
        &["context_overflow"],
        context,
        payloads::structured_request(),
    )
    .await
}

/// Dropping the future aborts the call cleanly.
pub(super) async fn cancellation<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Outcome {
    let (_server, provider) = match stage(factory, fixtures, Scenario::SlowResponse).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    // A future dropped before it resolves *is* cancellation in Rust. The check
    // is that the drop happens promptly and leaves nothing behind: a call that
    // ignores its deadline and finishes anyway is a call the runtime cannot
    // abandon.
    let request = payloads::narration_request().with_timeout(payloads::SLOW_RESPONSE_DELAY * 2);
    let cancelled =
        tokio::time::timeout(Duration::from_millis(120), provider.generate(request)).await;
    match cancelled {
        Err(_elapsed) => Outcome::Passed,
        Ok(Ok(_)) => Outcome::failed("the slow fixture answered before the cancellation deadline"),
        Ok(Err(error)) => {
            if matches!(error.kind(), ProviderErrorKind::Cancelled) {
                Outcome::Passed
            } else {
                Outcome::failed(format!(
                    "the call ended as {} before it could be cancelled",
                    error.kind().as_str()
                ))
            }
        }
    }
}

/// Every observed failure carries the class the policy layer expects.
pub(super) fn retry_classification(context: &Context) -> Outcome {
    if context.observed.is_empty() {
        return Outcome::Skipped("no failure check produced an error to classify".to_owned());
    }
    let mut wrong = Vec::new();
    for (check, kind) in &context.observed {
        let expected = match kind {
            ProviderErrorKind::Timeout
            | ProviderErrorKind::Transport
            | ProviderErrorKind::Malformed
            | ProviderErrorKind::Server { .. } => RetryClass::Retry,
            ProviderErrorKind::RateLimited { .. } => RetryClass::RetryAfter,
            ProviderErrorKind::Authentication
            | ProviderErrorKind::Authorization
            // An expired credential is never a plain retry: the same token
            // would fail again. A caller holding a refresher may refresh and
            // try the same profile; one without a refresher moves on.
            | ProviderErrorKind::CredentialExpired
            // A spent quota is never a rate limit: waiting does not refill it,
            // so the router moves to another candidate instead of sleeping.
            | ProviderErrorKind::QuotaExhausted { .. }
            | ProviderErrorKind::ModelNotFound
            | ProviderErrorKind::CapabilityMismatch { .. }
            | ProviderErrorKind::Unsupported { .. } => RetryClass::Fallback,
            _ => RetryClass::Fatal,
        };
        let actual = kind.retry_class();
        if actual != expected {
            wrong.push(format!(
                "{check}: {} classified as {actual}, expected {expected}",
                kind.as_str()
            ));
        }
    }
    if wrong.is_empty() {
        Outcome::Passed
    } else {
        Outcome::failed(wrong.join("; "))
    }
}

/// The configured credential appears nowhere the adapter can be observed.
pub(super) async fn secret_redaction<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
) -> Outcome {
    let (_server, provider) = match stage(factory, fixtures, Scenario::SecretInBody).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    let mut renderings = vec![format!("{provider:?}")];
    match provider.generate(payloads::structured_request()).await {
        Ok(response) => {
            renderings.push(format!("{response:?}"));
            renderings.push(render_response_text(&response));
        }
        Err(error) => {
            renderings.push(error.to_string());
            renderings.push(format!("{error:?}"));
        }
    }
    if let Some(leak) = find_secret(&renderings) {
        return Outcome::failed(leak);
    }
    // The redactor must also erase it from anything an adapter does log.
    let redactor = DefaultRedactor::new().with_secret(&ApiKey::new(payloads::DUMMY_API_KEY));
    let line = format!("authorization: Bearer {}", payloads::DUMMY_API_KEY);
    if redactor.redact(&line).contains(payloads::DUMMY_API_KEY) {
        return Outcome::failed("the default redactor did not mask the configured key");
    }
    Outcome::Passed
}

/// A profile that declares schema enforcement actually sends the schema.
pub(super) async fn no_silent_capability_downgrade<F: ProviderFactory, W: WireFixtures>(
    factory: &F,
    fixtures: &W,
    context: &Context,
) -> Outcome {
    // Whatever the transport, a profile must refuse a stage it cannot serve.
    let mutation = ModelPurpose::Extract.requirements();
    let declared_fit = mutation.satisfied_by(&context.capabilities).is_ok();
    if declared_fit != context.capabilities.structured_output.enforces_schema() {
        return Outcome::failed(
            "the declared structured-output transport disagrees with the understanding \
             requirement it is admitted by",
        );
    }
    if !context.capabilities.structured_output.enforces_schema() {
        return Outcome::Skipped(format!(
            "the profile declares {}, which is not admitted for the understanding tasks",
            context.capabilities.structured_output
        ));
    }

    let (server, provider) = match stage(factory, fixtures, Scenario::ValidStructured).await {
        Ok(staged) => staged,
        Err(reason) => return Outcome::failed(reason),
    };
    if provider
        .generate(payloads::structured_request())
        .await
        .is_err()
    {
        return Outcome::failed(
            "a valid structured call failed, so the request could not be inspected",
        );
    }
    let Some(requests) = server.received_requests().await else {
        return Outcome::failed("the mock server recorded no requests");
    };
    let carried = requests
        .iter()
        .any(|request| String::from_utf8_lossy(&request.body).contains(payloads::SCHEMA_MARKER));
    if carried {
        Outcome::Passed
    } else {
        Outcome::failed(format!(
            "the profile declares {} but the request carried no schema; \
             lower the declaration rather than the test",
            context.capabilities.structured_output
        ))
    }
}

/// Concatenates a response's text and tool arguments, for leak hunting.
fn render_response_text(response: &ModelResponse) -> String {
    let mut out = response.text();
    for call in response.tool_calls() {
        out.push_str(&call.arguments.to_string());
    }
    if let Some(raw_id) = &response.raw_id {
        out.push_str(raw_id);
    }
    out
}

/// Reports the first rendering that contains the planted key.
fn find_secret(renderings: &[String]) -> Option<String> {
    // A prefix is enough: a truncated key is still a leak.
    let needle = &payloads::DUMMY_API_KEY[..payloads::DUMMY_API_KEY.len().min(20)];
    renderings
        .iter()
        .position(|rendering| rendering.contains(needle))
        .map(|index| format!("the configured credential appears in rendering #{index}"))
}

/// Builds the failure the run reports when the adapter cannot even be created.
pub(super) fn build_failure(check: Check, error: &ProviderError) -> CheckResult {
    CheckResult {
        check,
        status: CheckStatus::Failed,
        detail: Some(format!("adapter could not be built: {error}")),
    }
}
