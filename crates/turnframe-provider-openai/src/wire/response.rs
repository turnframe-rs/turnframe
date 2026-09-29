//! Chat-completions body → normalized response.
//!
//! # Why nothing here is `deny_unknown_fields`
//!
//! The workspace rule is that structures parsed from a client are strict. This
//! module is the documented exception, and the reason is structural: the
//! chat-completions *envelope* is an open set. Every gateway adds fields to it
//! — `system_fingerprint`, `service_tier`, provider-specific timing blocks,
//! citation arrays, reasoning traces — and a strict envelope would reject a
//! perfectly good answer from an endpoint this crate exists to serve.
//!
//! The strictness that invariant I18 actually needs is applied one layer up,
//! to the *model's own payload*, by
//! [`parse_structured`](turnframe_provider::structured::parse_structured): the
//! plan is validated against its schema with deny-unknown semantics, and one
//! malformed act rejects the whole document. Tolerating an unknown envelope
//! field cannot smuggle an act past that gate.
//!
//! What this module does refuse: a body with no choices, a tool call whose
//! arguments are not JSON, and a `finish_reason` it does not recognize (which
//! becomes [`FinishReason::Other`] plus a warning, never a guess at `Stop`).

use serde::Deserialize;
use serde_json::Value;
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelKey, ProviderKey, RequestId};
use turnframe_provider::request::{ContentPart, ToolCall};
use turnframe_provider::response::{FinishReason, ModelResponse, ResponseWarning, TokenUsage};

/// The chat-completions response envelope.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ChatResponse {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) choices: Vec<Choice>,
    #[serde(default)]
    pub(crate) usage: Option<Usage>,
}

/// One completion choice.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Choice {
    #[serde(default)]
    pub(crate) message: Option<ResponseMessage>,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

/// The assistant message inside a choice.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ResponseMessage {
    #[serde(default)]
    pub(crate) content: Option<WireText>,
    /// OpenAI's structured-output refusal channel: prose explaining why the
    /// model declined, with `content` left null.
    #[serde(default)]
    pub(crate) refusal: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<WireToolCall>,
}

/// Message text, in both shapes endpoints send it.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum WireText {
    /// The ordinary case.
    Text(String),
    /// A parts array, which a few gateways use even for pure text.
    Parts(Vec<TextPart>),
}

impl WireText {
    /// Flattens to a single string.
    pub(crate) fn flatten(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|part| part.text.as_deref())
                .collect(),
        }
    }
}

/// One element of a parts-shaped content array.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TextPart {
    #[serde(default)]
    pub(crate) text: Option<String>,
}

/// A tool call in a completed response.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct WireToolCall {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) function: WireFunction,
}

/// The function half of a tool call.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct WireFunction {
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Arguments as a JSON string, which is how this format carries them.
    #[serde(default)]
    pub(crate) arguments: Option<String>,
}

/// Reported token usage, across the several shapes endpoints report it in.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub(crate) struct Usage {
    #[serde(default)]
    pub(crate) prompt_tokens: u64,
    #[serde(default)]
    pub(crate) completion_tokens: u64,
    #[serde(default)]
    pub(crate) prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)]
    pub(crate) completion_tokens_details: Option<CompletionTokensDetails>,
    /// Some gateways report the cache hit at the top level instead.
    #[serde(default)]
    pub(crate) cached_tokens: Option<u64>,
    /// And some name it after the cache rather than after the tokens.
    #[serde(default)]
    pub(crate) prompt_cache_hit_tokens: Option<u64>,
}

/// The `prompt_tokens_details` block.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub(crate) struct PromptTokensDetails {
    #[serde(default)]
    pub(crate) cached_tokens: u64,
}

/// The `completion_tokens_details` block.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub(crate) struct CompletionTokensDetails {
    #[serde(default)]
    pub(crate) reasoning_tokens: u64,
}

impl Usage {
    /// Normalizes into [`TokenUsage`], preferring the nested cache counter and
    /// falling back to the two flatter spellings.
    pub(crate) fn normalize(&self) -> TokenUsage {
        let cached = self
            .prompt_tokens_details
            .map(|details| details.cached_tokens)
            .filter(|cached| *cached > 0)
            .or(self.cached_tokens)
            .or(self.prompt_cache_hit_tokens)
            .unwrap_or(0);
        let reasoning = self
            .completion_tokens_details
            .map_or(0, |details| details.reasoning_tokens);
        TokenUsage::new(self.prompt_tokens, self.completion_tokens)
            .with_cached_input(cached)
            .with_reasoning(reasoning)
    }
}

