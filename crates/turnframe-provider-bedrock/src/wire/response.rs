//! Converse output → normalized response.
//!
//! The SDK has already decoded the wire, so this module has one job: turn the
//! Converse vocabulary into the normalized one without inventing anything.
//!
//! # What it refuses, and what it merely reports
//!
//! An empty `content` array is an empty answer, and a block Converse grew after
//! this adapter was written — a reasoning block, a citation block — is dropped
//! with a [`FeatureDropped`](ResponseWarning::FeatureDropped) warning. A stop
//! reason this crate does not model becomes [`FinishReason::Other`], which a
//! structured stage refuses, never a guess at [`FinishReason::Stop`].
//!
//! # The stop reasons that are not obvious
//!
//! | Converse | Normalized | Why |
//! |---|---|---|
//! | `end_turn`, `stop_sequence` | [`Stop`](FinishReason::Stop) | |
//! | `max_tokens`, `model_context_window_exceeded` | [`MaxTokens`](FinishReason::MaxTokens) | the answer is a prefix either way |
//! | `tool_use` | [`ToolCalls`](FinishReason::ToolCalls) | |
//! | `content_filtered`, `guardrail_intervened` | [`ContentFilter`](FinishReason::ContentFilter) | a filter stopped the generation; the model did not decline |
//! | `refusal` | [`Refusal`](FinishReason::Refusal) | the model declined — a semantic outcome |
//! | `malformed_model_output`, `malformed_tool_use` | [`Other`](FinishReason::Other) | the output exists and is unusable; a structured stage must reject it |
//!
//! `refusal` is the interesting row. Converse's stop-reason set is **open** —
//! the SDK models it with an `Unknown` variant precisely because Bedrock fronts
//! several model families and passes their own labels through — and a model
//! family that declines spells it `refusal`. Recognizing that label by name
//! keeps a refusal distinguishable from malformed output, which is exactly the
//! distinction spec §20.8 asks an adapter to preserve. Flattening it into
//! `Other` would tell a stage "something we do not model happened" when the
//! provider said something precise.
//!
//! # Usage, and what "input" means
//!
//! Converse reports `inputTokens` **excluding** the two cache counters.
//! [`TokenUsage`] documents the opposite: `cached_input` is already counted in
//! `input`. So the normalization adds both cache counters back into `input` and
//! reports only the *read* half as `cached_input` — write tokens filled a
//! cache rather than being served from one, and they cost more rather than
//! less.

use aws_sdk_bedrockruntime::operation::converse::ConverseOutput as ConverseResponse;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConverseOutput as ConverseBody, StopReason, TokenUsage as WireUsage,
};
use turnframe_provider::error::{ErrorCode, ProviderError};
use turnframe_provider::ids::{CallId, ModelKey, ProviderKey, RequestId};
use turnframe_provider::request::{ContentPart, ToolCall};
use turnframe_provider::response::{FinishReason, ModelResponse, ResponseWarning, TokenUsage};

use crate::wire::document::from_document;

/// The stop-reason label a declining model family sends through Converse.
pub(crate) const REFUSAL_STOP_REASON: &str = "refusal";

/// Normalizes the counters Converse reports.
pub(crate) fn normalize_usage(usage: &WireUsage) -> TokenUsage {
    let count = |value: i32| u64::try_from(value).unwrap_or(0);
    let read = usage.cache_read_input_tokens.map_or(0, count);
    let written = usage.cache_write_input_tokens.map_or(0, count);
    let input = count(usage.input_tokens)
        .saturating_add(read)
        .saturating_add(written);
    TokenUsage::new(input, count(usage.output_tokens)).with_cached_input(read)
}

/// Maps a Converse stop reason onto the normalized one.
pub(crate) fn finish_reason(
    reported: &StopReason,
    warnings: &mut Vec<ResponseWarning>,
) -> FinishReason {
    match reported {
        StopReason::EndTurn | StopReason::StopSequence => FinishReason::Stop,
        StopReason::MaxTokens | StopReason::ModelContextWindowExceeded => FinishReason::MaxTokens,
        StopReason::ToolUse => FinishReason::ToolCalls,
        StopReason::ContentFiltered | StopReason::GuardrailIntervened => {
            FinishReason::ContentFilter
        }
        other if other.as_str() == REFUSAL_STOP_REASON => FinishReason::Refusal,
        other => {
            warnings.push(ResponseWarning::UnknownFinishReason {
                reported: ErrorCode::new(other.as_str()).as_str().to_owned(),
            });
            FinishReason::Other
        }
    }
}

