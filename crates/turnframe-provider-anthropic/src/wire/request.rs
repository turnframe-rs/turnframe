//! Normalized request → Messages API body.
//!
//! The conversion is total and honest. Everything the endpoint can express is
//! expressed; everything it cannot is either a
//! [`FeatureDropped`](ResponseWarning::FeatureDropped) warning the caller sees
//! on the response, or — when dropping it would change the *meaning* of the
//! call — a [`CapabilityMismatch`] failure before a byte leaves the process.
//!
//! | Situation | What happens |
//! |---|---|
//! | A ninth stop sequence on an endpoint that takes eight | dropped, warned |
//! | A cache hint on a profile that does not cache | dropped, warned |
//! | Metadata beyond `user_id` | dropped, warned |
//! | A non-text part in a system message | dropped, warned |
//! | An attachment whose media type this block cannot carry | dropped, warned |
//! | An image on a profile that declares no vision | refused |
//! | A document on a profile that declares no document input | refused |
//! | A tool on a profile that declares no tool calling | refused |
//! | [`OutputSpec::Json`] on a profile with neither transport | refused |
//!
//! # Four things the Messages API does differently
//!
//! **The system prompt is not a message.** It is a top-level `system` field, so
//! a [`Role::System`] message is hoisted into it rather than sent as a turn —
//! the API has no `system` role and rejects one.
//!
//! **A tool result belongs to a *user* message.** The model's `tool_use` block
//! lives in the assistant turn and the matching `tool_result` block goes in the
//! next user turn, first, which is why [`Role::Tool`] becomes `user` here and
//! results are ordered ahead of prose. The result carries a real `is_error`
//! flag, so unlike the chat-completions format there is no marker prefix to
//! invent.
//!
//! **`max_tokens` is required.** A request without it is a 400, so the
//! conversion always sends one: the caller's, or
//! [`Quirks::default_max_output_tokens`].
//!
//! **There is no `response_format`.** The only way to make the endpoint enforce
//! a schema is [`declare_output_tool`]: one tool whose `input_schema` *is* the
//! required schema, with `tool_choice` pinned to it. That is what
//! [`StructuredOutputCapability::NativeFunctionSchema`] means, and this module
//! is where the declaration is made true.

use serde::Serialize;
use serde_json::Value;
use turnframe_provider::capabilities::{
    CapabilityMismatch, MissingCapability, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::request::{
    CacheHint, ContentPart, DocumentSource, ImageSource, Message, ModelRequest, OutputSpec, Role,
    ToolChoice, ToolSpec,
};
use turnframe_provider::response::ResponseWarning;

use crate::profile::Quirks;

/// Longest tool name the Messages API accepts.
pub(crate) const MAX_TOOL_NAME_LEN: usize = 64;

/// Name the synthetic output tool falls back to when the requested one cannot
/// be made unique.
pub(crate) const FALLBACK_OUTPUT_TOOL_NAME: &str = "turnframe_structured_output";

/// Description attached to the synthetic output tool.
///
/// It tells the model the call *is* the answer. The runtime never executes it:
/// the tool exists only as a transport for a schema-checked document
/// (spec §20.4, §21.4).
pub(crate) const OUTPUT_TOOL_DESCRIPTION: &str = "Return the answer by calling this tool exactly once. Its input is the answer; \
     nothing is executed.";

/// Media type prefix that makes a part an image rather than a document.
const IMAGE_MEDIA_PREFIX: &str = "image/";

/// The Messages API request body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct MessagesRequest {
    pub(crate) model: String,
    /// Required by the API — never omitted, never guessed at call time.
    pub(crate) max_tokens: u32,
    pub(crate) messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) system: Option<SystemPrompt>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ToolDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<ToolChoiceWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) stop_sequences: Vec<String>,
    pub(crate) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<Metadata>,
}

/// The system prompt, in the two shapes the API accepts.
///
/// The plain string is used whenever nothing needs a cache breakpoint, because
/// it is the shape every endpoint that copies this API understands.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum SystemPrompt {
    Text(String),
    Blocks(Vec<Block>),
}

/// One turn.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct WireMessage {
    pub(crate) role: &'static str,
    pub(crate) content: Vec<Block>,
}

/// One content block.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Block {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Image {
        source: SourceWire,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Document {
        source: SourceWire,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

impl Block {
    /// A text block with no cache breakpoint.
    fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            cache_control: None,
        }
    }

    /// Returns `true` for a tool result, which must lead its user turn.
    const fn is_tool_result(&self) -> bool {
        matches!(self, Self::ToolResult { .. })
    }

    /// Puts a cache breakpoint on this block.
    fn mark_cached(&mut self) {
        let slot = match self {
            Self::Text { cache_control, .. }
            | Self::Image { cache_control, .. }
            | Self::Document { cache_control, .. }
            | Self::ToolUse { cache_control, .. }
            | Self::ToolResult { cache_control, .. } => cache_control,
        };
        *slot = Some(CacheControl::ephemeral());
    }
}

