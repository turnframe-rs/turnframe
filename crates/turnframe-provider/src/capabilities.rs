//! Capability model (spec §20.3) and per-model configuration profiles.
//!
//! Capabilities are **configured** per provider-model pair in a
//! [`ModelProfile`], never inferred from the brand: the same vendor ships
//! models with and without native JSON Schema support, and a gateway may strip
//! features. [`CapabilityRequirements::satisfied_by`] is the single place where
//! a requirement is checked against a declaration; routing and fallback both
//! call it, and a mismatch is reported, never papered over.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{ModelKey, ModelRef, ProviderKey};

/// How a model can be made to emit structured output (spec §20.3).
///
/// Ordered from strongest to weakest guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputCapability {
    /// The provider enforces a supplied JSON Schema on the output.
    NativeJsonSchema,
    /// The provider enforces the schema of a declared function; the call is
    /// used only as a transport for the structured payload, never executed.
    NativeFunctionSchema,
    /// Constrained decoding against a grammar derived from the schema.
    GrammarConstrained,
    /// The provider guarantees syntactically valid JSON but not the schema.
    JsonObject,
    /// The schema is only described in the prompt.
    PromptOnly,
    /// No structured-output support at all.
    None,
}

impl StructuredOutputCapability {
    /// Every capability, strongest first.
    pub const ALL: [Self; 6] = [
        Self::NativeJsonSchema,
        Self::NativeFunctionSchema,
        Self::GrammarConstrained,
        Self::JsonObject,
        Self::PromptOnly,
        Self::None,
    ];

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeJsonSchema => "native_json_schema",
            Self::NativeFunctionSchema => "native_function_schema",
            Self::GrammarConstrained => "grammar_constrained",
            Self::JsonObject => "json_object",
            Self::PromptOnly => "prompt_only",
            Self::None => "none",
        }
    }

    /// Returns `true` when the provider enforces the *schema* (not just JSON
    /// syntax) on the wire.
    #[must_use]
    pub const fn enforces_schema(self) -> bool {
        matches!(
            self,
            Self::NativeJsonSchema | Self::NativeFunctionSchema | Self::GrammarConstrained
        )
    }
}

impl fmt::Display for StructuredOutputCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Tool-calling support of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallingCapability {
    /// No tool calling.
    None,
    /// At most one tool call per response.
    Sequential,
    /// Several tool calls per response.
    Parallel,
}

impl ToolCallingCapability {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sequential => "sequential",
            Self::Parallel => "parallel",
        }
    }
}

impl fmt::Display for ToolCallingCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Declared capabilities of one provider-model pair (spec §20.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Structured-output transport.
    pub structured_output: StructuredOutputCapability,
    /// Tool-calling support.
    pub tool_calling: ToolCallingCapability,
    /// Whether several tool calls may appear in one response.
    pub parallel_tool_calls: bool,
    /// Image input.
    pub vision: bool,
    /// Document input — a PDF, a spreadsheet, a plain-text attachment.
    ///
    /// Separate from [`vision`](Self::vision) on purpose: a model that reads a
    /// PNG may reject a PDF, and a gateway may strip one and not the other.
    /// Defaults to `false` when a stored declaration predates the flag, which
    /// is the fail-closed answer.
    #[serde(default)]
    pub documents: bool,
    /// Audio input.
    pub audio_input: bool,
    /// Audio output.
    pub audio_output: bool,
    /// Incremental output.
    pub streaming: bool,
    /// Prompt caching honoured by the provider.
    pub prompt_caching: bool,
    /// Reasoning effort or budget controls.
    pub reasoning_controls: bool,
    /// Whether a sampling temperature may be sent. Reasoning models that accept only
    /// their default declare `false`, and the adapter then drops the parameter.
    #[serde(default)]
    pub temperature: bool,
    /// Whether a sampling seed is honoured.
    #[serde(default)]
    pub seed: bool,
    /// Context window in tokens, when known.
    pub max_context_tokens: Option<u64>,
    /// Whether tool-call ids round-trip unchanged.
    pub preserves_call_ids: bool,
}

