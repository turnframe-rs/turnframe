//! The normalized model response (spec §20.1).
//!
//! A [`ModelResponse`] is what every adapter returns, whatever the vendor sent:
//! the same [`ContentPart`] vocabulary the request uses, a
//! [`FinishReason`], reported [`TokenUsage`], and the warnings the adapter
//! wants the caller to know about.
//!
//! Three helpers cover the ways the runtime reads a response:
//!
//! * [`text`](ModelResponse::text) concatenates the prose;
//! * [`tool_calls`](ModelResponse::tool_calls) lists the calls in order;
//! * [`single_json`](ModelResponse::single_json) extracts the **one** JSON
//!   document a structured stage expects, and fails rather than choosing when
//!   there is more than one candidate (spec I18).
//!
//! A response is untrusted input (I9). Nothing here validates it against a
//! schema — that is [`structured`](crate::structured)'s job, and it is
//! all-or-nothing.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::{CallId, ModelKey, ModelRef, ProviderKey, RequestId};
use crate::request::{ContentPart, ToolCall};
use crate::structured::StructuredOutputError;

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    /// The model finished on its own or hit a stop sequence.
    Stop,
    /// The output token cap was reached. The answer is truncated, so a
    /// structured stage must reject it rather than parse the prefix.
    MaxTokens,
    /// The model ended its turn with tool calls.
    ToolCalls,
    /// A safety filter stopped the generation.
    ContentFilter,
    /// The model declined. A semantic outcome, distinguishable from a
    /// transport failure or a malformed body.
    Refusal,
    /// The provider reported something this crate does not model. Recorded as
    /// a [`ResponseWarning::UnknownFinishReason`] too.
    Other,
}

impl FinishReason {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::MaxTokens => "max_tokens",
            Self::ToolCalls => "tool_calls",
            Self::ContentFilter => "content_filter",
            Self::Refusal => "refusal",
            Self::Other => "other",
        }
    }

    /// Returns `true` when the answer is complete enough to be parsed.
    ///
    /// A truncated, filtered or refused answer never is: parsing its prefix is
    /// exactly the partial-execution failure I18 forbids.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Stop | Self::ToolCalls)
    }
}

impl fmt::Display for FinishReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Tokens the provider reported for a call.
///
/// Zero means "not reported": no provider distinguishes a real zero from a
/// missing count, and a cost estimate built on a guess is worse than one built
/// on a zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsage {
    /// Prompt tokens.
    pub input: u64,
    /// Generated tokens.
    pub output: u64,
    /// Prompt tokens served from the provider's cache, already counted in
    /// `input`.
    #[serde(default)]
    pub cached_input: u64,
    /// Reasoning tokens, already counted in `output` where the provider bills
    /// them that way.
    #[serde(default)]
    pub reasoning: u64,
}

impl TokenUsage {
    /// Usage with input and output counts only.
    #[must_use]
    pub const fn new(input: u64, output: u64) -> Self {
        Self {
            input,
            output,
            cached_input: 0,
            reasoning: 0,
        }
    }

    /// Nothing reported.
    #[must_use]
    pub const fn none() -> Self {
        Self::new(0, 0)
    }

    /// Records cached prompt tokens.
    #[must_use]
    pub const fn with_cached_input(mut self, cached: u64) -> Self {
        self.cached_input = cached;
        self
    }

    /// Records reasoning tokens.
    #[must_use]
    pub const fn with_reasoning(mut self, reasoning: u64) -> Self {
        self.reasoning = reasoning;
        self
    }

    /// Input plus output.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.input.saturating_add(self.output)
    }

    /// Returns `true` when the provider reported nothing.
    #[must_use]
    pub const fn is_unreported(self) -> bool {
        self.input == 0 && self.output == 0
    }
}