/// `false` is the default, so it is left unsaid.
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Where the bytes of an image or a document come from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum SourceWire {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// A prompt-cache breakpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CacheControl {
    pub(crate) r#type: &'static str,
}

impl CacheControl {
    /// The only cache lifetime this adapter asks for.
    const fn ephemeral() -> Self {
        Self {
            r#type: "ephemeral",
        }
    }
}

/// A declared tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolDeclaration {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
}

/// `tool_choice`, in the four shapes the API accepts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ToolChoiceWire {
    Auto {
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Any {
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Tool {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    None,
}

/// The only metadata field the Messages API has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Metadata {
    pub(crate) user_id: String,
}

/// Metadata key whose value becomes `metadata.user_id`.
pub(crate) const USER_ID_KEY: &str = "user_id";

/// A converted request and everything the conversion had to give up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConvertedRequest {
    pub(crate) body: MessagesRequest,
    pub(crate) warnings: Vec<ResponseWarning>,
}

/// Builds the Messages body for one normalized request.
///
/// # Errors
///
/// Returns a [`ProviderError`] carrying a [`CapabilityMismatch`] when the
/// request needs something the profile does not declare.
pub(crate) fn build_request(
    request: &ModelRequest,
    model: &str,
    capabilities: &ProviderCapabilities,
    quirks: &Quirks,
    streaming: bool,
) -> Result<ConvertedRequest, ProviderError> {
    let mut warnings = Vec::new();
    let mut missing = Vec::new();

    let needs_vision = request
        .messages
        .iter()
        .any(|message| message.role != Role::System && message.has_image());
    if needs_vision && !capabilities.vision {
        missing.push(MissingCapability::Vision);
    }
    let needs_documents = request
        .messages
        .iter()
        .any(|message| message.role != Role::System && message.has_document());
    if needs_documents && !capabilities.documents {
        missing.push(MissingCapability::Documents);
    }
    if !request.tools.is_empty() && !capabilities.supports_tools() {
        missing.push(MissingCapability::ToolCalling);
    }
    let output = output_transport(request, capabilities, &mut missing);
    if !missing.is_empty() {
        return Err(ProviderError::capability_mismatch(CapabilityMismatch {
            missing,
        }));
    }

    let system = system_text(request, output.prompt_hint, &mut warnings);
    let mut converted = convert_messages(request, &mut warnings);
    let system = place_cache_breakpoints(
        system,
        &mut converted,
        request.cache_hint.clone(),
        capabilities.prompt_caching,
        &mut warnings,
    );

    let mut tools: Vec<ToolDeclaration> = request
        .tools
        .iter()
        .map(|tool| ToolDeclaration {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.parameters.clone(),
        })
        .collect();
    let sequential = (quirks.send_disable_parallel_tool_use && !capabilities.parallel_tool_calls)
        .then_some(true);
    let tool_choice = match output.tool {
        Some(tool) => {
            if request.tool_choice != ToolChoice::Auto {
                // The transport owns the choice; saying so beats obeying a
                // caller who asked for a document and for a free hand at once.
                warnings.push(dropped("tool_choice"));
            }
            let name = tool.name.clone();
            tools.push(tool);
            Some(ToolChoiceWire::Tool {
                name,
                disable_parallel_tool_use: Some(true),
            })
        }
        None => convert_tool_choice(&request.tool_choice, tools.is_empty(), sequential),
    };

    let stop_sequences = truncate_stop(&request.stop, quirks.max_stop_sequences, &mut warnings);
    let metadata = convert_metadata(request, quirks, &mut warnings);
    let max_tokens = request
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .unwrap_or(quirks.default_max_output_tokens);

    let sampling = request.sampling_for(capabilities);
    warnings.extend(sampling.dropped.iter().map(|name| dropped(name)));
    Ok(ConvertedRequest {
        body: MessagesRequest {
            model: model.to_owned(),
            max_tokens,
            messages: converted.messages,
            system,
            tools,
            tool_choice,
            temperature: sampling.temperature,
            stop_sequences,
            stream: streaming,
            metadata,
        },
        warnings,
    })
}

/// What the output specification became.
struct OutputConversion {
    /// The synthetic tool the schema travels in, when the transport is a forced
    /// function schema.
    tool: Option<ToolDeclaration>,
    /// Text appended to the system prompt when the transport only describes the
    /// schema.
    prompt_hint: Option<String>,
}