impl ProviderCapabilities {
    /// The weakest declaration: nothing supported, context unknown.
    #[must_use]
    pub const fn minimal() -> Self {
        Self {
            structured_output: StructuredOutputCapability::None,
            tool_calling: ToolCallingCapability::None,
            parallel_tool_calls: false,
            vision: false,
            documents: false,
            audio_input: false,
            audio_output: false,
            streaming: false,
            prompt_caching: false,
            reasoning_controls: false,
            temperature: false,
            seed: false,
            max_context_tokens: None,
            preserves_call_ids: false,
        }
    }

    /// Sets the structured-output transport.
    #[must_use]
    pub const fn with_structured_output(mut self, capability: StructuredOutputCapability) -> Self {
        self.structured_output = capability;
        self
    }

    /// Sets tool calling; `Parallel` also sets `parallel_tool_calls`.
    #[must_use]
    pub const fn with_tool_calling(mut self, capability: ToolCallingCapability) -> Self {
        self.tool_calling = capability;
        self.parallel_tool_calls = matches!(capability, ToolCallingCapability::Parallel);
        self
    }

    /// Sets streaming support.
    #[must_use]
    pub const fn with_streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Sets vision support.
    #[must_use]
    pub const fn with_vision(mut self, vision: bool) -> Self {
        self.vision = vision;
        self
    }

    /// Sets document-input support.
    #[must_use]
    pub const fn with_documents(mut self, documents: bool) -> Self {
        self.documents = documents;
        self
    }

    /// Sets the context window.
    #[must_use]
    pub const fn with_max_context_tokens(mut self, tokens: u64) -> Self {
        self.max_context_tokens = Some(tokens);
        self
    }

    /// Sets whether call ids round-trip.
    #[must_use]
    pub const fn with_preserves_call_ids(mut self, preserves: bool) -> Self {
        self.preserves_call_ids = preserves;
        self
    }

    /// Sets prompt caching support.
    #[must_use]
    pub const fn with_prompt_caching(mut self, caching: bool) -> Self {
        self.prompt_caching = caching;
        self
    }

    /// Sets whether reasoning effort may be sent.
    #[must_use]
    pub const fn with_reasoning_controls(mut self, controls: bool) -> Self {
        self.reasoning_controls = controls;
        self
    }

    /// Sets whether a sampling temperature may be sent.
    #[must_use]
    pub const fn with_temperature(mut self, temperature: bool) -> Self {
        self.temperature = temperature;
        self
    }

    /// Sets whether a sampling seed is honoured.
    #[must_use]
    pub const fn with_seed(mut self, seed: bool) -> Self {
        self.seed = seed;
        self
    }

    /// Returns `true` when any tool calling is available.
    #[must_use]
    pub const fn supports_tools(&self) -> bool {
        !matches!(self.tool_calling, ToolCallingCapability::None)
    }
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self::minimal()
    }
}

/// One capability a provider lacks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MissingCapability {
    /// The declared structured-output transport is not in the accepted set.
    StructuredOutput {
        /// Transports that would have been accepted.
        required: Vec<StructuredOutputCapability>,
        /// What the profile declares.
        declared: StructuredOutputCapability,
    },
    /// Tools are needed but the model declares none.
    ToolCalling,
    /// Streaming is needed but not declared.
    Streaming,
    /// Vision is needed but not declared.
    Vision,
    /// A document part is present but document input is not declared.
    Documents,
    /// The declared context window is unknown or too small.
    ContextWindow {
        /// Minimum required.
        required: u64,
        /// Declared, when known.
        declared: Option<u64>,
    },
}

impl fmt::Display for MissingCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StructuredOutput { required, declared } => {
                write!(f, "structured_output(declared {declared}, required one of ")?;
                for (index, capability) in required.iter().enumerate() {
                    if index > 0 {
                        f.write_str("|")?;
                    }
                    write!(f, "{capability}")?;
                }
                f.write_str(")")
            }
            Self::ToolCalling => f.write_str("tool_calling"),
            Self::Streaming => f.write_str("streaming"),
            Self::Vision => f.write_str("vision"),
            Self::Documents => f.write_str("documents"),
            Self::ContextWindow { required, declared } => match declared {
                Some(declared) => write!(
                    f,
                    "context_window(declared {declared}, required {required})"
                ),
                None => write!(f, "context_window(undeclared, required {required})"),
            },
        }
    }
}