/// Builds the normalized response from a decoded Converse output.
///
/// `provider` and `model` are the *configured* keys: Converse echoes no model
/// identifier in its response, so the configured one is the only honest label.
///
/// Every text block is concatenated into **one** leading text part and the tool
/// calls follow in order — the same shape
/// [`StreamAccumulator`](turnframe_provider::stream::StreamAccumulator)
/// produces, which is what makes the two paths comparable (spec §20.8).
///
/// # Errors
///
/// Returns [`Malformed`](turnframe_provider::error::ProviderErrorKind::Malformed)
/// for a `toolUse` block with no name: a call the runtime cannot route is not a
/// call it may guess at.
pub(crate) fn build_response(
    output: &ConverseResponse,
    request_id: RequestId,
    provider: &ProviderKey,
    model: &ModelKey,
    preserves_call_ids: bool,
    raw_id: Option<&str>,
) -> Result<ModelResponse, ProviderError> {
    let mut warnings = Vec::new();
    let finish = finish_reason(&output.stop_reason, &mut warnings);

    let mut text = String::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    let mut synthesized = false;
    let mut unsupported = false;
    if let Some(ConverseBody::Message(message)) = output.output.as_ref() {
        for block in &message.content {
            match block {
                ContentBlock::Text(fragment) => text.push_str(fragment),
                ContentBlock::ToolUse(call) => {
                    if call.name.is_empty() {
                        return Err(ProviderError::malformed("tool_use_without_name"));
                    }
                    let id = if call.tool_use_id.is_empty() {
                        synthesized = true;
                        CallId::new(format!("call_{}", calls.len()))
                    } else {
                        CallId::new(&call.tool_use_id)
                    };
                    calls.push(ToolCall::new(id, &call.name, from_document(&call.input)));
                }
                // A block Converse grew after this adapter was written. Dropped
                // and reported, never guessed at.
                _ => unsupported = true,
            }
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

    let usage = output
        .usage
        .as_ref()
        .map(normalize_usage)
        .unwrap_or_default();
    if usage.is_unreported() {
        warnings.push(ResponseWarning::UsageUnreported);
    }

    let mut response = ModelResponse::new(request_id, provider.clone(), model.clone())
        .with_finish(finish)
        .with_usage(usage);
    response.content = content;
    response.warnings = warnings;
    if let Some(id) = raw_id.filter(|id| !id.is_empty()) {
        response = response.with_raw_id(id);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_bedrockruntime::types::{
        ConversationRole, ImageBlock, ImageFormat, ImageSource, Message, ToolUseBlock,
    };
    use aws_smithy_types::{Blob, Document};
    use serde_json::json;

    fn message(content: Vec<ContentBlock>) -> ConverseBody {
        ConverseBody::Message(
            Message::builder()
                .role(ConversationRole::Assistant)
                .set_content(Some(content))
                .build()
                .expect("a role is set"),
        )
    }

    fn output(body: ConverseBody, stop: StopReason) -> ConverseResponse {
        ConverseResponse::builder()
            .output(body)
            .stop_reason(stop)
            .usage(
                WireUsage::builder()
                    .input_tokens(10)
                    .output_tokens(4)
                    .total_tokens(19)
                    .cache_read_input_tokens(5)
                    .build()
                    .expect("the counters are set"),
            )
            .build()
            .expect("a stop reason is set")
    }

    fn normalized(output: &ConverseResponse) -> ModelResponse {
        build_response(
            output,
            RequestId::nil(),
            &ProviderKey::from("bedrock"),
            &ModelKey::from("model"),
            true,
            Some("req-1"),
        )
        .expect("a well-formed output")
    }

    #[test]
    fn text_blocks_become_one_leading_text_part() {
        let body = message(vec![
            ContentBlock::Text("Ho preparato ".to_owned()),
            ContentBlock::Text("la modifica.".to_owned()),
        ]);
        let response = normalized(&output(body, StopReason::EndTurn));
        assert_eq!(response.content.len(), 1);
        assert_eq!(response.text(), "Ho preparato la modifica.");
        assert_eq!(response.finish, FinishReason::Stop);
        assert_eq!(response.raw_id.as_deref(), Some("req-1"));
    }

    #[test]
    fn a_tool_call_keeps_its_id_name_and_arguments() {
        let call = ToolUseBlock::builder()
            .tool_use_id("tooluse_1")
            .name("read")
            .input(Document::Object(
                [("target".to_owned(), Document::String("tok_1".to_owned()))]
                    .into_iter()
                    .collect(),
            ))
            .build()
            .expect("every field is set");
        let response = normalized(&output(
            message(vec![ContentBlock::ToolUse(call)]),
            StopReason::ToolUse,
        ));
        let calls = response.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "tooluse_1");
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments, json!({"target": "tok_1"}));
        assert_eq!(response.finish, FinishReason::ToolCalls);
    }

    #[test]
    fn cache_counters_are_folded_back_into_the_prompt_total() {
        let response = normalized(&output(message(vec![]), StopReason::EndTurn));
        assert_eq!(response.usage.input, 15);
        assert_eq!(response.usage.cached_input, 5);
        assert_eq!(response.usage.output, 4);
    }

    #[test]
    fn a_filter_a_refusal_and_an_unknown_label_are_three_different_answers() {
        let mut warnings = Vec::new();
        assert_eq!(
            finish_reason(&StopReason::GuardrailIntervened, &mut warnings),
            FinishReason::ContentFilter
        );
        assert_eq!(
            finish_reason(&StopReason::ContentFiltered, &mut warnings),
            FinishReason::ContentFilter
        );
        assert!(warnings.is_empty());
        assert_eq!(
            finish_reason(&StopReason::from(REFUSAL_STOP_REASON), &mut warnings),
            FinishReason::Refusal
        );
        assert!(warnings.is_empty(), "a refusal is modelled, not unknown");
        assert_eq!(
            finish_reason(&StopReason::MalformedToolUse, &mut warnings),
            FinishReason::Other
        );
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn a_call_without_a_name_is_malformed_rather_than_guessed_at() {
        let call = ToolUseBlock::builder()
            .tool_use_id("tooluse_1")
            .name("")
            .input(Document::Null)
            .build()
            .expect("the fields are set, even if empty");
        let error = build_response(
            &output(
                message(vec![ContentBlock::ToolUse(call)]),
                StopReason::ToolUse,
            ),
            RequestId::nil(),
            &ProviderKey::from("bedrock"),
            &ModelKey::from("model"),
            true,
            None,
        )
        .expect_err("a nameless call is unroutable");
        assert_eq!(error.kind().as_str(), "malformed");
    }

    #[test]
    fn a_block_this_adapter_does_not_forward_is_dropped_and_reported() {
        // An image block in an *answer*: a real Converse shape this adapter has
        // no normalized part for, and the stand-in for anything the API grows
        // next.
        let image = ImageBlock::builder()
            .format(ImageFormat::Png)
            .source(ImageSource::Bytes(Blob::new(b"not-really-a-png".to_vec())))
            .build()
            .expect("the format is set");
        let body = message(vec![
            ContentBlock::Text("prima".to_owned()),
            ContentBlock::Image(image),
        ]);
        let response = normalized(&output(body, StopReason::EndTurn));
        assert_eq!(response.text(), "prima");
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::FeatureDropped {
                    feature: "unsupported_content_block".to_owned()
                })
        );
    }

    #[test]
    fn an_answer_with_no_usage_says_so() {
        let bare = ConverseResponse::builder()
            .output(message(vec![]))
            .stop_reason(StopReason::EndTurn)
            .build()
            .expect("a stop reason is set");
        let response = normalized(&bare);
        assert!(
            response
                .warnings
                .contains(&ResponseWarning::UsageUnreported)
        );
    }
}
