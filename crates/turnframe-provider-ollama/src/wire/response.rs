//! `/api/chat` body → normalized response.
//!
//! # Why nothing here is `deny_unknown_fields`
//!
//! The workspace rule is that structures parsed from a client are strict. This
//! module is the documented exception, and the reason is structural: Ollama's
//! chat envelope grows with the daemon. `total_duration`, `load_duration`,
//! `prompt_eval_duration`, `eval_duration`, `created_at`, `thinking` and the
//! debug fields a build adds all arrive unannounced, and a strict envelope
//! would reject a perfectly good answer from a daemon a version ahead.
//!
//! The strictness invariant I18 actually needs is applied one layer up, to the
//! *model's own payload*, by
//! [`parse_structured`](turnframe_provider::structured::parse_structured): the
//! plan is validated against its schema with deny-unknown semantics, and one
//! malformed act rejects the whole document.
//!
//! # Three things this endpoint does not give us
//!
//! * **No call ids.** A tool call is a name and an arguments object, nothing
//!   more, so the ids are synthesized from the call's position and every
//!   response carrying one says so with
//!   [`SynthesizedCallIds`](ResponseWarning::SynthesizedCallIds). A profile
//!   here therefore always declares `preserves_call_ids: false`, and the
//!   builder refuses to say otherwise.
//! * **No response id.** There is nothing to put in
//!   [`raw_id`](turnframe_provider::response::ModelResponse::raw_id), so it
//!   stays empty rather than being filled with a timestamp that identifies
//!   nothing.
//! * **No cache accounting.** The daemon reuses its own KV cache between calls
//!   and reports not a token of it, so
//!   [`cached_input`](turnframe_provider::response::TokenUsage::cached_input)
//!   is always zero — which the contract reads as "not reported", exactly what
//!   is true here.

use serde::Deserialize;
use serde_json::Value;
use turnframe_provider::error::{ErrorCode, ProviderError};
use turnframe_provider::ids::{CallId, ModelKey, ProviderKey, RequestId};
use turnframe_provider::request::{ContentPart, ToolCall};
use turnframe_provider::response::{FinishReason, ModelResponse, ResponseWarning, TokenUsage};

/// The `/api/chat` response envelope, and the shape of one streamed chunk.
///
/// Ollama sends the same object either way: a non-streamed call is one object
/// with `done: true`, and a streamed call is a sequence of them of which only
/// the last is done.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ChatResponse {
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) message: Option<ResponseMessage>,
    #[serde(default)]
    pub(crate) done: bool,
    #[serde(default)]
    pub(crate) done_reason: Option<String>,
    #[serde(default)]
    pub(crate) prompt_eval_count: Option<u64>,
    #[serde(default)]
    pub(crate) eval_count: Option<u64>,
    /// A daemon that fails mid-stream reports it as an error object inside an
    /// otherwise successful body.
    #[serde(default)]
    pub(crate) error: Option<Value>,
}

/// The assistant message inside a response or a chunk.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ResponseMessage {
    #[serde(default)]
    pub(crate) content: Option<String>,
    /// A reasoning model's private trace. Never part of the answer.
    #[serde(default)]
    pub(crate) thinking: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<WireToolCall>,
}

/// A tool call, as this endpoint spells it: a function and nothing else.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct WireToolCall {
    #[serde(default)]
    pub(crate) function: WireToolFunction,
}

/// The function half of a tool call.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct WireToolFunction {
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Arguments as a JSON **object**, not as a string.
    #[serde(default)]
    pub(crate) arguments: Option<Value>,
}

impl ChatResponse {
    /// Reported usage, normalized.
    ///
    /// `prompt_eval_count` is the prompt, `eval_count` is the generation, and
    /// there is no third number: a local runtime bills nothing, caches nothing
    /// it will admit to, and has no separate reasoning counter.
    pub(crate) fn usage(&self) -> TokenUsage {
        TokenUsage::new(
            self.prompt_eval_count.unwrap_or(0),
            self.eval_count.unwrap_or(0),
        )
    }
}

/// The arguments of one tool call, normalized.
///
/// An absent or null arguments field becomes the empty object, in both the
/// whole and the streamed path, so a no-argument call reads the same either
/// way.
pub(crate) fn tool_arguments(function: &WireToolFunction) -> Value {
    match function.arguments.as_ref() {
        Some(Value::Null) | None => Value::Object(serde_json::Map::new()),
        Some(arguments) => arguments.clone(),
    }
}