/// A provider does not satisfy a set of requirements.
///
/// `Display` lists capability codes only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, thiserror::Error)]
pub struct CapabilityMismatch {
    /// Every unmet requirement, in check order.
    pub missing: Vec<MissingCapability>,
}

impl CapabilityMismatch {
    /// Returns `true` when the structured-output requirement is among the
    /// unmet ones: the case spec §0 rule 9 forbids downgrading.
    #[must_use]
    pub fn structured_output_unmet(&self) -> bool {
        self.missing
            .iter()
            .any(|missing| matches!(missing, MissingCapability::StructuredOutput { .. }))
    }
}

impl fmt::Display for CapabilityMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("capability mismatch: ")?;
        for (index, missing) in self.missing.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{missing}")?;
        }
        Ok(())
    }
}

/// What a request or purpose demands of a provider.
///
/// An empty `structured_output` set means "no structured-output requirement".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct CapabilityRequirements {
    /// Acceptable structured-output transports; empty means none required.
    pub structured_output: Vec<StructuredOutputCapability>,
    /// Tool calling is needed.
    pub needs_tools: bool,
    /// Streaming is needed.
    pub needs_streaming: bool,
    /// Minimum declared context window, in tokens.
    pub min_context_tokens: Option<u64>,
    /// Image input is needed.
    pub needs_vision: bool,
    /// Document input is needed.
    #[serde(default)]
    pub needs_documents: bool,
}

impl CapabilityRequirements {
    /// No requirement at all.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            structured_output: Vec::new(),
            needs_tools: false,
            needs_streaming: false,
            min_context_tokens: None,
            needs_vision: false,
            needs_documents: false,
        }
    }

    /// Requires tool calling.
    #[must_use]
    pub fn with_tools(mut self) -> Self {
        self.needs_tools = true;
        self
    }

    /// Requires streaming.
    #[must_use]
    pub fn with_streaming(mut self) -> Self {
        self.needs_streaming = true;
        self
    }

    /// Requires vision.
    #[must_use]
    pub fn with_vision(mut self) -> Self {
        self.needs_vision = true;
        self
    }

    /// Requires document input.
    #[must_use]
    pub fn with_documents(mut self) -> Self {
        self.needs_documents = true;
        self
    }

    /// Requires a minimum context window.
    #[must_use]
    pub fn with_min_context_tokens(mut self, tokens: u64) -> Self {
        self.min_context_tokens = Some(tokens);
        self
    }

    /// Checks the requirements against a declaration.
    ///
    /// Every unmet requirement is reported, in the order: structured output,
    /// tool calling, streaming, vision, documents, context window. An unknown
    /// context window fails a context requirement (fail closed).
    pub fn satisfied_by(&self, caps: &ProviderCapabilities) -> Result<(), CapabilityMismatch> {
        let mut missing = Vec::new();
        if !self.structured_output.is_empty()
            && !self.structured_output.contains(&caps.structured_output)
        {
            missing.push(MissingCapability::StructuredOutput {
                required: self.structured_output.clone(),
                declared: caps.structured_output,
            });
        }
        if self.needs_tools && !caps.supports_tools() {
            missing.push(MissingCapability::ToolCalling);
        }
        if self.needs_streaming && !caps.streaming {
            missing.push(MissingCapability::Streaming);
        }
        if self.needs_vision && !caps.vision {
            missing.push(MissingCapability::Vision);
        }
        if self.needs_documents && !caps.documents {
            missing.push(MissingCapability::Documents);
        }
        if let Some(required) = self.min_context_tokens {
            let ok = caps
                .max_context_tokens
                .is_some_and(|declared| declared >= required);
            if !ok {
                missing.push(MissingCapability::ContextWindow {
                    required,
                    declared: caps.max_context_tokens,
                });
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(CapabilityMismatch { missing })
        }
    }
}