/// Maps a vendor `finish_reason` onto the normalized one.
///
/// An unrecognized label becomes [`FinishReason::Other`] and a
/// [`ResponseWarning::UnknownFinishReason`]: a stage that needs a complete
/// answer then refuses it, which is the safe reading of "the endpoint stopped
/// for a reason we do not model". A **missing** label becomes
/// [`FinishReason::Stop`], because several compatible endpoints simply omit the
/// field on a normal completion.
pub(crate) fn finish_reason(
    reported: Option<&str>,
    warnings: &mut Vec<ResponseWarning>,
) -> FinishReason {
    match reported {
        None | Some("") | Some("stop") | Some("end_turn") | Some("eos") => FinishReason::Stop,
        Some("length") | Some("max_tokens") | Some("model_length") => FinishReason::MaxTokens,
        Some("tool_calls") | Some("function_call") | Some("tool_use") => FinishReason::ToolCalls,
        Some("content_filter") => FinishReason::ContentFilter,
        Some("refusal") => FinishReason::Refusal,
        Some(other) => {
            warnings.push(ResponseWarning::UnknownFinishReason {
                reported: turnframe_provider::error::ErrorCode::new(other)
                    .as_str()
                    .to_owned(),
            });
            FinishReason::Other
        }
    }
}

/// Builds the normalized response from a decoded envelope.
///
/// `provider` and `model` are the *configured* keys; the reported model
/// replaces the configured one when the endpoint names it, because an alias
/// resolved server-side is exactly what a replay record needs to show.
///
/// # Errors
///
/// Returns [`ProviderErrorKind::Malformed`](turnframe_provider::error::ProviderErrorKind::Malformed)
/// when there is no choice to read or a tool call's arguments are not JSON.
pub(crate) fn build_response(
    body: &ChatResponse,
    request_id: RequestId,
    provider: &ProviderKey,
    model: &ModelKey,
    preserves_call_ids: bool,
) -> Result<ModelResponse, ProviderError> {
    let mut warnings = Vec::new();
    let Some(choice) = body.choices.first() else {
        return Err(ProviderError::malformed("no_choices"));
    };
    if body.choices.len() > 1 {
        warnings.push(ResponseWarning::FeatureDropped {
            feature: "multiple_choices".to_owned(),
        });
    }
    let reported_model = body
        .model
        .as_deref()
        .filter(|reported| !reported.is_empty())
        .map_or_else(|| model.clone(), ModelKey::from);

    let mut finish = finish_reason(choice.finish_reason.as_deref(), &mut warnings);
    let message = choice.message.clone().unwrap_or_default();

    // A refusal is a semantic outcome, not a malformed answer: the text is the
    // model's explanation and the finish reason says so, so a structured stage
    // reports `Refusal` rather than a parse failure.
    let text = match message.refusal.as_deref().filter(|r| !r.is_empty()) {
        Some(refusal) => {
            finish = FinishReason::Refusal;
            refusal.to_owned()
        }
        None => message
            .content
            .as_ref()
            .map(WireText::flatten)
            .unwrap_or_default(),
    };

    let mut content = Vec::with_capacity(message.tool_calls.len() + 1);
    if !text.is_empty() {
        content.push(ContentPart::text(text));
    }
    let mut synthesized = false;
    for (index, call) in message.tool_calls.iter().enumerate() {
        let id = match call.id.as_deref().filter(|id| !id.is_empty()) {
            Some(id) => CallId::new(id),
            None => {
                synthesized = true;
                CallId::new(format!("call_{index}"))
            }
        };
        content.push(ContentPart::ToolCall(ToolCall::new(
            id,
            call.function.name.clone().unwrap_or_default(),
            parse_arguments(call.function.arguments.as_deref())?,
        )));
    }
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
        .usage
        .as_ref()
        .map(Usage::normalize)
        .unwrap_or_default();
    if usage.is_unreported() {
        warnings.push(ResponseWarning::UsageUnreported);
    }

    let mut response = ModelResponse::new(request_id, provider.clone(), reported_model)
        .with_finish(finish)
        .with_usage(usage);
    response.content = content;
    response.warnings = warnings;
    if let Some(id) = body.id.as_deref().filter(|id| !id.is_empty()) {
        response = response.with_raw_id(id);
    }
    Ok(response)
}