/// Something an adapter wants the caller to know without failing the call.
///
/// Warnings are the honest channel for "I did the job, but not exactly the way
/// you asked": they belong in a replay record, and a stage that cares may
/// refuse a response that carries one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResponseWarning {
    /// The provider did not return usable tool-call ids and the adapter
    /// synthesized them. The profile must declare
    /// [`preserves_call_ids = false`](crate::capabilities::ProviderCapabilities::preserves_call_ids).
    SynthesizedCallIds,
    /// The provider reported a finish reason this crate does not model.
    UnknownFinishReason {
        /// The provider's own label, sanitized to a short code.
        reported: String,
    },
    /// The provider did not report token usage.
    UsageUnreported,
    /// The adapter dropped a request feature the provider cannot express
    /// (a stop sequence beyond the vendor limit, a cache hint, a tool-choice
    /// mode). Never used for structured output: an adapter that cannot enforce
    /// a schema declares a weaker capability instead of warning about it.
    FeatureDropped {
        /// Which feature, as a short code.
        feature: String,
    },
    /// The answer was rebuilt from a stream rather than received whole.
    Reconstructed,
}

/// One model answer (spec §20.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelResponse {
    /// The request this answers. Equal to
    /// [`ModelRequest::request_id`](crate::request::ModelRequest::request_id)
    /// even after a retry.
    pub request_id: RequestId,
    /// Which provider answered.
    pub provider: ProviderKey,
    /// Which model answered. May differ from the requested key when the
    /// provider resolved an alias; the *reported* model is recorded.
    pub model: ModelKey,
    /// The answer, in order.
    pub content: Vec<ContentPart>,
    /// Why generation stopped.
    pub finish: FinishReason,
    /// Reported tokens.
    #[serde(default)]
    pub usage: TokenUsage,
    /// The provider's own response identifier, for support tickets. Never a
    /// body and never a header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_id: Option<String>,
    /// Wall-clock duration of the call, measured by the adapter.
    pub latency: Duration,
    /// Adapter warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ResponseWarning>,
}

impl ModelResponse {
    /// A response with no content, for adapters to build on.
    #[must_use]
    pub fn new(
        request_id: RequestId,
        provider: impl Into<ProviderKey>,
        model: impl Into<ModelKey>,
    ) -> Self {
        Self {
            request_id,
            provider: provider.into(),
            model: model.into(),
            content: Vec::new(),
            finish: FinishReason::Stop,
            usage: TokenUsage::none(),
            raw_id: None,
            latency: Duration::ZERO,
            warnings: Vec::new(),
        }
    }

    /// Appends a content part.
    #[must_use]
    pub fn with_part(mut self, part: ContentPart) -> Self {
        self.content.push(part);
        self
    }

    /// Appends a text part.
    #[must_use]
    pub fn with_text(self, text: impl Into<String>) -> Self {
        self.with_part(ContentPart::text(text))
    }

    /// Appends a tool call.
    #[must_use]
    pub fn with_tool_call(self, call: ToolCall) -> Self {
        self.with_part(ContentPart::ToolCall(call))
    }

    /// Sets the finish reason.
    #[must_use]
    pub const fn with_finish(mut self, finish: FinishReason) -> Self {
        self.finish = finish;
        self
    }

