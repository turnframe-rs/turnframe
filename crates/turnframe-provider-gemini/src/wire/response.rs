//! `generateContent` body → normalized response.
//!
//! # Why nothing here is `deny_unknown_fields`
//!
//! The workspace rule is that structures parsed from a client are strict. This
//! module is the documented exception, and the reason is structural: Google
//! adds fields to `GenerateContentResponse` continuously — `modelVersion`,
//! `responseId`, `avgLogprobs`, `groundingMetadata`, `urlContextMetadata`,
//! `citationMetadata` — and a strict envelope would start rejecting perfectly
//! good answers the week the next one ships.
//!
//! The strictness invariant I18 actually needs is applied one layer up, to the
//! *model's own payload*, by
//! [`parse_structured`](turnframe_provider::structured::parse_structured): the
//! plan is validated against its schema with deny-unknown semantics, and one
//! malformed act rejects the whole document. Tolerating an unknown envelope
//! field cannot smuggle an act past that gate.
//!
//! # A blocked answer is a content filter, never a generic failure
//!
//! Gemini reports safety two ways, and both land on
//! [`ContentFilter`](turnframe_provider::error::ProviderErrorKind::ContentFilter):
//!
//! * `promptFeedback.blockReason` — the *prompt* was refused, so there is no
//!   candidate at all;
//! * a candidate finishing as `SAFETY`, `RECITATION`, `BLOCKLIST`,
//!   `PROHIBITED_CONTENT` or `SPII`.
//!
//! Which of the two shapes it takes depends on whether anything came back. A
//! blocked candidate carrying **no content** is a failure, because there is
//! nothing to hand the caller; a blocked candidate carrying a **prefix** is a
//! response finishing as `ContentFilter`, which
//! [`FinishReason::is_complete`] already refuses to let a structured stage
//! parse. Either way the outcome is `Fatal`: retrying elsewhere until a model
//! complies is a safety bypass, not a recovery.
//!
//! # Usage, and the two counters that need arithmetic
//!
//! Gemini's `promptTokenCount` **includes** `cachedContentTokenCount`, which is
//! exactly what [`TokenUsage`] documents (`cached_input` is "already counted in
//! `input`"), so the two map across with no adjustment. `thoughtsTokenCount` is
//! the one that does need it: it sits *outside* `candidatesTokenCount` in the
//! response and *inside* the output tokens on the bill, so the normalized
//! `output` is the sum and `reasoning` is the thinking half of it. A cost
//! estimate built on `candidatesTokenCount` alone would under-report every
//! reasoning call.

use serde::Deserialize;
use serde_json::Value;
use turnframe_provider::error::{ErrorCode, ProviderError};
use turnframe_provider::ids::{CallId, ModelKey, ProviderKey, RequestId};
use turnframe_provider::request::{ContentPart, ToolCall};
use turnframe_provider::response::{FinishReason, ModelResponse, ResponseWarning, TokenUsage};

/// The `generateContent` response envelope.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerateContentResponse {
    #[serde(default)]
    pub(crate) candidates: Vec<Candidate>,
    #[serde(default)]
    pub(crate) prompt_feedback: Option<PromptFeedback>,
    #[serde(default)]
    pub(crate) usage_metadata: Option<UsageMetadata>,
    #[serde(default)]
    pub(crate) model_version: Option<String>,
    #[serde(default)]
    pub(crate) response_id: Option<String>,
}

/// One candidate answer.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Candidate {
    /// Absent when the candidate was blocked before producing anything.
    #[serde(default)]
    pub(crate) content: Option<Content>,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

/// The parts of a candidate.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Content {
    #[serde(default)]
    pub(crate) parts: Vec<Part>,
}