/// The call id used for the tool call at `index`.
///
/// Deterministic and shared by the whole and the streamed path, so the two
/// produce the same response for the same exchange.
pub(crate) fn synthesized_call_id(index: usize) -> CallId {
    CallId::new(format!("call_{index}"))
}

/// Maps Ollama's `done_reason` onto the normalized finish reason.
///
/// `stop` with tool calls present is [`FinishReason::ToolCalls`]: the daemon
/// reports the runner's stop condition and has no separate label for "ended its
/// turn to call something", but the caller needs the distinction. A label this
/// crate does not model becomes [`FinishReason::Other`] plus a warning, never a
/// guess at `Stop` — `load` and `unload` are real values that mean the daemon
/// (un)loaded a model rather than generating an answer.
pub(crate) fn finish_reason(
    reported: Option<&str>,
    has_tool_calls: bool,
    warnings: &mut Vec<ResponseWarning>,
) -> FinishReason {
    match reported {
        None | Some("") | Some("stop") => {
            if has_tool_calls {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }
        }
        Some("length") => FinishReason::MaxTokens,
        Some(other) => {
            warnings.push(ResponseWarning::UnknownFinishReason {
                reported: ErrorCode::new(other).as_str().to_owned(),
            });
            FinishReason::Other
        }
    }
}

