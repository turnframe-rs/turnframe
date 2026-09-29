//! Messages API body → normalized response.
//!
//! # Why nothing here is `deny_unknown_fields`
//!
//! The workspace rule is that structures parsed from a client are strict. This
//! module is the documented exception, and the reason is structural: the
//! Messages *envelope* is an open set. Anthropic adds fields to it — a
//! `container` block, server-tool results, a `context_management` report — and
//! the endpoints that reimplement the API add their own. A strict envelope
//! would reject a perfectly good answer.
//!
//! The strictness invariant I18 actually needs is applied one layer up, to the
//! *model's own payload*, by
//! [`parse_structured`](turnframe_provider::structured::parse_structured): the
//! plan is validated against its schema with deny-unknown semantics, and one
//! malformed act rejects the whole document. Tolerating an unknown envelope
//! field cannot smuggle an act past that gate.
//!
//! # What this module does refuse
//!
//! A `tool_use` block with no name. Everything else has an honest reading: an
//! empty `content` array is an empty answer, an unknown block type is dropped
//! with a warning, and a `stop_reason` this crate does not model becomes
//! [`FinishReason::Other`] — which a structured stage refuses — never a guess
//! at [`FinishReason::Stop`].
//!
//! # Usage, and what "input" means
//!
//! Anthropic reports four counters and its `input_tokens` **excludes** the
//! cached ones. [`TokenUsage`] documents the opposite: `cached_input` is
//! already counted in `input`. So the normalization adds the two cache counters
//! back into `input` and reports only the *read* half as `cached_input` —
//! creation tokens were not served from a cache, they filled one, and they cost
//! more rather than less.

use serde::Deserialize;
use serde_json::Value;
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelKey, ProviderKey, RequestId};
use turnframe_provider::request::{ContentPart, ToolCall};
use turnframe_provider::response::{FinishReason, ModelResponse, ResponseWarning, TokenUsage};

/// The Messages response envelope.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct MessageResponse {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) content: Vec<ResponseBlock>,
    #[serde(default)]
    pub(crate) stop_reason: Option<String>,
    #[serde(default)]
    pub(crate) usage: Option<Usage>,
}

/// One block of a completed answer.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseBlock {
    /// Prose.
    Text {
        #[serde(default)]
        text: String,
    },
    /// A call the model made. `input` is already a JSON value here — unlike the
    /// chat-completions format, which carries it as a string that has to be
    /// parsed and can fail.
    ToolUse {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        input: Option<Value>,
    },
    /// Anything else: a thinking block, a server-tool result, whatever the API
    /// grows next. Dropped, and reported as dropped.
    #[serde(other)]
    Unsupported,
}

/// Reported token usage.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub(crate) struct Usage {
    #[serde(default)]
    pub(crate) input_tokens: u64,
    #[serde(default)]
    pub(crate) output_tokens: u64,
    #[serde(default)]
    pub(crate) cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) cache_read_input_tokens: Option<u64>,
}

impl Usage {
    /// Normalizes into [`TokenUsage`].
    ///
    /// `input` becomes the whole prompt — fresh, cache-filling and cache-read
    /// tokens together — because that is what [`TokenUsage::input`] means, and
    /// `cached_input` reports only the tokens that were actually served from
    /// the cache.
    pub(crate) fn normalize(&self) -> TokenUsage {
        let created = self.cache_creation_input_tokens.unwrap_or(0);
        let read = self.cache_read_input_tokens.unwrap_or(0);
        let input = self
            .input_tokens
            .saturating_add(created)
            .saturating_add(read);
        TokenUsage::new(input, self.output_tokens).with_cached_input(read)
    }

    /// Folds a later report into this one, keeping whatever it reported.
    ///
    /// A stream splits usage across `message_start` (the prompt) and
    /// `message_delta` (the completion), so the two have to be merged before
    /// the counts can be compared with the non-streamed ones.
    pub(crate) fn merge(&mut self, later: &Self) {
        if later.input_tokens > 0 {
            self.input_tokens = later.input_tokens;
        }
        if later.output_tokens > 0 {
            self.output_tokens = later.output_tokens;
        }
        if later.cache_creation_input_tokens.is_some() {
            self.cache_creation_input_tokens = later.cache_creation_input_tokens;
        }
        if later.cache_read_input_tokens.is_some() {
            self.cache_read_input_tokens = later.cache_read_input_tokens;
        }
    }
}