/// One part of a candidate's content.
///
/// Gemini models this as a `oneof`, so at most one field is set; unknown part
/// kinds decode into an all-`None` struct and are ignored rather than
/// misread.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Part {
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) function_call: Option<FunctionCall>,
    /// `true` on a part that is the model's own reasoning rather than its
    /// answer. Never concatenated into the reply.
    #[serde(default)]
    pub(crate) thought: bool,
}

/// A call the model made.
///
/// The REST surface usually omits `id`: a `functionCall` carries a name and
/// arguments and nothing to correlate on. That is why the default profile
/// declares `preserves_call_ids: false`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FunctionCall {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) args: Option<Value>,
}

/// What Gemini says about the prompt itself.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptFeedback {
    /// `"SAFETY"`, `"OTHER"`, `"BLOCKLIST"`, `"PROHIBITED_CONTENT"`, …
    #[serde(default)]
    pub(crate) block_reason: Option<String>,
}

/// Reported token usage.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageMetadata {
    /// Prompt tokens, **including** the cached ones.
    #[serde(default)]
    pub(crate) prompt_token_count: u64,
    #[serde(default)]
    pub(crate) candidates_token_count: u64,
    #[serde(default)]
    pub(crate) cached_content_token_count: u64,
    /// Thinking tokens. Billed as output, reported separately.
    #[serde(default)]
    pub(crate) thoughts_token_count: u64,
}

impl UsageMetadata {
    /// Normalizes into [`TokenUsage`].
    ///
    /// `input` keeps Gemini's own `promptTokenCount`, which already contains
    /// the cached tokens, so `cached_input` is the "already counted in `input`"
    /// figure the provider crate documents rather than a second charge. A cache
    /// count larger than the prompt it is part of is nonsense, so it is clamped
    /// rather than propagated.
    pub(crate) fn normalize(&self) -> TokenUsage {
        let output = self
            .candidates_token_count
            .saturating_add(self.thoughts_token_count);
        TokenUsage::new(self.prompt_token_count, output)
            .with_cached_input(self.cached_content_token_count.min(self.prompt_token_count))
            .with_reasoning(self.thoughts_token_count)
    }
}

/// Finish reasons that mean a safety system stopped the generation.
///
/// `RECITATION` is here because it is one: Gemini stops when the output starts
/// reproducing memorized material, and the answer is as unusable as a blocked
/// one. Reading it as a generic failure would make it retryable, and a
/// retryable safety stop is a safety bypass.
const BLOCKING_FINISH_REASONS: &[&str] = &[
    "SAFETY",
    "RECITATION",
    "BLOCKLIST",
    "PROHIBITED_CONTENT",
    "SPII",
    "IMAGE_SAFETY",
];

/// Maps a Gemini `finishReason` onto the normalized one.
///
/// An unrecognized label becomes [`FinishReason::Other`] and a
/// [`ResponseWarning::UnknownFinishReason`]: a stage that needs a complete
/// answer then refuses it, which is the safe reading of "the service stopped
/// for a reason we do not model". A **missing** label on a candidate that
/// produced content is [`FinishReason::Stop`], because that is what a streamed
/// chunk before the last one looks like.
pub(crate) fn finish_reason(
    reported: Option<&str>,
    warnings: &mut Vec<ResponseWarning>,
) -> FinishReason {
    let reported = reported.map(str::trim).filter(|label| !label.is_empty());
    match reported {
        None | Some("STOP") | Some("FINISH_REASON_UNSPECIFIED") => FinishReason::Stop,
        Some("MAX_TOKENS") => FinishReason::MaxTokens,
        // Gemini has no `tool_calls` finish reason: a turn that ends in a
        // function call still reports `STOP`, and the caller sees the calls.
        Some(label) if BLOCKING_FINISH_REASONS.contains(&label) => FinishReason::ContentFilter,
        // The model produced a call the service could not parse. Nothing usable
        // came back, and re-rolling is the remedy, which `Other` allows without
        // pretending the answer is complete.
        Some("MALFORMED_FUNCTION_CALL") | Some("OTHER") | Some("UNEXPECTED_TOOL_CALL") => {
            warnings.push(unknown_finish(reported.unwrap_or_default()));
            FinishReason::Other
        }
        Some(other) => {
            warnings.push(unknown_finish(other));
            FinishReason::Other
        }
    }
}