/// Decodes a tool call's arguments string.
///
/// An absent or blank argument string is an empty object — several endpoints
/// send `""` for a no-argument call — but a *present and broken* one is a
/// failure, never a best effort.
fn parse_arguments(raw: Option<&str>) -> Result<Value, ProviderError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(Value::Object(serde_json::Map::new()));
    };
    serde_json::from_str(raw).map_err(|_| ProviderError::malformed("tool_arguments_not_json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(value: Value) -> ChatResponse {
        serde_json::from_value(value).expect("decodes")
    }

    fn build(value: Value) -> Result<ModelResponse, ProviderError> {
        build_response(
            &decode(value),
            RequestId::nil(),
            &ProviderKey::from("openai"),
            &ModelKey::from("configured"),
            true,
        )
    }

    #[test]
    fn a_plain_answer_maps_field_for_field() {
        let response = build(json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o-2024-08-06",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Ho preparato la modifica."},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 42,
                "completion_tokens": 7,
                "prompt_tokens_details": {"cached_tokens": 30},
                "completion_tokens_details": {"reasoning_tokens": 3}
            }
        }))
        .expect("maps");
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert_eq!(response.finish, FinishReason::Stop);
        assert_eq!(response.raw_id.as_deref(), Some("chatcmpl-1"));
        // The *reported* model wins over the configured alias.
        assert_eq!(response.model.as_str(), "gpt-4o-2024-08-06");
        assert_eq!(response.usage.input, 42);
        assert_eq!(response.usage.output, 7);
        assert_eq!(response.usage.cached_input, 30);
        assert_eq!(response.usage.reasoning, 3);
        assert!(response.warnings.is_empty(), "{:?}", response.warnings);
    }

    #[test]
    fn an_unknown_envelope_field_is_tolerated() {
        let response = build(json!({
            "id": "chatcmpl-1",
            "system_fingerprint": "fp_1",
            "service_tier": "default",
            "x_gateway": {"anything": [1, 2, 3]},
            "choices": [{
                "message": {"content": "ok", "reasoning_content": "…"},
                "finish_reason": "stop",
                "logprobs": null
            }]
        }))
        .expect("maps");
        assert_eq!(response.text(), "ok");
    }

    #[test]
    fn tool_calls_keep_their_ids_and_order() {
        let response = build(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [
                        {"id": "call_a", "type": "function",
                         "function": {"name": "first", "arguments": "{\"x\":1}"}},
                        {"id": "call_b", "type": "function",
                         "function": {"name": "second", "arguments": ""}}
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("maps");
        assert_eq!(response.finish, FinishReason::ToolCalls);
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_str(), "call_a");
        assert_eq!(calls[0].arguments, json!({"x": 1}));
        assert_eq!(calls[1].id.as_str(), "call_b");
        assert_eq!(calls[1].arguments, json!({}));
    }

    #[test]
    fn text_comes_before_tool_calls_so_a_stream_can_match_it() {
        let response = build(json!({
            "choices": [{
                "message": {
                    "content": "un momento",
                    "tool_calls": [{"id": "c", "function": {"name": "n", "arguments": "{}"}}]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("maps");
        assert!(matches!(response.content[0], ContentPart::Text { .. }));
        assert!(matches!(response.content[1], ContentPart::ToolCall(_)));
    }

    #[test]
    fn broken_tool_arguments_fail_the_whole_response() {
        let error = build(json!({
            "choices": [{
                "message": {"tool_calls": [{"id": "c", "function": {"name": "n",
                    "arguments": "{\"x\": "}}]},
                "finish_reason": "tool_calls"
            }]
        }))
        .expect_err("refused");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("tool_arguments_not_json".to_owned())
        );
    }

    #[test]
    fn a_missing_id_is_synthesized_and_declared_as_a_disagreement() {
        let response = build(json!({
            "choices": [{
                "message": {"tool_calls": [{"function": {"name": "n", "arguments": "{}"}}]},
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("maps");
        assert_eq!(response.tool_calls()[0].id.as_str(), "call_0");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::SynthesizedCallIds)
        );
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "call_id_preservation".to_owned()
                })
        );
    }

    #[test]
    fn a_refusal_is_its_own_outcome() {
        let response = build(json!({
            "choices": [{
                "message": {"content": null, "refusal": "I cannot help with that request."},
                "finish_reason": "stop"
            }]
        }))
        .expect("maps");
        assert_eq!(response.finish, FinishReason::Refusal);
        assert_eq!(response.text(), "I cannot help with that request.");
        assert!(matches!(
            response.single_json(),
            Err(turnframe_provider::structured::StructuredOutputError::Refusal)
        ));
    }

    #[test]
    fn an_empty_body_has_no_choice_to_read() {
        let error = build(json!({"id": "chatcmpl-1", "choices": []})).expect_err("refused");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("no_choices".to_owned())
        );
    }

    #[test]
    fn empty_content_becomes_an_empty_response_not_an_empty_plan() {
        let response = build(json!({
            "choices": [{"message": {"content": ""}, "finish_reason": "stop"}]
        }))
        .expect("maps");
        assert!(response.content.is_empty());
        assert!(response.is_empty());
        assert!(matches!(
            response.single_json(),
            Err(turnframe_provider::structured::StructuredOutputError::NoOutput)
        ));
    }

    #[test]
    fn finish_reasons_map_across_the_spellings_endpoints_use() {
        let mut warnings = Vec::new();
        assert_eq!(finish_reason(None, &mut warnings), FinishReason::Stop);
        assert_eq!(
            finish_reason(Some("stop"), &mut warnings),
            FinishReason::Stop
        );
        assert_eq!(
            finish_reason(Some("length"), &mut warnings),
            FinishReason::MaxTokens
        );
        assert_eq!(
            finish_reason(Some("model_length"), &mut warnings),
            FinishReason::MaxTokens
        );
        assert_eq!(
            finish_reason(Some("tool_calls"), &mut warnings),
            FinishReason::ToolCalls
        );
        assert_eq!(
            finish_reason(Some("function_call"), &mut warnings),
            FinishReason::ToolCalls
        );
        assert_eq!(
            finish_reason(Some("content_filter"), &mut warnings),
            FinishReason::ContentFilter
        );
        assert!(warnings.is_empty());

        assert_eq!(
            finish_reason(Some("something new"), &mut warnings),
            FinishReason::Other
        );
        assert_eq!(
            warnings,
            vec![ResponseWarning::UnknownFinishReason {
                reported: "something_new".to_owned()
            }]
        );
    }

    #[test]
    fn a_parts_shaped_content_array_flattens() {
        let response = build(json!({
            "choices": [{
                "message": {"content": [{"type": "text", "text": "due "},
                                        {"type": "text", "text": "pezzi"}]},
                "finish_reason": "stop"
            }]
        }))
        .expect("maps");
        assert_eq!(response.text(), "due pezzi");
    }

    #[test]
    fn usage_is_read_from_whichever_place_the_endpoint_puts_it() {
        let nested = Usage {
            prompt_tokens: 10,
            completion_tokens: 2,
            prompt_tokens_details: Some(PromptTokensDetails { cached_tokens: 8 }),
            ..Usage::default()
        };
        assert_eq!(nested.normalize().cached_input, 8);

        let flat = Usage {
            prompt_tokens: 10,
            cached_tokens: Some(6),
            ..Usage::default()
        };
        assert_eq!(flat.normalize().cached_input, 6);

        let hit = Usage {
            prompt_tokens: 10,
            prompt_cache_hit_tokens: Some(4),
            ..Usage::default()
        };
        assert_eq!(hit.normalize().cached_input, 4);
        assert_eq!(Usage::default().normalize(), TokenUsage::none());
    }

    #[test]
    fn unreported_usage_is_warned_about() {
        let response = build(json!({
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}]
        }))
        .expect("maps");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::UsageUnreported)
        );
    }

    #[test]
    fn a_second_choice_is_dropped_loudly() {
        let response = build(json!({
            "choices": [
                {"message": {"content": "one"}, "finish_reason": "stop"},
                {"message": {"content": "two"}, "finish_reason": "stop"}
            ]
        }))
        .expect("maps");
        assert_eq!(response.text(), "one");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "multiple_choices".to_owned()
                })
        );
    }
}