/// Maps a vendor `stop_reason` onto the normalized one.
///
/// An unrecognized label — `pause_turn` and whatever follows it — becomes
/// [`FinishReason::Other`] plus a [`ResponseWarning::UnknownFinishReason`]: a
/// stage that needs a complete answer then refuses it, which is the safe
/// reading of "the model stopped for a reason we do not model". A **missing**
/// label is [`FinishReason::Stop`], because that is what a mid-stream message
/// carries before it ends.
pub(crate) fn finish_reason(
    reported: Option<&str>,
    warnings: &mut Vec<ResponseWarning>,
) -> FinishReason {
    match reported {
        None | Some("") | Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
        // The window ran out mid-generation: the answer is a prefix either way.
        Some("max_tokens") | Some("model_context_window_exceeded") => FinishReason::MaxTokens,
        Some("tool_use") => FinishReason::ToolCalls,
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
/// Every text block is concatenated into **one** leading text part and the
/// tool calls follow in order — the same shape
/// [`StreamAccumulator`](turnframe_provider::stream::StreamAccumulator)
/// produces, which is what makes the two paths comparable (spec §20.8).
///
/// # Errors
///
/// Returns [`ProviderErrorKind::Malformed`](turnframe_provider::error::ProviderErrorKind::Malformed)
/// for a `tool_use` block with no name: a call the runtime cannot route is not
/// a call it may guess at.
pub(crate) fn build_response(
    body: &MessageResponse,
    request_id: RequestId,
    provider: &ProviderKey,
    model: &ModelKey,
    preserves_call_ids: bool,
) -> Result<ModelResponse, ProviderError> {
    let mut warnings = Vec::new();
    let finish = finish_reason(body.stop_reason.as_deref(), &mut warnings);
    let reported_model = body
        .model
        .as_deref()
        .filter(|reported| !reported.is_empty())
        .map_or_else(|| model.clone(), ModelKey::from);

    let mut text = String::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    let mut synthesized = false;
    let mut unsupported = false;
    for block in &body.content {
        match block {
            ResponseBlock::Text { text: fragment } => text.push_str(fragment),
            ResponseBlock::ToolUse { id, name, input } => {
                let Some(name) = name.as_deref().filter(|name| !name.is_empty()) else {
                    return Err(ProviderError::malformed("tool_use_without_name"));
                };
                let id = match id.as_deref().filter(|id| !id.is_empty()) {
                    Some(id) => CallId::new(id),
                    None => {
                        synthesized = true;
                        CallId::new(format!("call_{}", calls.len()))
                    }
                };
                let input = input
                    .clone()
                    .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                calls.push(ToolCall::new(id, name, input));
            }
            ResponseBlock::Unsupported => unsupported = true,
        }
    }
    if unsupported {
        warnings.push(ResponseWarning::FeatureDropped {
            feature: "unsupported_content_block".to_owned(),
        });
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

    let mut content: Vec<ContentPart> = Vec::with_capacity(calls.len() + 1);
    if !text.is_empty() {
        content.push(ContentPart::text(text));
    }
    content.extend(calls.into_iter().map(ContentPart::ToolCall));

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(value: Value) -> MessageResponse {
        serde_json::from_value(value).expect("decodes")
    }

    fn build(value: Value) -> Result<ModelResponse, ProviderError> {
        build_response(
            &decode(value),
            RequestId::nil(),
            &ProviderKey::from("anthropic"),
            &ModelKey::from("configured-model"),
            true,
        )
    }

    #[test]
    fn a_complete_answer_maps_field_for_field() {
        let response = build(json!({
            "id": "msg_01XYZ",
            "type": "message",
            "role": "assistant",
            "model": "claude-reported-1",
            "content": [
                {"type": "text", "text": "Ho preparato "},
                {"type": "text", "text": "la modifica."},
                {"type": "tool_use", "id": "toolu_1", "name": "plan", "input": {"acts": []}}
            ],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 7,
                "cache_creation_input_tokens": 5,
                "cache_read_input_tokens": 30
            }
        }))
        .expect("builds");

        // One text part, in front of the calls.
        assert_eq!(response.content.len(), 2);
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert_eq!(response.content[0].kind(), "text");
        assert_eq!(response.tool_calls()[0].id.as_str(), "toolu_1");
        assert_eq!(response.tool_calls()[0].arguments, json!({"acts": []}));
        assert_eq!(response.finish, FinishReason::ToolCalls);
        assert_eq!(response.raw_id.as_deref(), Some("msg_01XYZ"));
        // The reported model wins over the configured one.
        assert_eq!(response.model.as_str(), "claude-reported-1");
        // 10 fresh + 5 written + 30 read = 45 prompt tokens, 30 of them cached.
        assert_eq!(response.usage.input, 45);
        assert_eq!(response.usage.cached_input, 30);
        assert_eq!(response.usage.output, 7);
        assert!(response.warnings.is_empty());
    }

    #[test]
    fn stop_reasons_map_onto_the_normalized_family() {
        for (reported, expected) in [
            ("end_turn", FinishReason::Stop),
            ("stop_sequence", FinishReason::Stop),
            ("max_tokens", FinishReason::MaxTokens),
            ("model_context_window_exceeded", FinishReason::MaxTokens),
            ("tool_use", FinishReason::ToolCalls),
            ("refusal", FinishReason::Refusal),
        ] {
            let mut warnings = Vec::new();
            assert_eq!(finish_reason(Some(reported), &mut warnings), expected);
            assert!(warnings.is_empty(), "{reported} warned");
        }
        let mut warnings = Vec::new();
        assert_eq!(finish_reason(None, &mut warnings), FinishReason::Stop);
        assert_eq!(
            finish_reason(Some("pause_turn"), &mut warnings),
            FinishReason::Other
        );
        assert_eq!(
            warnings,
            vec![ResponseWarning::UnknownFinishReason {
                reported: "pause_turn".to_owned()
            }]
        );
        assert!(!FinishReason::Other.is_complete());
    }

    #[test]
    fn a_refusal_is_a_finish_reason_and_not_a_parse_failure() {
        let response = build(json!({
            "id": "msg_1",
            "content": [{"type": "text", "text": "I cannot help with that request."}],
            "stop_reason": "refusal",
            "usage": {"input_tokens": 4, "output_tokens": 9}
        }))
        .expect("builds");
        assert_eq!(response.finish, FinishReason::Refusal);
        assert!(matches!(
            response.single_json(),
            Err(turnframe_provider::structured::StructuredOutputError::Refusal)
        ));
    }

    #[test]
    fn an_empty_answer_is_empty_and_never_a_plan() {
        let response = build(json!({
            "id": "msg_1",
            "content": [],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 4, "output_tokens": 0}
        }))
        .expect("builds");
        assert!(response.is_empty());
        assert!(matches!(
            response.single_json(),
            Err(turnframe_provider::structured::StructuredOutputError::NoOutput)
        ));
    }

    #[test]
    fn an_unknown_block_is_dropped_and_reported() {
        let response = build(json!({
            "id": "msg_1",
            "content": [
                {"type": "thinking", "thinking": "step one", "signature": "sig"},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }))
        .expect("builds");
        assert_eq!(response.text(), "answer");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "unsupported_content_block".to_owned()
                })
        );
    }

    #[test]
    fn a_call_without_a_name_is_malformed_and_a_call_without_an_id_is_flagged() {
        let error = build(json!({
            "content": [{"type": "tool_use", "id": "toolu_1", "input": {}}],
            "stop_reason": "tool_use"
        }))
        .expect_err("a call the runtime cannot route");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("tool_use_without_name".to_owned())
        );

        let response = build(json!({
            "content": [{"type": "tool_use", "name": "plan"}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }))
        .expect("builds");
        assert_eq!(response.tool_calls()[0].id.as_str(), "call_0");
        assert_eq!(response.tool_calls()[0].arguments, json!({}));
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::SynthesizedCallIds)
        );
        // The declaration said ids survive; the wire disagreed, and it is said.
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "call_id_preservation".to_owned()
                })
        );
    }

    #[test]
    fn unreported_usage_is_zero_and_says_so() {
        let response = build(json!({"content": [], "stop_reason": "end_turn"})).expect("builds");
        assert!(response.usage.is_unreported());
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::UsageUnreported)
        );
        // The configured model is kept when the endpoint names none.
        assert_eq!(response.model.as_str(), "configured-model");
        assert!(response.raw_id.is_none());
    }

    #[test]
    fn usage_merges_the_two_halves_a_stream_reports() {
        let mut usage = Usage {
            input_tokens: 10,
            output_tokens: 1,
            cache_creation_input_tokens: Some(5),
            cache_read_input_tokens: Some(30),
        };
        usage.merge(&Usage {
            output_tokens: 7,
            ..Usage::default()
        });
        assert_eq!(
            usage.normalize(),
            TokenUsage::new(45, 7).with_cached_input(30)
        );
        // A later report that names the prompt again replaces it.
        usage.merge(&Usage {
            input_tokens: 12,
            ..Usage::default()
        });
        assert_eq!(usage.normalize().input, 47);
    }

    #[test]
    fn an_open_envelope_still_decodes() {
        let response = build(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok", "citations": null}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 1, "output_tokens": 1, "service_tier": "standard"},
            "container": null
        }))
        .expect("an unknown envelope field is not a failure");
        assert_eq!(response.text(), "ok");
    }
}