/// A sanitized warning for a finish reason this crate does not model.
fn unknown_finish(reported: &str) -> ResponseWarning {
    ResponseWarning::UnknownFinishReason {
        reported: ErrorCode::new(reported).as_str().to_owned(),
    }
}

/// Builds the normalized response from a decoded envelope.
///
/// `provider` and `model` are the *configured* keys; the reported
/// `modelVersion` replaces the configured model when the service names one,
/// because an alias resolved server-side is exactly what a replay record needs
/// to show.
///
/// Text is concatenated into **one** part placed before the tool calls, which
/// is the same shape
/// [`StreamAccumulator`](turnframe_provider::stream::StreamAccumulator) builds.
/// That is what makes the streamed and non-streamed answers comparable.
///
/// # Errors
///
/// * [`ContentFilter`](turnframe_provider::error::ProviderErrorKind::ContentFilter)
///   when the prompt was blocked, or the only candidate was blocked before
///   producing anything.
/// * [`Malformed`](turnframe_provider::error::ProviderErrorKind::Malformed)
///   when there is no candidate to read at all.
pub(crate) fn build_response(
    body: &GenerateContentResponse,
    request_id: RequestId,
    provider: &ProviderKey,
    model: &ModelKey,
    preserves_call_ids: bool,
) -> Result<ModelResponse, ProviderError> {
    let mut warnings = Vec::new();

    if let Some(reason) = body
        .prompt_feedback
        .as_ref()
        .and_then(|feedback| feedback.block_reason.as_deref())
        .filter(|reason| !reason.is_empty() && *reason != "BLOCK_REASON_UNSPECIFIED")
    {
        // The prompt never reached the model. There is no answer to return and
        // nothing a retry elsewhere should be allowed to obtain.
        return Err(ProviderError::content_filter().with_code(reason));
    }

    let Some(candidate) = body.candidates.first() else {
        return Err(ProviderError::malformed("no_candidates"));
    };
    if body.candidates.len() > 1 {
        warnings.push(ResponseWarning::FeatureDropped {
            feature: "multiple_candidates".to_owned(),
        });
    }

    let finish = finish_reason(candidate.finish_reason.as_deref(), &mut warnings);
    let (text, calls, synthesized) = read_parts(candidate, &mut warnings);

    if finish == FinishReason::ContentFilter && text.is_empty() && calls.is_empty() {
        // Blocked with nothing to show for it: a failure, not an empty answer.
        let code = candidate
            .finish_reason
            .as_deref()
            .unwrap_or("SAFETY")
            .to_owned();
        return Err(ProviderError::content_filter().with_code(code));
    }

    let mut content = Vec::with_capacity(calls.len() + 1);
    if !text.is_empty() {
        content.push(ContentPart::text(text));
    }
    content.extend(calls.into_iter().map(ContentPart::ToolCall));

    if synthesized {
        warnings.push(ResponseWarning::SynthesizedCallIds);
        if preserves_call_ids {
            // The declaration and the wire disagree. Say so on the response
            // rather than quietly renumbering: the conformance suite fails the
            // declaration check on exactly this, and the fix is to lower the
            // declaration.
            warnings.push(ResponseWarning::FeatureDropped {
                feature: "call_id_preservation".to_owned(),
            });
        }
    }

    let usage = body
        .usage_metadata
        .as_ref()
        .map(UsageMetadata::normalize)
        .unwrap_or_default();
    if usage.is_unreported() {
        warnings.push(ResponseWarning::UsageUnreported);
    }

    let reported_model = body
        .model_version
        .as_deref()
        .filter(|reported| !reported.is_empty())
        .map_or_else(|| model.clone(), ModelKey::from);

    let mut response = ModelResponse::new(request_id, provider.clone(), reported_model)
        .with_finish(finish)
        .with_usage(usage);
    response.content = content;
    response.warnings = warnings;
    if let Some(id) = body.response_id.as_deref().filter(|id| !id.is_empty()) {
        response = response.with_raw_id(id);
    }
    Ok(response)
}