/// Maps [`OutputSpec`] onto the transport the **declared capability** allows.
///
/// The mapping is driven by the declaration, never by what the caller asked
/// for: that is the whole no-silent-downgrade rule. A profile that declares
/// `PromptOnly` cannot be talked into forcing a tool by a request that sets
/// `strict: true`, and a profile that declares a transport this adapter cannot
/// send fails closed instead of improvising.
fn output_transport(
    request: &ModelRequest,
    capabilities: &ProviderCapabilities,
    missing: &mut Vec<MissingCapability>,
) -> OutputConversion {
    let OutputSpec::Json { schema, name, .. } = &request.output else {
        return OutputConversion {
            tool: None,
            prompt_hint: None,
        };
    };
    match capabilities.structured_output {
        StructuredOutputCapability::NativeFunctionSchema => OutputConversion {
            tool: Some(declare_output_tool(name, schema, &request.tools)),
            prompt_hint: None,
        },
        StructuredOutputCapability::PromptOnly => OutputConversion {
            tool: None,
            prompt_hint: Some(schema_hint(schema)),
        },
        declared => {
            // `None` has no transport at all; `NativeJsonSchema`, `JsonObject`
            // and `GrammarConstrained` are transports the Messages API does not
            // have, and the builder refuses to declare them. Reaching here means
            // a profile was assembled another way, so fail closed.
            missing.push(MissingCapability::StructuredOutput {
                required: vec![
                    StructuredOutputCapability::NativeFunctionSchema,
                    StructuredOutputCapability::PromptOnly,
                ],
                declared,
            });
            OutputConversion {
                tool: None,
                prompt_hint: None,
            }
        }
    }
}

/// Builds the one tool a structured answer travels in.
///
/// The tool's `input_schema` **is** the caller's schema — not a description of
/// it, not a wrapper around it — which is what makes
/// [`StructuredOutputCapability::NativeFunctionSchema`] a true statement here:
/// the endpoint validates the model's input against it before the block is
/// emitted. The call is never executed; the runtime reads its input as the
/// document (spec §20.4).
pub(crate) fn declare_output_tool(
    name: &str,
    schema: &Value,
    declared: &[ToolSpec],
) -> ToolDeclaration {
    ToolDeclaration {
        name: output_tool_name(name, declared),
        description: OUTPUT_TOOL_DESCRIPTION.to_owned(),
        input_schema: schema.clone(),
    }
}

/// A tool name the API accepts and no declared read tool already owns.
///
/// The API takes `^[a-zA-Z0-9_-]{1,64}$`, so anything else becomes `_`. A
/// collision with a read tool would make the forced choice ambiguous, so it is
/// resolved by suffixing rather than by hoping.
pub(crate) fn output_tool_name(requested: &str, declared: &[ToolSpec]) -> String {
    let sanitized: String = requested
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .take(MAX_TOOL_NAME_LEN)
        .collect();
    let base = if sanitized.is_empty() {
        FALLBACK_OUTPUT_TOOL_NAME.to_owned()
    } else {
        sanitized
    };
    let taken = |candidate: &str| declared.iter().any(|tool| tool.name == candidate);
    if !taken(&base) {
        return base;
    }
    for attempt in 0..100u32 {
        let suffix = if attempt == 0 {
            "_output".to_owned()
        } else {
            format!("_output_{attempt}")
        };
        let candidate = with_suffix(&base, &suffix);
        if !taken(&candidate) {
            return candidate;
        }
    }
    FALLBACK_OUTPUT_TOOL_NAME.to_owned()
}

/// Appends `suffix`, trimming `base` so the result still fits.
fn with_suffix(base: &str, suffix: &str) -> String {
    let room = MAX_TOOL_NAME_LEN.saturating_sub(suffix.len());
    let mut out: String = base.chars().take(room).collect();
    out.push_str(suffix);
    out
}

/// The instruction appended to the system prompt for a non-enforcing transport.
fn schema_hint(schema: &Value) -> String {
    format!(
        "Reply with a single JSON document and nothing else. \
         It must satisfy this JSON Schema:\n{schema}"
    )
}