/// Money in micro-cents (one cent = 1 000 000 micro-cents), integer so that
/// cost comparisons are exact and hashable.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct MicroCents(pub u64);

impl MicroCents {
    /// Micro-cents per cent.
    pub const PER_CENT: u64 = 1_000_000;

    /// Builds from whole cents (saturating).
    #[must_use]
    pub const fn from_cents(cents: u64) -> Self {
        Self(cents.saturating_mul(Self::PER_CENT))
    }

    /// Builds from whole dollars (saturating).
    #[must_use]
    pub const fn from_dollars(dollars: u64) -> Self {
        Self::from_cents(dollars.saturating_mul(100))
    }

    /// The raw value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Saturating addition.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl fmt::Display for MicroCents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}µ¢", self.0)
    }
}

/// Configured profile of one provider-model pair.
///
/// The profile is the single source of truth an adapter exposes through
/// [`crate::provider::ModelProvider::profile`]; routing reads capabilities,
/// cost, region and tags from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfile {
    /// Provider key.
    pub provider: ProviderKey,
    /// Model key.
    pub model: ModelKey,
    /// Declared capabilities.
    pub capabilities: ProviderCapabilities,
    /// Price per million input tokens, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_million_input: Option<MicroCents>,
    /// Price per million output tokens, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_million_output: Option<MicroCents>,
    /// Region or data-residency label the model runs in (e.g. `"eu"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Free-form configuration tags (e.g. `"cheap"`, `"eval-2026-08"`).
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ModelProfile {
    /// A profile with the given capabilities and no cost, region or tags.
    #[must_use]
    pub fn new(
        provider: impl Into<ProviderKey>,
        model: impl Into<ModelKey>,
        capabilities: ProviderCapabilities,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            capabilities,
            cost_per_million_input: None,
            cost_per_million_output: None,
            region: None,
            tags: Vec::new(),
        }
    }

    /// Sets both per-million prices.
    #[must_use]
    pub fn with_cost(mut self, input: MicroCents, output: MicroCents) -> Self {
        self.cost_per_million_input = Some(input);
        self.cost_per_million_output = Some(output);
        self
    }

    /// Sets the region label.
    #[must_use]
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Adds a tag.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// The provider-model reference.
    #[must_use]
    pub fn reference(&self) -> ModelRef {
        ModelRef {
            provider: self.provider.clone(),
            model: self.model.clone(),
        }
    }

    /// The higher of the two declared per-million prices, when both are known.
    ///
    /// Routing compares this against a cost ceiling; a profile with an unknown
    /// price does not pass a ceiling (fail closed).
    #[must_use]
    pub fn max_cost_per_million(&self) -> Option<MicroCents> {
        match (self.cost_per_million_input, self.cost_per_million_output) {
            (Some(input), Some(output)) => Some(input.max(output)),
            _ => None,
        }
    }

    /// Estimated cost of a call from reported usage, when prices are known.
    #[must_use]
    pub fn estimate_cost(&self, usage: &crate::response::TokenUsage) -> Option<MicroCents> {
        let input = self.cost_per_million_input?;
        let output = self.cost_per_million_output?;
        let per_token = |rate: MicroCents, tokens: u64| -> u64 {
            // rate is per million tokens; integer arithmetic, rounded down.
            rate.0.saturating_mul(tokens) / 1_000_000
        };
        Some(MicroCents(
            per_token(input, usage.input).saturating_add(per_token(output, usage.output)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satisfied_by_reports_every_gap_in_order() {
        let requirements = CapabilityRequirements {
            structured_output: vec![StructuredOutputCapability::NativeJsonSchema],
            needs_tools: true,
            needs_streaming: true,
            min_context_tokens: Some(100_000),
            needs_vision: true,
            needs_documents: true,
        };
        let caps = ProviderCapabilities::minimal();
        let mismatch = requirements.satisfied_by(&caps).unwrap_err();
        assert_eq!(mismatch.missing.len(), 6);
        assert!(mismatch.structured_output_unmet());
        assert!(matches!(
            mismatch.missing[0],
            MissingCapability::StructuredOutput { .. }
        ));
        assert!(matches!(mismatch.missing[4], MissingCapability::Documents));
        assert!(matches!(
            mismatch.missing[5],
            MissingCapability::ContextWindow {
                required: 100_000,
                declared: None
            }
        ));
        let text = mismatch.to_string();
        assert!(text.contains("structured_output(declared none"));
        assert!(text.contains("documents"));
        assert!(text.contains("context_window(undeclared"));
    }

    #[test]
    fn documents_are_declared_apart_from_images() {
        // The whole point of the flag: a model that reads a PNG may reject a
        // PDF, so "accepts images" must never be read as "accepts documents".
        let sighted = ProviderCapabilities::minimal().with_vision(true);
        let requirements = CapabilityRequirements::none().with_documents();
        let mismatch = requirements.satisfied_by(&sighted).unwrap_err();
        assert_eq!(mismatch.missing, vec![MissingCapability::Documents]);
        assert_eq!(mismatch.to_string(), "capability mismatch: documents");

        let reader = sighted.with_documents(true);
        assert!(requirements.satisfied_by(&reader).is_ok());
        // And the converse: documents alone do not admit an image.
        let paper_only = ProviderCapabilities::minimal().with_documents(true);
        assert!(
            CapabilityRequirements::none()
                .with_vision()
                .satisfied_by(&paper_only)
                .is_err()
        );
    }

    #[test]
    fn a_declaration_written_before_the_documents_flag_reads_as_false() {
        // Fail closed: an older stored profile must not be read as accepting
        // documents just because the field is absent.
        let stored = serde_json::to_value(ProviderCapabilities::minimal().with_vision(true))
            .expect("serializes");
        let mut object = stored.as_object().expect("an object").clone();
        object.remove("documents");
        let older: ProviderCapabilities =
            serde_json::from_value(serde_json::Value::Object(object)).expect("still decodes");
        assert!(!older.documents);
        assert!(older.vision);
    }

    #[test]
    fn context_window_is_fail_closed_and_compared_numerically() {
        let requirements = CapabilityRequirements::none().with_min_context_tokens(8_000);
        assert!(
            requirements
                .satisfied_by(&ProviderCapabilities::minimal())
                .is_err()
        );
        let small = ProviderCapabilities::minimal().with_max_context_tokens(4_000);
        assert!(requirements.satisfied_by(&small).is_err());
        let large = ProviderCapabilities::minimal().with_max_context_tokens(8_000);
        assert!(requirements.satisfied_by(&large).is_ok());
    }

    #[test]
    fn empty_structured_set_means_no_requirement() {
        let requirements = CapabilityRequirements::none();
        assert!(
            requirements
                .satisfied_by(&ProviderCapabilities::minimal())
                .is_ok()
        );
    }

    #[test]
    fn profile_cost_helpers() {
        let profile = ModelProfile::new("p", "m", ProviderCapabilities::minimal())
            .with_cost(MicroCents::from_cents(250), MicroCents::from_dollars(10))
            .with_region("eu")
            .with_tag("cheap");
        assert_eq!(
            profile.max_cost_per_million(),
            Some(MicroCents::from_dollars(10))
        );
        let usage = crate::response::TokenUsage::new(1_000_000, 500_000);
        assert_eq!(
            profile.estimate_cost(&usage),
            Some(MicroCents::from_cents(250).saturating_add(MicroCents::from_dollars(5)))
        );
        assert_eq!(profile.reference().to_string(), "p/m");
        let unknown = ModelProfile::new("p", "m", ProviderCapabilities::minimal());
        assert_eq!(unknown.max_cost_per_million(), None);
        assert_eq!(unknown.estimate_cost(&usage), None);
    }

    #[test]
    fn labels_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&StructuredOutputCapability::NativeJsonSchema).unwrap(),
            "\"native_json_schema\""
        );
        assert_eq!(
            serde_json::to_string(&ToolCallingCapability::Parallel).unwrap(),
            "\"parallel\""
        );
        let caps =
            ProviderCapabilities::minimal().with_tool_calling(ToolCallingCapability::Parallel);
        assert!(caps.parallel_tool_calls && caps.supports_tools());
    }
}