/// Reads a candidate's parts into concatenated prose and ordered tool calls.
///
/// Returns whether any call id had to be synthesized.
fn read_parts(
    candidate: &Candidate,
    warnings: &mut Vec<ResponseWarning>,
) -> (String, Vec<ToolCall>, bool) {
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut synthesized = false;
    let Some(content) = candidate.content.as_ref() else {
        return (text, calls, synthesized);
    };
    for part in &content.parts {
        if let Some(call) = part.function_call.as_ref() {
            let id = match call.id.as_deref().filter(|id| !id.is_empty()) {
                Some(id) => CallId::new(id),
                None => {
                    synthesized = true;
                    CallId::new(format!("call_{}", calls.len()))
                }
            };
            calls.push(ToolCall::new(
                id,
                call.name.clone().unwrap_or_default(),
                call.args.clone().unwrap_or_else(default_args),
            ));
            continue;
        }
        if part.thought {
            // The model's own reasoning is not its answer, and putting it in
            // the reply would leak it to the user.
            warnings.push(ResponseWarning::FeatureDropped {
                feature: "thought_part".to_owned(),
            });
            continue;
        }
        if let Some(fragment) = part.text.as_deref() {
            text.push_str(fragment);
        }
    }
    (text, calls, synthesized)
}

/// The arguments of a call that declared none.
fn default_args() -> Value {
    Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turnframe_provider::error::ProviderErrorKind;

    fn decode(value: Value) -> GenerateContentResponse {
        serde_json::from_value(value).expect("decodes")
    }

    fn build(value: Value) -> Result<ModelResponse, ProviderError> {
        build_response(
            &decode(value),
            RequestId::nil(),
            &ProviderKey::from("gemini"),
            &ModelKey::from("configured"),
            false,
        )
    }

    fn answer(parts: Value, finish: &str) -> Value {
        json!({
            "candidates": [{
                "content": {"role": "model", "parts": parts},
                "finishReason": finish,
                "index": 0,
                "safetyRatings": [],
                "avgLogprobs": -0.2
            }],
            "usageMetadata": {
                "promptTokenCount": 42,
                "candidatesTokenCount": 7,
                "totalTokenCount": 49,
                "cachedContentTokenCount": 30
            },
            "modelVersion": "gemini-2.5-flash-001",
            "responseId": "resp-turnframe-1"
        })
    }

    #[test]
    fn a_plain_answer_maps_field_for_field() {
        let response = build(answer(
            json!([{"text": "Ho preparato la modifica."}]),
            "STOP",
        ))
        .expect("a valid body");
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert_eq!(response.finish, FinishReason::Stop);
        assert_eq!(response.request_id, RequestId::nil());
        assert_eq!(response.provider.as_str(), "gemini");
        // The reported model version replaces the configured key.
        assert_eq!(response.model.as_str(), "gemini-2.5-flash-001");
        assert_eq!(response.raw_id.as_deref(), Some("resp-turnframe-1"));
        // An unknown envelope field did not stop it decoding.
        assert!(response.warnings.is_empty(), "{:?}", response.warnings);
    }

    #[test]
    fn text_is_one_part_placed_before_the_calls_whatever_order_it_arrived_in() {
        let response = build(answer(
            json!([
                {"text": "controllo"},
                {"functionCall": {"name": "case.get", "args": {"target": "tok_1"}}},
                {"text": " subito"}
            ]),
            "STOP",
        ))
        .expect("a valid body");
        assert_eq!(response.content.len(), 2);
        assert_eq!(response.content[0].as_text(), Some("controllo subito"));
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "case.get");
        assert_eq!(calls[0].arguments, json!({"target": "tok_1"}));
    }

    #[test]
    fn a_call_with_no_id_gets_a_synthesized_one_and_says_so() {
        let response = build(answer(
            json!([
                {"functionCall": {"name": "a", "args": {}}},
                {"functionCall": {"name": "b", "args": {}}}
            ]),
            "STOP",
        ))
        .expect("a valid body");
        let calls = response.tool_calls();
        assert_eq!(calls[0].id.as_str(), "call_0");
        assert_eq!(calls[1].id.as_str(), "call_1");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::SynthesizedCallIds)
        );

        // Where the surface does carry an id, it survives verbatim.
        let with_id = build(answer(
            json!([{"functionCall": {"id": "fc-7", "name": "a", "args": {}}}]),
            "STOP",
        ))
        .expect("a valid body");
        assert_eq!(with_id.tool_calls()[0].id.as_str(), "fc-7");
        assert!(
            !with_id
                .warnings
                .contains(&ResponseWarning::SynthesizedCallIds)
        );
    }

    #[test]
    fn a_declaration_that_promises_id_preservation_is_contradicted_out_loud() {
        let body = decode(answer(
            json!([{"functionCall": {"name": "a", "args": {}}}]),
            "STOP",
        ));
        let response = build_response(
            &body,
            RequestId::nil(),
            &ProviderKey::from("gemini"),
            &ModelKey::from("m"),
            true,
        )
        .expect("a valid body");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "call_id_preservation".to_owned()
                })
        );
    }

    #[test]
    fn a_blocked_candidate_with_nothing_to_show_is_a_content_filter_failure() {
        for reason in BLOCKING_FINISH_REASONS {
            let error = build(json!({
                "candidates": [{"finishReason": reason, "index": 0}],
                "usageMetadata": {"promptTokenCount": 12, "totalTokenCount": 12}
            }))
            .expect_err("blocked");
            assert!(
                matches!(error.kind(), ProviderErrorKind::ContentFilter),
                "{reason} mapped to {}",
                error.kind().as_str()
            );
            assert_eq!(
                error.retry_class(),
                turnframe_provider::error::RetryClass::Fatal
            );
            assert_eq!(
                error.code().map(|code| code.as_str().to_owned()),
                Some((*reason).to_owned())
            );
        }
    }

    #[test]
    fn a_blocked_candidate_that_produced_a_prefix_is_an_incomplete_answer() {
        let response = build(answer(
            json!([{"text": "Il tuo viaggiatore si"}]),
            "RECITATION",
        ))
        .expect("a prefix arrived");
        assert_eq!(response.finish, FinishReason::ContentFilter);
        // Which is exactly what stops a structured stage parsing the prefix.
        assert!(!response.finish.is_complete());
        assert!(matches!(
            response.single_json(),
            Err(turnframe_provider::structured::StructuredOutputError::NoOutput)
        ));
    }

    #[test]
    fn a_blocked_prompt_never_produces_a_response_at_all() {
        let error = build(json!({
            "promptFeedback": {"blockReason": "SAFETY", "safetyRatings": []},
            "usageMetadata": {"promptTokenCount": 8, "totalTokenCount": 8}
        }))
        .expect_err("the prompt was refused");
        assert!(matches!(error.kind(), ProviderErrorKind::ContentFilter));
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("SAFETY".to_owned())
        );

        // The unspecified placeholder is not a block.
        assert!(
            build(json!({
                "promptFeedback": {"blockReason": "BLOCK_REASON_UNSPECIFIED"},
                "candidates": [{"content": {"parts": [{"text": "ok"}]}, "finishReason": "STOP"}]
            }))
            .is_ok()
        );
    }

    #[test]
    fn a_body_with_no_candidate_is_malformed_rather_than_empty() {
        let error =
            build(json!({"usageMetadata": {"promptTokenCount": 1}})).expect_err("nothing to read");
        assert!(matches!(error.kind(), ProviderErrorKind::Malformed));
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("no_candidates".to_owned())
        );

        // A candidate with an empty part list *is* an empty answer, though.
        let empty = build(answer(json!([]), "STOP")).expect("an empty answer");
        assert!(empty.is_empty());
        assert_eq!(empty.finish, FinishReason::Stop);
    }

    #[test]
    fn usage_keeps_googles_own_totals_and_folds_thinking_into_output() {
        let response = build(json!({
            "candidates": [{"content": {"parts": [{"text": "ok"}]}, "finishReason": "STOP"}],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 20,
                "cachedContentTokenCount": 80,
                "thoughtsTokenCount": 5,
                "totalTokenCount": 125
            }
        }))
        .expect("a valid body");
        // `promptTokenCount` already contains the cached tokens, which is what
        // `TokenUsage` documents, so nothing is subtracted.
        assert_eq!(response.usage.input, 100);
        assert_eq!(response.usage.cached_input, 80);
        // Thinking is billed as output, so it is counted there and reported.
        assert_eq!(response.usage.output, 25);
        assert_eq!(response.usage.reasoning, 5);

        // A nonsensical cache count is clamped rather than propagated.
        let clamped = UsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 1,
            cached_content_token_count: 99,
            thoughts_token_count: 0,
        }
        .normalize();
        assert_eq!(clamped.cached_input, 10);
    }

    #[test]
    fn an_unreported_usage_is_said_out_loud_rather_than_guessed() {
        let response = build(json!({
            "candidates": [{"content": {"parts": [{"text": "ok"}]}, "finishReason": "STOP"}]
        }))
        .expect("a valid body");
        assert!(response.usage.is_unreported());
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::UsageUnreported)
        );
    }

    #[test]
    fn a_finish_reason_this_crate_does_not_model_is_reported_not_guessed() {
        let mut warnings = Vec::new();
        assert_eq!(
            finish_reason(Some("STOP"), &mut warnings),
            FinishReason::Stop
        );
        assert_eq!(finish_reason(None, &mut warnings), FinishReason::Stop);
        assert_eq!(finish_reason(Some(""), &mut warnings), FinishReason::Stop);
        assert_eq!(
            finish_reason(Some("MAX_TOKENS"), &mut warnings),
            FinishReason::MaxTokens
        );
        assert!(warnings.is_empty());

        assert_eq!(
            finish_reason(Some("MALFORMED_FUNCTION_CALL"), &mut warnings),
            FinishReason::Other
        );
        assert_eq!(
            finish_reason(Some("SOMETHING_NEW"), &mut warnings),
            FinishReason::Other
        );
        assert_eq!(warnings.len(), 2);
        assert!(matches!(
            &warnings[1],
            ResponseWarning::UnknownFinishReason { reported } if reported == "SOMETHING_NEW"
        ));
    }

    #[test]
    fn a_thought_part_is_kept_out_of_the_reply() {
        let response = build(answer(
            json!([
                {"text": "il viaggiatore chiede...", "thought": true},
                {"text": "Ho preparato la modifica."}
            ]),
            "STOP",
        ))
        .expect("a valid body");
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "thought_part".to_owned()
                })
        );
    }

    #[test]
    fn a_second_candidate_is_reported_rather_than_merged() {
        let response = build(json!({
            "candidates": [
                {"content": {"parts": [{"text": "prima"}]}, "finishReason": "STOP"},
                {"content": {"parts": [{"text": "seconda"}]}, "finishReason": "STOP"}
            ]
        }))
        .expect("a valid body");
        assert_eq!(response.text(), "prima");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "multiple_candidates".to_owned()
                })
        );
    }
}