/// Joins the caller's system prompt, any hoisted system message and the
/// transport's own hint, in that order.
///
/// A system message that carries anything but text loses it: the `system` field
/// is a text field, and inventing a turn for the rest would change the shape of
/// the conversation.
fn system_text(
    request: &ModelRequest,
    hint: Option<String>,
    warnings: &mut Vec<ResponseWarning>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(system) = request.system.as_deref().filter(|text| !text.is_empty()) {
        parts.push(system.to_owned());
    }
    for message in request.messages.iter().filter(|m| m.role == Role::System) {
        if message.content.iter().any(|part| part.as_text().is_none()) {
            warnings.push(dropped("system_non_text"));
        }
        let text = message.text();
        if !text.is_empty() {
            parts.push(text);
        }
    }
    if let Some(hint) = hint {
        parts.push(hint);
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// The converted conversation plus, for each input message, the turn it landed
/// in — which is what a prefix cache hint is measured against.
struct ConvertedMessages {
    messages: Vec<WireMessage>,
    /// `turn_of_input[i]` is the index in `messages` that input message `i`
    /// contributed to, or `None` when it contributed nothing.
    turn_of_input: Vec<Option<usize>>,
}

/// Converts the conversation into Messages turns.
///
/// Three rules, all of them the API's:
///
/// * a [`Role::System`] message is not a turn — it was hoisted into `system`;
/// * a [`Role::Tool`] message is a **user** turn carrying `tool_result` blocks,
///   ordered ahead of anything else in that turn;
/// * consecutive turns with the same role are merged, because the API treats
///   them as one turn anyway and merging here makes the result deterministic.
fn convert_messages(
    request: &ModelRequest,
    warnings: &mut Vec<ResponseWarning>,
) -> ConvertedMessages {
    let mut messages: Vec<WireMessage> = Vec::with_capacity(request.messages.len());
    let mut turn_of_input: Vec<Option<usize>> = Vec::with_capacity(request.messages.len());

    for message in &request.messages {
        if message.role == Role::System {
            turn_of_input.push(None);
            continue;
        }
        let blocks = convert_blocks(message, warnings);
        if blocks.is_empty() {
            // The API rejects an empty content array, so a message that carries
            // nothing this adapter can express is not sent at all.
            turn_of_input.push(None);
            continue;
        }
        let role = wire_role(message.role);
        match messages.last_mut() {
            Some(last) if last.role == role => {
                last.content.extend(blocks);
                turn_of_input.push(Some(messages.len() - 1));
            }
            _ => {
                messages.push(WireMessage {
                    role,
                    content: blocks,
                });
                turn_of_input.push(Some(messages.len() - 1));
            }
        }
    }
    for message in &mut messages {
        if message.role == "user" {
            // Stable partition: results first, everything else in its order.
            message
                .content
                .sort_by_key(|block| u8::from(!block.is_tool_result()));
        }
    }
    ConvertedMessages {
        messages,
        turn_of_input,
    }
}

/// Converts one message's parts into blocks, results first within the message.
fn convert_blocks(message: &Message, warnings: &mut Vec<ResponseWarning>) -> Vec<Block> {
    let mut results: Vec<Block> = Vec::new();
    let mut rest: Vec<Block> = Vec::new();
    let mut calls: Vec<Block> = Vec::new();
    for part in &message.content {
        match part {
            ContentPart::Text { text } if !text.is_empty() => rest.push(Block::text(text)),
            ContentPart::Text { .. } => {}
            ContentPart::Image { source } => match image_block(source) {
                Some(block) => rest.push(block),
                None => warnings.push(dropped("unsupported_image_source")),
            },
            ContentPart::Document { source } => match document_block(source) {
                Some(block) => rest.push(block),
                None => warnings.push(dropped("unsupported_document_source")),
            },
            ContentPart::ToolCall(call) => calls.push(Block::ToolUse {
                id: call.id.as_str().to_owned(),
                name: call.name.clone(),
                input: call.arguments.clone(),
                cache_control: None,
            }),
            ContentPart::ToolResult(result) => results.push(Block::ToolResult {
                tool_use_id: result.call_id.as_str().to_owned(),
                content: result.content.clone(),
                is_error: result.is_error,
                cache_control: None,
            }),
            // The part vocabulary is growable; an adapter that cannot express a
            // future part says so instead of inventing one.
            _ => warnings.push(dropped("unsupported_content_part")),
        }
    }
    results.extend(rest);
    results.extend(calls);
    results
}

/// Builds the `image` block for an image part.
///
/// An image is an image: the Messages API's `document` block is reached through
/// [`ContentPart::Document`] now, so nothing here has to read a media type and
/// decide what the caller meant. A media type that is not `image/*` is refused
/// rather than sent as an image the API will reject — a PDF announced as a PNG
/// is a lie whichever layer tells it.
fn image_block(source: &ImageSource) -> Option<Block> {
    match source {
        ImageSource::Base64 { media_type, data } => media_type
            .starts_with(IMAGE_MEDIA_PREFIX)
            .then(|| Block::Image {
                source: SourceWire::Base64 {
                    media_type: media_type.clone(),
                    data: data.clone(),
                },
                cache_control: None,
            }),
        ImageSource::Url { url } => Some(Block::Image {
            source: SourceWire::Url { url: url.clone() },
            cache_control: None,
        }),
        // A future source this adapter cannot express is dropped and reported,
        // never guessed at.
        _ => None,
    }
}

/// Builds the `document` block for a document part.
///
/// Both forms state their media type, so the block is exact: no suffix is read
/// off a URL, and a `.pdf` in a query string decides nothing.
fn document_block(source: &DocumentSource) -> Option<Block> {
    let wire = match source {
        DocumentSource::Base64 { media_type, data } => SourceWire::Base64 {
            media_type: media_type.clone(),
            data: data.clone(),
        },
        DocumentSource::Url { url, .. } => SourceWire::Url { url: url.clone() },
        _ => return None,
    };
    Some(Block::Document {
        source: wire,
        cache_control: None,
    })
}

/// The role name a normalized role travels under.
///
/// [`Role::System`] never reaches here — it is hoisted — and [`Role::Tool`] is
/// a user turn, because that is where the API puts a tool result.
const fn wire_role(role: Role) -> &'static str {
    match role {
        Role::Assistant => "assistant",
        Role::User | Role::Tool | Role::System => "user",
    }
}

/// Places the cache breakpoints a [`CacheHint`] asks for.
///
/// The system prompt switches from the string shape to the block shape only
/// when it needs a breakpoint, because the string shape is the one every
/// endpoint that copies this API understands.
///
/// A [`CacheHint::Prefix`] marks the end of the turn its last message landed
/// in. When two input messages merged into one turn the breakpoint therefore
/// covers a little more than was asked — never less — which changes cost and
/// never meaning.
fn place_cache_breakpoints(
    system: Option<String>,
    converted: &mut ConvertedMessages,
    hint: CacheHint,
    prompt_caching: bool,
    warnings: &mut Vec<ResponseWarning>,
) -> Option<SystemPrompt> {
    let plain = |system: Option<String>| system.map(SystemPrompt::Text);
    if matches!(hint, CacheHint::None) {
        return plain(system);
    }
    if !prompt_caching {
        warnings.push(dropped("cache_hint"));
        return plain(system);
    }
    let system = match system {
        Some(text) => {
            let mut block = Block::text(text);
            block.mark_cached();
            Some(SystemPrompt::Blocks(vec![block]))
        }
        None => None,
    };
    if let CacheHint::Prefix { messages } = hint {
        let turn = converted
            .turn_of_input
            .iter()
            .take(messages)
            .filter_map(|turn| *turn)
            .next_back();
        if let Some(turn) = turn
            && let Some(block) = converted
                .messages
                .get_mut(turn)
                .and_then(|message| message.content.last_mut())
        {
            block.mark_cached();
        }
    }
    system
}

/// Maps the tool-choice mode. `Auto` is the API's own default and is omitted
/// unless a sequential declaration has to be enforced; every mode is omitted
/// when no tool is declared, because the API rejects `tool_choice` without
/// `tools`.
fn convert_tool_choice(
    choice: &ToolChoice,
    no_tools: bool,
    disable_parallel_tool_use: Option<bool>,
) -> Option<ToolChoiceWire> {
    if no_tools {
        return None;
    }
    match choice {
        ToolChoice::Auto => disable_parallel_tool_use.map(|flag| ToolChoiceWire::Auto {
            disable_parallel_tool_use: Some(flag),
        }),
        ToolChoice::None => Some(ToolChoiceWire::None),
        ToolChoice::Required => Some(ToolChoiceWire::Any {
            disable_parallel_tool_use,
        }),
        ToolChoice::Named { name } => Some(ToolChoiceWire::Tool {
            name: name.clone(),
            disable_parallel_tool_use,
        }),
        _ => None,
    }
}

/// Truncates the stop list to what the endpoint accepts, warning when it does.
fn truncate_stop(
    stop: &[String],
    limit: usize,
    warnings: &mut Vec<ResponseWarning>,
) -> Vec<String> {
    if stop.len() > limit {
        warnings.push(dropped("stop_sequences_over_limit"));
        stop.iter().take(limit).cloned().collect()
    } else {
        stop.to_vec()
    }
}

/// Maps request metadata onto the one field the API has.
///
/// `user_id` travels; every other label is dropped with a warning, because the
/// Messages API has nowhere to put it and quietly losing a routing label is how
/// a replay record stops matching the call it describes.
fn convert_metadata(
    request: &ModelRequest,
    quirks: &Quirks,
    warnings: &mut Vec<ResponseWarning>,
) -> Option<Metadata> {
    if request.metadata.is_empty() {
        return None;
    }
    let user_id = quirks
        .send_metadata_user_id
        .then(|| request.metadata.get(USER_ID_KEY))
        .flatten();
    let carried = usize::from(user_id.is_some());
    if request.metadata.len() > carried {
        warnings.push(dropped("metadata"));
    }
    user_id.map(|user_id| Metadata {
        user_id: user_id.to_owned(),
    })
}

/// A dropped-feature warning with a short, stable code.
fn dropped(feature: &str) -> ResponseWarning {
    ResponseWarning::FeatureDropped {
        feature: feature.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turnframe_provider::capabilities::ToolCallingCapability;
    use turnframe_provider::purpose::ModelPurpose;
    use turnframe_provider::request::{ToolCall, ToolResult};

    fn caps() -> ProviderCapabilities {
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
            .with_tool_calling(ToolCallingCapability::Parallel)
            .with_vision(true)
            .with_documents(true)
            .with_streaming(true)
            .with_prompt_caching(true)
            .with_preserves_call_ids(true)
    }

    fn convert(request: &ModelRequest, capabilities: &ProviderCapabilities) -> ConvertedRequest {
        build_request(
            request,
            "claude-test",
            capabilities,
            &Quirks::anthropic(),
            false,
        )
        .expect("converts")
    }

    fn plan_schema() -> Value {
        json!({"type": "object", "properties": {"acts": {"type": "array"}}, "required": ["acts"]})
    }

    #[test]
    fn a_schema_travels_as_the_input_schema_of_one_forced_tool() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_system("Be precise.")
            .with_message(Message::user("ciao"))
            .with_output(OutputSpec::json("user_turn_plan", plan_schema()));
        let converted = convert(&request, &caps());

        assert_eq!(converted.body.tools.len(), 1);
        let tool = &converted.body.tools[0];
        assert_eq!(tool.name, "user_turn_plan");
        assert_eq!(tool.input_schema, plan_schema());
        assert_eq!(
            converted.body.tool_choice,
            Some(ToolChoiceWire::Tool {
                name: "user_turn_plan".to_owned(),
                disable_parallel_tool_use: Some(true)
            })
        );
        // Nothing describes the schema in prose when the endpoint enforces it.
        assert_eq!(
            converted.body.system,
            Some(SystemPrompt::Text("Be precise.".to_owned()))
        );
    }

    #[test]
    fn a_prompt_only_profile_describes_the_schema_and_forces_nothing() {
        let request = ModelRequest::new(ModelPurpose::OfflineEvaluate)
            .with_message(Message::user("ciao"))
            .with_output(OutputSpec::json("plan", plan_schema()));
        let weak = caps().with_structured_output(StructuredOutputCapability::PromptOnly);
        let converted = convert(&request, &weak);
        assert!(converted.body.tools.is_empty());
        assert!(converted.body.tool_choice.is_none());
        let Some(SystemPrompt::Text(system)) = &converted.body.system else {
            panic!("the hint must be a plain system prompt");
        };
        assert!(system.contains("JSON Schema"), "{system}");
        assert!(system.contains("\"acts\""), "{system}");
    }

    #[test]
    fn a_transport_the_messages_api_does_not_have_is_refused() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_output(OutputSpec::json("plan", plan_schema()));
        for declared in [
            StructuredOutputCapability::NativeJsonSchema,
            StructuredOutputCapability::JsonObject,
            StructuredOutputCapability::GrammarConstrained,
            StructuredOutputCapability::None,
        ] {
            let profile = caps().with_structured_output(declared);
            let error = build_request(&request, "m", &profile, &Quirks::anthropic(), false)
                .expect_err("refused");
            assert_eq!(
                error.retry_class(),
                turnframe_provider::error::RetryClass::Fallback
            );
            assert!(error.to_string().contains("structured_output"), "{error}");
        }
    }

    #[test]
    fn the_output_tool_never_collides_with_a_read_tool() {
        let read = vec![ToolSpec::new("plan", "reads", json!({}))];
        assert_eq!(output_tool_name("plan", &read), "plan_output");
        assert_eq!(output_tool_name("other", &read), "other");
        // Anything the API's name grammar rejects becomes an underscore.
        assert_eq!(output_tool_name("user turn/plan", &[]), "user_turn_plan");
        assert_eq!(output_tool_name("", &[]), FALLBACK_OUTPUT_TOOL_NAME);
        let long = "x".repeat(200);
        assert_eq!(output_tool_name(&long, &[]).len(), MAX_TOOL_NAME_LEN);
        let taken = vec![
            ToolSpec::new(&long[..MAX_TOOL_NAME_LEN], "reads", json!({})),
            ToolSpec::new(
                with_suffix(&long[..MAX_TOOL_NAME_LEN], "_output"),
                "reads",
                json!({}),
            ),
        ];
        let resolved = output_tool_name(&long, &taken);
        assert!(resolved.ends_with("_output_1"), "{resolved}");
        assert!(resolved.len() <= MAX_TOOL_NAME_LEN);
    }

    #[test]
    fn the_system_prompt_is_hoisted_out_of_the_conversation() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_system("Answer in Italian.")
            .with_message(Message::system("Never invent identifiers."))
            .with_message(Message::user("ciao"));
        let converted = convert(&request, &caps());
        assert_eq!(
            converted.body.system,
            Some(SystemPrompt::Text(
                "Answer in Italian.\n\nNever invent identifiers.".to_owned()
            ))
        );
        assert_eq!(converted.body.messages.len(), 1);
        assert_eq!(converted.body.messages[0].role, "user");
    }

    #[test]
    fn a_tool_result_becomes_the_first_block_of_a_user_turn() {
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::user("stato?"))
            .with_message(Message::new(
                Role::Assistant,
                vec![ContentPart::ToolCall(ToolCall::new(
                    "toolu_1",
                    "read_case",
                    json!({"id": "c1"}),
                ))],
            ))
            .with_message(Message::tool_result(ToolResult::error("toolu_1", "boom")))
            .with_message(Message::user("e adesso?"))
            .with_tools(vec![ToolSpec::new("read_case", "Reads.", json!({}))]);
        let converted = convert(&request, &caps());

        assert_eq!(converted.body.messages.len(), 3);
        assert_eq!(converted.body.messages[1].role, "assistant");
        assert_eq!(
            converted.body.messages[1].content[0],
            Block::ToolUse {
                id: "toolu_1".to_owned(),
                name: "read_case".to_owned(),
                input: json!({"id": "c1"}),
                cache_control: None
            }
        );
        // The result and the following user text merged into one turn, with the
        // result leading it.
        let last = &converted.body.messages[2];
        assert_eq!(last.role, "user");
        assert_eq!(
            last.content[0],
            Block::ToolResult {
                tool_use_id: "toolu_1".to_owned(),
                content: "boom".to_owned(),
                is_error: true,
                cache_control: None
            }
        );
        assert_eq!(last.content[1], Block::text("e adesso?"));
        // `is_error` is a real field here, so no marker is invented.
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(
            body["messages"][2]["content"][0]["is_error"],
            Value::Bool(true)
        );
    }

    #[test]
    fn an_image_needs_a_vision_declaration_and_a_document_needs_its_own() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![
                ContentPart::text("guarda"),
                ContentPart::image_base64("image/png", "AAAA"),
                ContentPart::document_base64("application/pdf", "JVBER"),
                ContentPart::document_url("https://x.test/report?v=2", "application/pdf"),
                ContentPart::image_url("https://x.test/photo.jpg"),
            ],
        ));
        let blind = caps().with_vision(false);
        let error =
            build_request(&request, "m", &blind, &Quirks::anthropic(), false).expect_err("refused");
        assert!(error.to_string().contains("vision"), "{error}");

        // The two are declared apart, so a profile that reads images and not
        // PDFs is refused for the PDF and named for it.
        let sighted = caps().with_documents(false);
        let error = build_request(&request, "m", &sighted, &Quirks::anthropic(), false)
            .expect_err("refused");
        assert!(error.to_string().contains("documents"), "{error}");

        let converted = convert(&request, &caps());
        let blocks = &converted.body.messages[0].content;
        assert_eq!(blocks.len(), 5);
        assert!(matches!(blocks[1], Block::Image { .. }));
        assert!(matches!(blocks[2], Block::Document { .. }));
        // The URL document is a document because the part says so — not
        // because the path happens to end in `.pdf`, which this one does not.
        assert!(matches!(blocks[3], Block::Document { .. }));
        assert!(matches!(blocks[4], Block::Image { .. }));
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(
            body["messages"][0]["content"][1]["source"]["type"],
            "base64"
        );
        assert_eq!(
            body["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(
            body["messages"][0]["content"][2]["source"]["media_type"],
            "application/pdf"
        );
        assert_eq!(body["messages"][0]["content"][3]["type"], "document");
        assert_eq!(body["messages"][0]["content"][4]["source"]["type"], "url");
    }

    #[test]
    fn an_image_part_whose_media_type_is_not_an_image_is_reported_not_reshaped() {
        // The workaround this adapter used to carry: a PDF arriving as an image
        // was quietly rerouted to a document block. With a document part in the
        // vocabulary that guess is neither needed nor honest.
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::image_base64("application/pdf", "JVBER")],
        ));
        let converted = convert(&request, &caps());
        assert!(
            converted
                .warnings
                .contains(&dropped("unsupported_image_source"))
        );
        assert!(converted.body.messages.is_empty());
    }

    #[test]
    fn cache_hints_place_breakpoints_only_where_caching_is_declared() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_system("stable preamble")
            .with_message(Message::user("primo"))
            .with_message(Message::assistant("risposta"))
            .with_message(Message::user("secondo"))
            .with_cache_hint(CacheHint::Prefix { messages: 2 });
        let converted = convert(&request, &caps());
        let Some(SystemPrompt::Blocks(system)) = &converted.body.system else {
            panic!("a cached system prompt uses the block shape");
        };
        assert_eq!(
            system[0],
            Block::Text {
                text: "stable preamble".to_owned(),
                cache_control: Some(CacheControl::ephemeral())
            }
        );
        // The breakpoint sits at the end of the turn the second message is in.
        assert_eq!(
            converted.body.messages[1].content[0],
            Block::Text {
                text: "risposta".to_owned(),
                cache_control: Some(CacheControl::ephemeral())
            }
        );
        assert!(converted.warnings.is_empty());

        let uncached = caps().with_prompt_caching(false);
        let converted = convert(&request, &uncached);
        assert!(matches!(converted.body.system, Some(SystemPrompt::Text(_))));
        assert!(converted.warnings.contains(&dropped("cache_hint")));
    }

    #[test]
    fn max_tokens_is_always_sent_and_the_caller_wins() {
        let request =
            ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("ciao"));
        assert_eq!(
            convert(&request, &caps()).body.max_tokens,
            crate::profile::DEFAULT_MAX_OUTPUT_TOKENS
        );
        let capped = request.clone().with_max_output_tokens(128);
        assert_eq!(convert(&capped, &caps()).body.max_tokens, 128);

        let quirky = Quirks::anthropic().with_default_max_output_tokens(999);
        let converted = build_request(&request, "m", &caps(), &quirky, true).expect("converts");
        assert_eq!(converted.body.max_tokens, 999);
        assert!(converted.body.stream);
    }

    #[test]
    fn dropped_features_are_warned_about_rather_than_hidden() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("ciao"))
            .with_stop(vec!["a".into(), "b".into(), "c".into()])
            .with_metadata("user_id", "u-42")
            .expect("label")
            .with_metadata("workflow", "trip")
            .expect("label");
        let tight = Quirks::anthropic().with_max_stop_sequences(2);
        let converted = build_request(&request, "m", &caps(), &tight, false).expect("converts");
        assert_eq!(
            converted.body.stop_sequences,
            ["a".to_owned(), "b".to_owned()]
        );
        assert!(
            converted
                .warnings
                .contains(&dropped("stop_sequences_over_limit"))
        );
        assert_eq!(
            converted.body.metadata,
            Some(Metadata {
                user_id: "u-42".to_owned()
            })
        );
        assert!(converted.warnings.contains(&dropped("metadata")));

        let plain = Quirks::conservative();
        let converted = build_request(&request, "m", &caps(), &plain, false).expect("converts");
        assert!(converted.body.metadata.is_none());
        assert!(converted.warnings.contains(&dropped("metadata")));
    }

    #[test]
    fn tool_choice_follows_the_request_unless_a_document_was_asked_for() {
        let tools = vec![ToolSpec::new("read_case", "Reads.", json!({}))];
        let base = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::user("stato?"))
            .with_tools(tools.clone())
            .with_output(OutputSpec::ToolCalls);

        assert!(convert(&base, &caps()).body.tool_choice.is_none());
        assert_eq!(
            convert(
                &base.clone().with_tool_choice(ToolChoice::Required),
                &caps()
            )
            .body
            .tool_choice,
            Some(ToolChoiceWire::Any {
                disable_parallel_tool_use: None
            })
        );
        assert_eq!(
            convert(&base.clone().with_tool_choice(ToolChoice::None), &caps())
                .body
                .tool_choice,
            Some(ToolChoiceWire::None)
        );
        assert_eq!(
            convert(
                &base
                    .clone()
                    .with_tool_choice(ToolChoice::named("read_case")),
                &caps()
            )
            .body
            .tool_choice,
            Some(ToolChoiceWire::Tool {
                name: "read_case".to_owned(),
                disable_parallel_tool_use: None
            })
        );

        // A sequential declaration is enforced on the wire, not merely stated.
        let sequential = caps().with_tool_calling(ToolCallingCapability::Sequential);
        assert_eq!(
            convert(&base, &sequential).body.tool_choice,
            Some(ToolChoiceWire::Auto {
                disable_parallel_tool_use: Some(true)
            })
        );

        // A structured request overrides an explicit choice, and says so.
        let structured = base
            .with_output(OutputSpec::json("plan", plan_schema()))
            .with_tool_choice(ToolChoice::Required);
        let converted = convert(&structured, &caps());
        assert_eq!(
            converted.body.tool_choice,
            Some(ToolChoiceWire::Tool {
                name: "plan".to_owned(),
                disable_parallel_tool_use: Some(true)
            })
        );
        assert!(converted.warnings.contains(&dropped("tool_choice")));
        assert_eq!(converted.body.tools.len(), 2);
    }

    #[test]
    fn a_tool_on_a_tool_less_profile_is_refused() {
        let request = ModelRequest::new(ModelPurpose::Investigate).with_tools(vec![ToolSpec::new(
            "read_case",
            "Reads.",
            json!({}),
        )]);
        let plain = caps().with_tool_calling(ToolCallingCapability::None);
        let error =
            build_request(&request, "m", &plain, &Quirks::anthropic(), false).expect_err("refused");
        assert!(error.to_string().contains("tool_calling"), "{error}");
    }

    #[test]
    fn the_serialized_body_is_the_shape_the_endpoint_expects() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("ciao"))
            .with_temperature(0.0);
        let converted = convert(&request, &caps().with_temperature(true));
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(body["model"], "claude-test");
        assert_eq!(body["stream"], false);
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "ciao");
        assert_eq!(body["temperature"], 0.0);
        // Absent options do not appear as nulls.
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("system").is_none());
        assert!(body.get("metadata").is_none());
        assert!(body.get("stop_sequences").is_none());
    }

    #[test]
    fn an_empty_message_is_not_sent_at_all() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::new(Role::User, Vec::new()))
            .with_message(Message::user("ciao"));
        let converted = convert(&request, &caps());
        assert_eq!(converted.body.messages.len(), 1);
        assert_eq!(converted.body.messages[0].content.len(), 1);
    }
}