/// Builds the normalized response from a decoded envelope.
///
/// `provider` and `model` are the *configured* keys; the reported model
/// replaces the configured one when the daemon names it, because the tag it
/// actually served is what a replay record needs to show.
///
/// # Errors
///
/// Returns [`Malformed`](turnframe_provider::error::ProviderErrorKind::Malformed)
/// when the body carries no message at all — a successful status with nothing
/// in it is not an empty answer, it is a body this adapter cannot read.
pub(crate) fn build_response(
    body: &ChatResponse,
    request_id: RequestId,
    provider: &ProviderKey,
    model: &ModelKey,
) -> Result<ModelResponse, ProviderError> {
    let mut warnings = Vec::new();
    let Some(message) = body.message.as_ref() else {
        return Err(ProviderError::malformed("no_message"));
    };
    let reported_model = body
        .model
        .as_deref()
        .filter(|reported| !reported.is_empty())
        .map_or_else(|| model.clone(), ModelKey::from);

    let mut content = Vec::with_capacity(message.tool_calls.len() + 1);
    if let Some(text) = message.content.as_deref().filter(|text| !text.is_empty()) {
        content.push(ContentPart::text(text));
    }
    if message
        .thinking
        .as_deref()
        .is_some_and(|thinking| !thinking.is_empty())
    {
        // A reasoning trace is not the answer, and folding it into the text
        // would corrupt a structured payload. It is dropped, and said so.
        warnings.push(ResponseWarning::FeatureDropped {
            feature: "thinking".to_owned(),
        });
    }
    for (index, call) in message.tool_calls.iter().enumerate() {
        content.push(ContentPart::ToolCall(ToolCall::new(
            synthesized_call_id(index),
            call.function.name.clone().unwrap_or_default(),
            tool_arguments(&call.function),
        )));
    }
    if !message.tool_calls.is_empty() {
        warnings.push(ResponseWarning::SynthesizedCallIds);
    }

    let finish = finish_reason(
        body.done_reason.as_deref(),
        !message.tool_calls.is_empty(),
        &mut warnings,
    );
    let usage = body.usage();
    if usage.is_unreported() {
        warnings.push(ResponseWarning::UsageUnreported);
    }

    let mut response = ModelResponse::new(request_id, provider.clone(), reported_model)
        .with_finish(finish)
        .with_usage(usage);
    response.content = content;
    response.warnings = warnings;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turnframe_provider::ids::{ModelKey, ProviderKey, RequestId};

    fn decode(body: Value) -> ChatResponse {
        serde_json::from_value(body).expect("the envelope is tolerant")
    }

    fn build(body: Value) -> ModelResponse {
        let decoded = decode(body);
        build_response(
            &decoded,
            RequestId::nil(),
            &ProviderKey::from("ollama"),
            &ModelKey::from("qwen3:8b"),
        )
        .expect("builds")
    }

    fn answer(content: &str) -> Value {
        json!({
            "model": "qwen3:8b",
            "created_at": "2026-09-05T10:00:00.000000Z",
            "message": {"role": "assistant", "content": content},
            "done": true,
            "done_reason": "stop",
            "total_duration": 5_191_566_416_u64,
            "prompt_eval_count": 42,
            "eval_count": 7
        })
    }

    #[test]
    fn a_whole_answer_becomes_a_normalized_response() {
        let response = build(answer("Ho preparato la modifica."));
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert_eq!(response.finish, FinishReason::Stop);
        assert_eq!(response.usage, TokenUsage::new(42, 7));
        assert_eq!(response.model.as_str(), "qwen3:8b");
        // A local runtime has no response id to report, so none is invented.
        assert!(response.raw_id.is_none());
    }

    #[test]
    fn a_local_runtime_reports_no_cached_tokens() {
        let response = build(answer("ciao"));
        assert_eq!(response.usage.cached_input, 0);
        assert_eq!(response.usage.reasoning, 0);
        assert_eq!(response.usage.total(), 49);
    }

    #[test]
    fn missing_counts_are_reported_as_unreported_rather_than_guessed() {
        let response = build(json!({
            "message": {"role": "assistant", "content": "ciao"},
            "done": true,
            "done_reason": "stop"
        }));
        assert!(response.usage.is_unreported());
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::UsageUnreported)
        );
    }

    #[test]
    fn tool_calls_get_synthesized_ids_and_say_so() {
        let response = build(json!({
            "model": "qwen3:8b",
            "message": {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "load_case", "arguments": {"target": "tok_1"}}},
                {"function": {"name": "load_case", "arguments": {"target": "tok_2"}}}
            ]},
            "done": true,
            "done_reason": "stop"
        }));
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_str(), "call_0");
        assert_eq!(calls[1].id.as_str(), "call_1");
        assert_eq!(calls[0].arguments, json!({"target": "tok_1"}));
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::SynthesizedCallIds)
        );
        // `stop` with calls present is a turn that ended to call something.
        assert_eq!(response.finish, FinishReason::ToolCalls);
    }

    #[test]
    fn a_call_without_arguments_is_an_empty_object_not_a_null() {
        let response = build(json!({
            "message": {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "now", "arguments": null}}
            ]},
            "done": true
        }));
        assert_eq!(response.tool_calls()[0].arguments, json!({}));
    }

    #[test]
    fn finish_reasons_map_and_an_unknown_one_is_never_guessed_at_stop() {
        let truncated = build(json!({
            "message": {"role": "assistant", "content": "meta"},
            "done": true, "done_reason": "length"
        }));
        assert_eq!(truncated.finish, FinishReason::MaxTokens);
        assert!(!truncated.finish.is_complete());

        let loaded = build(json!({
            "message": {"role": "assistant", "content": ""},
            "done": true, "done_reason": "unload"
        }));
        assert_eq!(loaded.finish, FinishReason::Other);
        assert!(loaded.warnings.iter().any(|warning| matches!(
            warning,
            ResponseWarning::UnknownFinishReason { reported } if reported == "unload"
        )));
    }

    #[test]
    fn a_reasoning_trace_is_dropped_rather_than_folded_into_the_answer() {
        let response = build(json!({
            "message": {
                "role": "assistant",
                "content": "{\"acts\": []}",
                "thinking": "the user probably means the due date"
            },
            "done": true, "done_reason": "stop"
        }));
        assert_eq!(response.text(), "{\"acts\": []}");
        assert!(!response.text().contains("probably"));
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "thinking".to_owned()
                })
        );
    }

    #[test]
    fn an_empty_content_is_an_empty_response_not_an_empty_plan() {
        let response = build(answer(""));
        assert!(response.content.is_empty());
        assert!(response.is_empty());
        assert!(response.single_json().is_err());
    }

    #[test]
    fn a_body_with_no_message_is_malformed() {
        let decoded = decode(json!({"model": "qwen3:8b", "done": true}));
        let error = build_response(
            &decoded,
            RequestId::nil(),
            &ProviderKey::from("ollama"),
            &ModelKey::from("qwen3:8b"),
        )
        .expect_err("nothing to read");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("no_message".to_owned())
        );
    }

    #[test]
    fn an_unknown_envelope_field_does_not_reject_a_good_answer() {
        // The daemon grows fields between releases; the strictness invariant
        // I18 needs is applied to the model's payload, one layer up.
        let response = build(json!({
            "model": "qwen3:8b",
            "message": {"role": "assistant", "content": "ok"},
            "done": true,
            "done_reason": "stop",
            "a_field_from_a_newer_daemon": {"nested": true}
        }));
        assert_eq!(response.text(), "ok");
    }
}