    /// Sets the reported usage.
    #[must_use]
    pub const fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.usage = usage;
        self
    }

    /// Sets the provider's response identifier.
    #[must_use]
    pub fn with_raw_id(mut self, raw_id: impl Into<String>) -> Self {
        self.raw_id = Some(raw_id.into());
        self
    }

    /// Sets the measured latency.
    #[must_use]
    pub const fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Adds a warning.
    #[must_use]
    pub fn with_warning(mut self, warning: ResponseWarning) -> Self {
        self.warnings.push(warning);
        self
    }

    /// The provider-model pair that answered.
    #[must_use]
    pub fn reference(&self) -> ModelRef {
        ModelRef {
            provider: self.provider.clone(),
            model: self.model.clone(),
        }
    }

    /// Every text part, concatenated in order.
    ///
    /// ```
    /// use turnframe_provider::prelude::*;
    ///
    /// let response = ModelResponse::new(RequestId::nil(), "openai", "gpt-4o")
    ///     .with_text("Ho preparato ")
    ///     .with_text("la modifica.");
    /// assert_eq!(response.text(), "Ho preparato la modifica.");
    /// ```
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        for part in &self.content {
            if let Some(text) = part.as_text() {
                out.push_str(text);
            }
        }
        out
    }

    /// Every tool call, in the order the model produced them.
    #[must_use]
    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.content
            .iter()
            .filter_map(ContentPart::as_tool_call)
            .collect()
    }

    /// The call with this id, when present.
    #[must_use]
    pub fn tool_call(&self, id: &CallId) -> Option<&ToolCall> {
        self.tool_calls().into_iter().find(|call| &call.id == id)
    }

    /// Returns `true` when the model produced neither text nor a tool call.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tool_calls().is_empty() && self.text().trim().is_empty()
    }

    /// Extracts the single JSON document a structured stage expects.
    ///
    /// The rule is deterministic and refuses to guess:
    ///
    /// 1. A [`FinishReason::Refusal`] is a
    ///    [`Refusal`](StructuredOutputError::Refusal), never a parse attempt.
    /// 2. A finish reason that is not
    ///    [complete](FinishReason::is_complete) — truncation, a content filter —
    ///    is a [`NoOutput`](StructuredOutputError::NoOutput): the bytes that
    ///    arrived are a prefix, and parsing a prefix is what I18 forbids.
    /// 3. **Exactly one** tool call: its `arguments` are the document. The
    ///    surrounding prose is a preamble and is ignored, because a provider
    ///    using a function schema as a transport routinely emits both.
    /// 4. **More than one** tool call:
    ///    [`MultipleCandidates`](StructuredOutputError::MultipleCandidates). A
    ///    stage that expects one document never picks one of several.
    /// 5. **No tool call**: the concatenated text is parsed. A single
    ///    ```` ```json ```` fence around it is stripped first — a fence is
    ///    framing a prompt-only transport adds, not content.
    ///
    /// # Errors
    ///
    /// See [`StructuredOutputError`].
    pub fn single_json(&self) -> Result<serde_json::Value, StructuredOutputError> {
        if self.finish == FinishReason::Refusal {
            return Err(StructuredOutputError::Refusal);
        }
        let calls = self.tool_calls();
        if calls.len() > 1 {
            return Err(StructuredOutputError::MultipleCandidates {
                candidates: calls.len(),
            });
        }
        if !self.finish.is_complete() {
            return Err(StructuredOutputError::NoOutput);
        }
        if let Some(call) = calls.first() {
            return Ok(call.arguments.clone());
        }
        let text = self.text();
        let payload = strip_code_fence(text.trim());
        if payload.is_empty() {
            return Err(StructuredOutputError::NoOutput);
        }
        serde_json::from_str(payload).map_err(StructuredOutputError::not_json)
    }
}

/// Removes a single Markdown code fence around `text`, if that is all it is.
///
/// Only a fence that opens on the first line and closes on the last is removed,
/// so JSON containing a fenced string is untouched.
fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let Some(body_start) = rest.find('\n') else {
        return text;
    };
    // The opening line may carry a language tag and nothing else.
    if rest[..body_start]
        .chars()
        .any(|ch| !ch.is_ascii_alphanumeric())
    {
        return text;
    }
    let body = &rest[body_start + 1..];
    match body.trim_end().strip_suffix("```") {
        Some(inner) => inner.trim(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response() -> ModelResponse {
        ModelResponse::new(RequestId::nil(), "openai", "gpt-4o")
    }

    #[test]
    fn helpers_read_text_and_calls_in_order() {
        let built = response()
            .with_text("first ")
            .with_tool_call(ToolCall::new("call_a", "plan", json!({"n": 1})))
            .with_text("second")
            .with_tool_call(ToolCall::new("call_b", "plan", json!({"n": 2})));
        assert_eq!(built.text(), "first second");
        let calls = built.tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_str(), "call_a");
        assert_eq!(calls[1].id.as_str(), "call_b");
        assert!(built.tool_call(&CallId::from("call_b")).is_some());
        assert!(built.tool_call(&CallId::from("call_z")).is_none());
        assert!(!built.is_empty());
        assert_eq!(built.reference().to_string(), "openai/gpt-4o");
    }

    #[test]
    fn single_json_takes_the_only_tool_call_and_ignores_the_preamble() {
        let built = response()
            .with_text("Certo, ecco il piano.")
            .with_tool_call(ToolCall::new("call_a", "plan", json!({"acts": []})))
            .with_finish(FinishReason::ToolCalls);
        assert_eq!(built.single_json().unwrap(), json!({"acts": []}));
    }

    #[test]
    fn single_json_refuses_to_choose_between_two_calls() {
        let built = response()
            .with_tool_call(ToolCall::new("a", "plan", json!({"n": 1})))
            .with_tool_call(ToolCall::new("b", "plan", json!({"n": 2})))
            .with_finish(FinishReason::ToolCalls);
        assert!(matches!(
            built.single_json(),
            Err(StructuredOutputError::MultipleCandidates { candidates: 2 })
        ));
    }

    #[test]
    fn single_json_parses_text_and_strips_one_fence() {
        let plain = response().with_text(" {\"a\": 1} ");
        assert_eq!(plain.single_json().unwrap(), json!({"a": 1}));

        let fenced = response().with_text("```json\n{\"a\": 1}\n```");
        assert_eq!(fenced.single_json().unwrap(), json!({"a": 1}));

        let bare_fence = response().with_text("```\n{\"a\": 1}\n```");
        assert_eq!(bare_fence.single_json().unwrap(), json!({"a": 1}));

        // A fence-looking prefix that is not a fence is left alone and fails.
        let not_a_fence = response().with_text("```json {\"a\": 1}");
        assert!(matches!(
            not_a_fence.single_json(),
            Err(StructuredOutputError::NotJson { .. })
        ));
    }

    #[test]
    fn single_json_never_parses_an_incomplete_answer() {
        let truncated = response()
            .with_text("{\"a\": 1")
            .with_finish(FinishReason::MaxTokens);
        assert!(matches!(
            truncated.single_json(),
            Err(StructuredOutputError::NoOutput)
        ));

        let filtered = response()
            .with_text("{\"a\": 1}")
            .with_finish(FinishReason::ContentFilter);
        assert!(matches!(
            filtered.single_json(),
            Err(StructuredOutputError::NoOutput)
        ));

        let refused = response()
            .with_text("I cannot help with that.")
            .with_finish(FinishReason::Refusal);
        assert!(matches!(
            refused.single_json(),
            Err(StructuredOutputError::Refusal)
        ));

        let empty = response();
        assert!(empty.is_empty());
        assert!(matches!(
            empty.single_json(),
            Err(StructuredOutputError::NoOutput)
        ));
    }

    #[test]
    fn finish_reasons_say_whether_output_is_complete() {
        assert!(FinishReason::Stop.is_complete());
        assert!(FinishReason::ToolCalls.is_complete());
        for incomplete in [
            FinishReason::MaxTokens,
            FinishReason::ContentFilter,
            FinishReason::Refusal,
            FinishReason::Other,
        ] {
            assert!(!incomplete.is_complete(), "{incomplete}");
        }
    }

    #[test]
    fn usage_totals_and_reports_absence() {
        assert!(TokenUsage::none().is_unreported());
        let usage = TokenUsage::new(100, 20)
            .with_cached_input(80)
            .with_reasoning(5);
        assert_eq!(usage.total(), 120);
        assert_eq!(usage.cached_input, 80);
        assert_eq!(usage.reasoning, 5);
        assert!(!usage.is_unreported());
    }

    #[test]
    fn responses_round_trip_through_serde() {
        let built = response()
            .with_text("hi")
            .with_tool_call(ToolCall::new("c1", "plan", json!({})))
            .with_finish(FinishReason::ToolCalls)
            .with_usage(TokenUsage::new(10, 3))
            .with_raw_id("resp_123")
            .with_latency(Duration::from_millis(420))
            .with_warning(ResponseWarning::Reconstructed);
        let json = serde_json::to_string(&built).unwrap();
        let back: ModelResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, built);
    }
}
