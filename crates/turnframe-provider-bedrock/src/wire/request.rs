//! Normalized request → Converse input.
//!
//! The conversion is total and honest. Everything Converse can express is
//! expressed; everything it cannot is either a
//! [`FeatureDropped`](ResponseWarning::FeatureDropped) warning the caller sees
//! on the response, or — when dropping it would change the *meaning* of the
//! call — a [`CapabilityMismatch`] failure before a byte leaves the process.
//!
//! | Situation | What happens |
//! |---|---|
//! | A fifth stop sequence on a model that takes four | dropped, warned |
//! | A cache hint on a profile that does not cache | dropped, warned |
//! | A metadata label Bedrock's charset rejects | dropped, warned |
//! | A non-text part in a system message | dropped, warned |
//! | An image whose media type Converse has no format for | dropped, warned |
//! | A document whose media type Converse has no format for | dropped, warned |
//! | An attachment behind an `https://` URL | dropped, warned |
//! | An image on a profile that declares no vision | refused |
//! | A document on a profile that declares no document input | refused |
//! | A tool on a profile that declares no tool calling | refused |
//! | [`OutputSpec::Json`] on a profile with neither transport | refused |
//!
//! # Five things Converse does differently
//!
//! **The system prompt is not a message.** It is a separate `system` array of
//! [`SystemContentBlock`]s, so a [`Role::System`] message is hoisted into it
//! rather than sent as a turn — the API has no system role.
//!
//! **A tool result belongs to a *user* turn.** The model's `toolUse` block
//! lives in the assistant turn and the matching `toolResult` goes in the next
//! user turn, first. [`Role::Tool`] therefore becomes `user` here, and results
//! are ordered ahead of prose. The result carries a real
//! [`ToolResultStatus`], so no marker prefix is invented for a failure.
//!
//! **Turns alternate.** Converse rejects two consecutive turns with the same
//! role, so consecutive messages with the same role are merged into one turn.
//!
//! **An attachment is bytes, not a URL.** [`ImageBlock`] and [`DocumentBlock`]
//! take inline bytes or an S3 location and nothing else, and each carries an
//! explicit format enum rather than a media type — so the media type is what
//! decides between the two blocks and which format is sent.
//!
//! **There is no response format.** The only way to make Converse enforce a
//! schema is [`declare_output_tool`]: one tool whose `inputSchema` *is* the
//! required schema, with `toolChoice` pinned to it. That is what
//! [`StructuredOutputCapability::NativeFunctionSchema`] means, and this module
//! is where the declaration is made true.

use std::collections::HashMap;

use aws_sdk_bedrockruntime::types::{
    CachePointBlock, CachePointType, ContentBlock, ConversationRole, DocumentBlock, DocumentFormat,
    DocumentSource as WireDocumentSource, ImageBlock, ImageFormat, ImageSource as WireImageSource,
    InferenceConfiguration, Message as WireMessage, S3Location, SpecificToolChoice,
    SystemContentBlock, Tool, ToolChoice as WireToolChoice, ToolConfiguration, ToolInputSchema,
    ToolResultBlock, ToolResultContentBlock, ToolResultStatus, ToolSpecification, ToolUseBlock,
};
use aws_smithy_types::Blob;
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

use crate::wire::document::to_document;

/// Longest tool name Converse accepts.
pub(crate) const MAX_TOOL_NAME_LEN: usize = 64;

/// Name the synthetic output tool falls back to when the requested one cannot
/// be sanitized into something usable.
pub(crate) const FALLBACK_OUTPUT_TOOL_NAME: &str = "turnframe_structured_output";

/// Description attached to the synthetic output tool.
///
/// It tells the model the call *is* the answer. The runtime never executes it:
/// the tool exists only as a transport for a schema-checked document
/// (spec §20.4, §21.4).
pub(crate) const OUTPUT_TOOL_DESCRIPTION: &str = "Return the answer by calling this tool exactly once. Its input is the answer; \
     nothing is executed.";

/// Metadata label carrying the call's stable request id (spec §20.7).
///
/// Converse has no idempotency token, so the request id travels as a
/// correlation label in `requestMetadata` instead — enough to line a retry up
/// with the call it repeats in Bedrock's invocation logs, and no more.
pub(crate) const REQUEST_ID_LABEL: &str = "turnframe_request_id";

/// Media type prefix that makes an attachment an image rather than a document.
const IMAGE_MEDIA_PREFIX: &str = "image/";

/// Scheme an attachment URL must use to become an S3 location.
const S3_SCHEME: &str = "s3://";

/// What the conversion produced, and what it had to give up.
///
/// The pieces are handed to the SDK's fluent builder by the provider; keeping
/// them in a plain struct is what lets the conversion be unit-tested without a
/// client, a credential or a socket.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConvertedRequest {
    /// The `system` array.
    pub(crate) system: Vec<SystemContentBlock>,
    /// The conversation, oldest first, with turns alternating.
    pub(crate) messages: Vec<WireMessage>,
    /// `inferenceConfig`, when the request constrains anything.
    pub(crate) inference: Option<InferenceConfiguration>,
    /// `toolConfig`, when tools or the forced output tool are declared.
    pub(crate) tool_config: Option<ToolConfiguration>,
    /// `requestMetadata`, after the labels Bedrock's charset rejects are
    /// dropped.
    pub(crate) request_metadata: HashMap<String, String>,
    /// Everything the conversion gave up.
    pub(crate) warnings: Vec<ResponseWarning>,
}

/// Limits and switches that differ per model family behind Bedrock.
///
/// They are configured on the builder for the same reason capabilities are:
/// Converse fronts several vendors, and asking the endpoint would answer for
/// none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireLimits {
    /// How many stop sequences this model accepts.
    pub(crate) max_stop_sequences: usize,
    /// Whether `requestMetadata` may be sent at all.
    pub(crate) send_request_metadata: bool,
}

/// Builds the Converse input for one normalized request.
///
/// # Errors
///
/// Returns a [`ProviderError`] carrying a [`CapabilityMismatch`] when the
/// request needs something the profile does not declare, or an
/// `InvalidRequest` when a block the SDK requires could not be assembled.
pub(crate) fn build_request(
    request: &ModelRequest,
    capabilities: &ProviderCapabilities,
    limits: WireLimits,
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

    let system_text = system_text(request, output.prompt_hint, &mut warnings);
    let mut converted = convert_messages(request, &mut warnings)?;
    let system = place_cache_breakpoints(
        system_text,
        &mut converted,
        &request.cache_hint,
        capabilities.prompt_caching,
        &mut warnings,
    );
    let tool_config = build_tool_config(request, output.tool, &mut warnings)?;

    Ok(ConvertedRequest {
        system,
        messages: converted.messages,
        inference: inference_config(
            request,
            capabilities,
            limits.max_stop_sequences,
            &mut warnings,
        ),
        tool_config,
        request_metadata: request_metadata(request, limits, &mut warnings),
        warnings,
    })
}

/// What the output specification became.
struct OutputConversion {
    /// The synthetic tool the schema travels in, when the transport is a forced
    /// function schema.
    tool: Option<OutputTool>,
    /// Text appended to the system prompt when the transport only describes the
    /// schema.
    prompt_hint: Option<String>,
}

/// Maps [`OutputSpec`] onto the transport the **declared capability** allows.
///
/// The mapping is driven by the declaration, never by what the caller asked
/// for: that is the whole no-silent-downgrade rule. A profile that declares
/// `PromptOnly` cannot be talked into forcing a tool by a request that sets
/// `strict: true`, and a profile that declares a transport Converse does not
/// have fails closed instead of improvising.
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
            // and `GrammarConstrained` are transports Converse does not have,
            // and the builder refuses to declare them. Reaching here means a
            // profile was assembled another way, so fail closed.
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

/// The one tool a structured answer travels in.
///
/// It is kept as a name and a schema rather than as a built
/// [`ToolSpecification`] so that assembling it — the only step that can fail —
/// happens in one place, alongside the caller's own tools.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OutputTool {
    /// The name `toolChoice` is pinned to.
    pub(crate) name: String,
    /// The schema that becomes the tool's `inputSchema`, unchanged.
    pub(crate) schema: Value,
}

/// Describes the one tool a structured answer travels in.
///
/// The tool's `inputSchema` **is** the caller's schema — not a description of
/// it, not a wrapper around it — which is what makes
/// [`StructuredOutputCapability::NativeFunctionSchema`] a true statement here.
/// The call is never executed; the runtime reads its input as the document
/// (spec §20.4).
pub(crate) fn declare_output_tool(name: &str, schema: &Value, declared: &[ToolSpec]) -> OutputTool {
    OutputTool {
        name: output_tool_name(name, declared),
        schema: schema.clone(),
    }
}

/// A tool name Converse accepts and no declared read tool already owns.
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
/// A system message that carries anything but text loses it: the `system` array
/// holds text blocks, and inventing a turn for the rest would change the shape
/// of the conversation.
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

/// Converts the conversation into Converse turns.
fn convert_messages(
    request: &ModelRequest,
    warnings: &mut Vec<ResponseWarning>,
) -> Result<ConvertedMessages, ProviderError> {
    let mut turns: Vec<(ConversationRole, Vec<ContentBlock>)> =
        Vec::with_capacity(request.messages.len());
    let mut turn_of_input: Vec<Option<usize>> = Vec::with_capacity(request.messages.len());
    let mut documents = 0usize;

    for message in &request.messages {
        if message.role == Role::System {
            turn_of_input.push(None);
            continue;
        }
        let blocks = convert_blocks(message, &mut documents, warnings);
        if blocks.is_empty() {
            // Converse rejects an empty content array, so a message carrying
            // nothing this adapter can express is not sent at all.
            turn_of_input.push(None);
            continue;
        }
        let role = wire_role(message.role);
        match turns.last_mut() {
            Some((last_role, content)) if *last_role == role => {
                content.extend(blocks);
                turn_of_input.push(Some(turns.len() - 1));
            }
            _ => {
                turns.push((role, blocks));
                turn_of_input.push(Some(turns.len() - 1));
            }
        }
    }

    let mut messages = Vec::with_capacity(turns.len());
    for (role, mut content) in turns {
        if role == ConversationRole::User {
            // Stable partition: results first, everything else in its order.
            content.sort_by_key(|block| u8::from(!block.is_tool_result()));
        }
        messages.push(
            WireMessage::builder()
                .role(role)
                .set_content(Some(content))
                .build()
                .map_err(|_| ProviderError::invalid_request("message_without_role"))?,
        );
    }
    Ok(ConvertedMessages {
        messages,
        turn_of_input,
    })
}

/// Converts one message's parts into blocks, results first within the message.
fn convert_blocks(
    message: &Message,
    documents: &mut usize,
    warnings: &mut Vec<ResponseWarning>,
) -> Vec<ContentBlock> {
    let mut results: Vec<ContentBlock> = Vec::new();
    let mut rest: Vec<ContentBlock> = Vec::new();
    let mut calls: Vec<ContentBlock> = Vec::new();
    for part in &message.content {
        match part {
            ContentPart::Text { text } if !text.is_empty() => {
                rest.push(ContentBlock::Text(text.clone()));
            }
            ContentPart::Text { .. } => {}
            ContentPart::Image { source } => match image_block(source) {
                Some(block) => rest.push(block),
                None => warnings.push(dropped("unsupported_image_source")),
            },
            ContentPart::Document { source } => match document_block(source, documents) {
                Some(block) => rest.push(block),
                None => warnings.push(dropped("unsupported_document_source")),
            },
            ContentPart::ToolCall(call) => {
                match ToolUseBlock::builder()
                    .tool_use_id(call.id.as_str())
                    .name(&call.name)
                    .input(to_document(&call.arguments))
                    .build()
                {
                    Ok(block) => calls.push(ContentBlock::ToolUse(block)),
                    Err(_) => warnings.push(dropped("tool_call_without_id")),
                }
            }
            ContentPart::ToolResult(result) => {
                let status = if result.is_error {
                    ToolResultStatus::Error
                } else {
                    ToolResultStatus::Success
                };
                match ToolResultBlock::builder()
                    .tool_use_id(result.call_id.as_str())
                    .content(ToolResultContentBlock::Text(result.content.clone()))
                    .status(status)
                    .build()
                {
                    Ok(block) => results.push(ContentBlock::ToolResult(block)),
                    Err(_) => warnings.push(dropped("tool_result_without_id")),
                }
            }
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
/// Converse demands an explicit format enum rather than a media type, so a
/// media type with no image format — a TIFF, an audio file — is dropped rather
/// than guessed at. A media type that is not `image/*` at all is dropped too:
/// the `document` block is reached through [`ContentPart::Document`] now, and
/// rerouting an image part into it would be guessing what the caller meant.
///
/// A URL is only expressible as an S3 location; `https://` has nowhere to go in
/// Converse, so it is dropped and reported.
fn image_block(source: &ImageSource) -> Option<ContentBlock> {
    match source {
        ImageSource::Base64 { media_type, data } => {
            if !media_type.starts_with(IMAGE_MEDIA_PREFIX) {
                return None;
            }
            let bytes = aws_smithy_types::base64::decode(data).ok()?;
            let format = image_format(media_type)?;
            let block = ImageBlock::builder()
                .format(format)
                .source(WireImageSource::Bytes(Blob::new(bytes)))
                .build()
                .ok()?;
            Some(ContentBlock::Image(block))
        }
        ImageSource::Url { url } => {
            let location = S3Location::builder().uri(s3_uri(url)?).build().ok()?;
            Some(ContentBlock::Image(
                ImageBlock::builder()
                    .format(image_format_from_path(url)?)
                    .source(WireImageSource::S3Location(location))
                    .build()
                    .ok()?,
            ))
        }
        // A future source this adapter cannot express is dropped and reported,
        // never guessed at.
        _ => None,
    }
}

/// Builds the `document` block for a document part.
///
/// Both forms state their media type, so the Converse format enum comes from
/// what the caller declared rather than from a file extension. A media type
/// Converse has no format for is dropped and reported.
fn document_block(source: &DocumentSource, documents: &mut usize) -> Option<ContentBlock> {
    let (format, wire) = match source {
        DocumentSource::Base64 { media_type, data } => {
            let bytes = aws_smithy_types::base64::decode(data).ok()?;
            (
                document_format(media_type)?,
                WireDocumentSource::Bytes(Blob::new(bytes)),
            )
        }
        DocumentSource::Url { url, media_type } => {
            let location = S3Location::builder().uri(s3_uri(url)?).build().ok()?;
            (
                document_format(media_type)?,
                WireDocumentSource::S3Location(location),
            )
        }
        _ => return None,
    };
    *documents += 1;
    let block = DocumentBlock::builder()
        .format(format)
        .name(document_name(*documents))
        .source(wire)
        .build()
        .ok()?;
    Some(ContentBlock::Document(block))
}

/// Returns the URL when it is an S3 URI Converse can fetch.
fn s3_uri(url: &str) -> Option<&str> {
    url.starts_with(S3_SCHEME).then_some(url)
}

/// The Converse image format for a media type, when there is one.
fn image_format(media_type: &str) -> Option<ImageFormat> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// The Converse image format implied by a URL's extension.
fn image_format_from_path(url: &str) -> Option<ImageFormat> {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let extension = path.rsplit('.').next()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// The Converse document format for a media type, when there is one.
fn document_format(media_type: &str) -> Option<DocumentFormat> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "application/pdf" => Some(DocumentFormat::Pdf),
        "text/csv" => Some(DocumentFormat::Csv),
        "text/html" => Some(DocumentFormat::Html),
        "text/markdown" => Some(DocumentFormat::Md),
        "text/plain" => Some(DocumentFormat::Txt),
        "application/msword" => Some(DocumentFormat::Doc),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(DocumentFormat::Docx)
        }
        "application/vnd.ms-excel" => Some(DocumentFormat::Xls),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            Some(DocumentFormat::Xlsx)
        }
        _ => None,
    }
}

/// A document name Converse accepts.
///
/// The API takes alphanumerics, single spaces, hyphens, parentheses and square
/// brackets — no underscores — and requires the name to be unique within a
/// request, so it is generated rather than taken from anything the caller sent.
fn document_name(ordinal: usize) -> String {
    format!("document-{ordinal}")
}

/// The role a normalized role travels under.
///
/// [`Role::System`] never reaches here — it is hoisted — and [`Role::Tool`] is
/// a user turn, because that is where Converse puts a tool result.
const fn wire_role(role: Role) -> ConversationRole {
    match role {
        Role::Assistant => ConversationRole::Assistant,
        Role::User | Role::Tool | Role::System => ConversationRole::User,
    }
}

/// Places the cache breakpoints a [`CacheHint`] asks for.
///
/// Converse marks a cacheable prefix with an explicit `cachePoint` block at the
/// end of what should be cached. A [`CacheHint::Prefix`] therefore marks the
/// end of the turn its last message landed in: when two input messages merged
/// into one turn the breakpoint covers a little more than was asked — never
/// less — which changes cost and never meaning.
fn place_cache_breakpoints(
    system: Option<String>,
    converted: &mut ConvertedMessages,
    hint: &CacheHint,
    prompt_caching: bool,
    warnings: &mut Vec<ResponseWarning>,
) -> Vec<SystemContentBlock> {
    let mut blocks: Vec<SystemContentBlock> =
        system.map(SystemContentBlock::Text).into_iter().collect();
    if matches!(hint, CacheHint::None) {
        return blocks;
    }
    if !prompt_caching {
        warnings.push(dropped("cache_hint"));
        return blocks;
    }
    if !blocks.is_empty()
        && let Some(point) = cache_point()
    {
        blocks.push(SystemContentBlock::CachePoint(point));
    }
    if let CacheHint::Prefix { messages } = hint {
        let turn = converted
            .turn_of_input
            .iter()
            .take(*messages)
            .filter_map(|turn| *turn)
            .next_back();
        if let Some(turn) = turn
            && let Some(message) = converted.messages.get_mut(turn)
            && let Some(point) = cache_point()
        {
            let mut content = message.content.clone();
            content.push(ContentBlock::CachePoint(point));
            message.content = content;
        }
    }
    blocks
}

/// The only cache lifetime this adapter asks for.
///
/// `None` is unreachable — the type is set on the line below — and a breakpoint
/// that could not be built is simply not placed, which costs money and never
/// meaning.
fn cache_point() -> Option<CachePointBlock> {
    CachePointBlock::builder()
        .r#type(CachePointType::Default)
        .build()
        .ok()
}

/// Assembles `toolConfig` from the declared tools and the output transport.
fn build_tool_config(
    request: &ModelRequest,
    output_tool: Option<OutputTool>,
    warnings: &mut Vec<ResponseWarning>,
) -> Result<Option<ToolConfiguration>, ProviderError> {
    let forced = output_tool.is_some();
    if !forced && request.tool_choice == ToolChoice::None {
        if !request.tools.is_empty() {
            // Converse has no "call nothing" mode. Declaring the tools and
            // hoping is not enforcement, so the tools are withheld instead:
            // that is what the caller actually asked for.
            warnings.push(dropped("tool_choice_none_as_no_tools"));
        }
        return Ok(None);
    }

    let mut tools: Vec<Tool> = Vec::with_capacity(request.tools.len() + 1);
    for tool in &request.tools {
        let spec = ToolSpecification::builder()
            .name(&tool.name)
            .description(&tool.description)
            .input_schema(ToolInputSchema::Json(to_document(&tool.parameters)))
            .build()
            .map_err(|_| ProviderError::invalid_request("tool_without_name"))?;
        tools.push(Tool::ToolSpec(spec));
    }

    let choice = match output_tool {
        Some(tool) => {
            if request.tool_choice != ToolChoice::Auto {
                // The transport owns the choice; saying so beats obeying a
                // caller who asked for a document and for a free hand at once.
                warnings.push(dropped("tool_choice"));
            }
            let spec = ToolSpecification::builder()
                .name(&tool.name)
                .description(OUTPUT_TOOL_DESCRIPTION)
                .input_schema(ToolInputSchema::Json(to_document(&tool.schema)))
                .build()
                .map_err(|_| ProviderError::invalid_request("output_tool_without_name"))?;
            tools.push(Tool::ToolSpec(spec));
            Some(WireToolChoice::Tool(
                SpecificToolChoice::builder()
                    .name(tool.name)
                    .build()
                    .map_err(|_| ProviderError::invalid_request("output_tool_without_name"))?,
            ))
        }
        None => convert_tool_choice(&request.tool_choice, warnings),
    };

    if tools.is_empty() {
        return Ok(None);
    }
    ToolConfiguration::builder()
        .set_tools(Some(tools))
        .set_tool_choice(choice)
        .build()
        .map(Some)
        .map_err(|_| ProviderError::invalid_request("tool_config_without_tools"))
}

/// Maps the tool-choice mode onto the three Converse has.
///
/// [`ToolChoice::None`] never reaches here: it is handled by withholding the
/// tools, which is the only way Converse can honour it.
fn convert_tool_choice(
    choice: &ToolChoice,
    warnings: &mut Vec<ResponseWarning>,
) -> Option<WireToolChoice> {
    match choice {
        ToolChoice::Auto => None,
        ToolChoice::Required => Some(WireToolChoice::Any(
            aws_sdk_bedrockruntime::types::AnyToolChoice::builder().build(),
        )),
        ToolChoice::Named { name } => SpecificToolChoice::builder()
            .name(name)
            .build()
            .ok()
            .map(WireToolChoice::Tool),
        // A mode this adapter cannot express is reported, never approximated.
        _ => {
            warnings.push(dropped("tool_choice"));
            None
        }
    }
}

/// Builds `inferenceConfig`, or `None` when the request constrains nothing.
fn inference_config(
    request: &ModelRequest,
    capabilities: &ProviderCapabilities,
    max_stop_sequences: usize,
    warnings: &mut Vec<ResponseWarning>,
) -> Option<InferenceConfiguration> {
    let sampling = request.sampling_for(capabilities);
    warnings.extend(sampling.dropped.iter().map(|name| dropped(name)));
    let stop = truncate_stop(&request.stop, max_stop_sequences, warnings);
    if request.max_output_tokens.is_none() && sampling.temperature.is_none() && stop.is_empty() {
        return None;
    }
    let mut builder = InferenceConfiguration::builder();
    if let Some(tokens) = request.max_output_tokens.filter(|tokens| *tokens > 0) {
        builder = builder.max_tokens(i32::try_from(tokens).unwrap_or(i32::MAX));
    }
    if let Some(temperature) = sampling.temperature {
        builder = builder.temperature(temperature);
    }
    if !stop.is_empty() {
        builder = builder.set_stop_sequences(Some(stop));
    }
    Some(builder.build())
}

/// Truncates the stop list to what the model accepts, warning when it does.
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

/// Maps the request's labels onto `requestMetadata`.
///
/// Bedrock accepts a narrower character set than [`RequestMetadata`] does, so a
/// label it would reject is dropped with a warning rather than sent and turned
/// into a 400 for the whole call.
///
/// [`RequestMetadata`]: turnframe_provider::request::RequestMetadata
fn request_metadata(
    request: &ModelRequest,
    limits: WireLimits,
    warnings: &mut Vec<ResponseWarning>,
) -> HashMap<String, String> {
    if !limits.send_request_metadata {
        if !request.metadata.is_empty() {
            warnings.push(dropped("request_metadata"));
        }
        return HashMap::new();
    }
    let mut out = HashMap::new();
    let mut rejected = false;
    for (key, value) in request.metadata.iter() {
        if is_bedrock_label(key) && is_bedrock_label(value) {
            out.insert(key.to_owned(), value.to_owned());
        } else {
            rejected = true;
        }
    }
    if rejected {
        warnings.push(dropped("metadata_label_charset"));
    }
    // The stable request id is the correlation hint Converse can carry
    // (spec §20.7); it is a UUID, so it always fits the charset.
    out.insert(REQUEST_ID_LABEL.to_owned(), request.request_id.to_string());
    out
}

/// Returns `true` when `text` fits Bedrock's `requestMetadata` charset.
fn is_bedrock_label(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 256
        && text.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    ':' | '_' | '@' | '$' | '#' | '=' | '/' | '+' | ',' | '-' | '.'
                )
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
    use turnframe_provider::ids::RequestId;
    use turnframe_provider::purpose::ModelPurpose;
    use turnframe_provider::request::{ToolCall, ToolResult};

    fn limits() -> WireLimits {
        WireLimits {
            max_stop_sequences: 4,
            send_request_metadata: true,
        }
    }

    fn capable() -> ProviderCapabilities {
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
            .with_tool_calling(ToolCallingCapability::Parallel)
            .with_vision(true)
            .with_documents(true)
            .with_prompt_caching(true)
    }

    fn schema() -> Value {
        json!({"type": "object", "properties": {"acts": {"type": "array"}}})
    }

    #[test]
    fn the_system_prompt_is_hoisted_out_of_the_conversation() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_system("Answer briefly.")
            .with_message(Message::system("And in Italian."))
            .with_message(Message::user("ciao"));
        let converted = build_request(&request, &capable(), limits()).unwrap();
        assert_eq!(converted.messages.len(), 1);
        assert_eq!(converted.system.len(), 1);
        let SystemContentBlock::Text(text) = &converted.system[0] else {
            panic!("the system prompt must be a text block");
        };
        assert_eq!(text, "Answer briefly.\n\nAnd in Italian.");
    }

    #[test]
    fn a_tool_result_becomes_a_user_turn_with_the_result_first() {
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::new(
                Role::Tool,
                vec![
                    ContentPart::text("ecco"),
                    ContentPart::ToolResult(ToolResult::ok("call_1", "{}")),
                ],
            ))
            .with_tools(vec![ToolSpec::new("read", "Reads.", json!({}))]);
        let converted = build_request(&request, &capable(), limits()).unwrap();
        let turn = &converted.messages[0];
        assert_eq!(turn.role, ConversationRole::User);
        assert!(turn.content[0].is_tool_result());
        assert!(turn.content[1].is_text());
    }

    #[test]
    fn consecutive_turns_with_the_same_role_are_merged() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("prima"))
            .with_message(Message::user("seconda"))
            .with_message(Message::assistant("ok"));
        let converted = build_request(&request, &capable(), limits()).unwrap();
        assert_eq!(converted.messages.len(), 2);
        assert_eq!(converted.messages[0].content.len(), 2);
        assert_eq!(converted.messages[1].role, ConversationRole::Assistant);
    }

    #[test]
    fn a_structured_request_forces_one_tool_carrying_the_schema() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("fai"))
            .with_output(OutputSpec::json("plan", schema()));
        let converted = build_request(&request, &capable(), limits()).unwrap();
        let config = converted.tool_config.expect("a tool config");
        assert_eq!(config.tools.len(), 1);
        let Tool::ToolSpec(spec) = &config.tools[0] else {
            panic!("the transport must be a tool spec");
        };
        assert_eq!(spec.name, "plan");
        let Some(ToolInputSchema::Json(document)) = &spec.input_schema else {
            panic!("the schema itself must travel");
        };
        assert_eq!(crate::wire::document::from_document(document), schema());
        let Some(WireToolChoice::Tool(choice)) = &config.tool_choice else {
            panic!("the choice must be pinned to the output tool");
        };
        assert_eq!(choice.name, "plan");
    }

    #[test]
    fn a_prompt_only_profile_describes_the_schema_and_forces_nothing() {
        let capabilities = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::PromptOnly);
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("fai"))
            .with_output(OutputSpec::json("plan", schema()));
        let converted = build_request(&request, &capabilities, limits()).unwrap();
        assert!(converted.tool_config.is_none());
        let SystemContentBlock::Text(text) = &converted.system[0] else {
            panic!("the hint must be a text block");
        };
        assert!(text.contains("JSON Schema"));
    }

    #[test]
    fn a_transport_converse_does_not_have_is_refused_rather_than_downgraded() {
        let capabilities = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeJsonSchema);
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("fai"))
            .with_output(OutputSpec::json("plan", schema()));
        let error = build_request(&request, &capabilities, limits()).unwrap_err();
        assert_eq!(error.kind().as_str(), "capability_mismatch");
    }

    #[test]
    fn the_output_tool_never_collides_with_a_declared_read_tool() {
        let declared = vec![ToolSpec::new("plan", "Reads.", json!({}))];
        assert_eq!(output_tool_name("plan", &declared), "plan_output");
        assert_eq!(output_tool_name("a b", &[]), "a_b");
        assert_eq!(output_tool_name("", &[]), FALLBACK_OUTPUT_TOOL_NAME);
    }

    #[test]
    fn an_image_needs_the_declaration_and_a_format_converse_knows() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::image_base64("image/png", "aGk=")],
        ));
        let blind = ProviderCapabilities::minimal();
        assert_eq!(
            build_request(&request, &blind, limits())
                .unwrap_err()
                .kind()
                .as_str(),
            "capability_mismatch"
        );
        let converted = build_request(&request, &capable(), limits()).unwrap();
        assert!(converted.messages[0].content[0].is_image());

        let tiff = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![
                ContentPart::text("guarda"),
                ContentPart::image_base64("image/tiff", "aGk="),
            ],
        ));
        let converted = build_request(&tiff, &capable(), limits()).unwrap();
        assert_eq!(converted.messages[0].content.len(), 1);
        assert!(
            converted
                .warnings
                .contains(&dropped("unsupported_image_source"))
        );
    }

    #[test]
    fn a_document_becomes_a_document_block_and_needs_its_own_declaration() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::document_base64("application/pdf", "aGk=")],
        ));

        // Reading a PNG says nothing about reading a PDF.
        let sighted = capable().with_documents(false);
        let error = build_request(&request, &sighted, limits()).unwrap_err();
        assert_eq!(error.kind().as_str(), "capability_mismatch");
        assert!(error.to_string().contains("documents"), "{error}");

        let converted = build_request(&request, &capable(), limits()).unwrap();
        let ContentBlock::Document(block) = &converted.messages[0].content[0] else {
            panic!("a document part is a document block");
        };
        assert_eq!(block.name, "document-1");
        assert_eq!(block.format, DocumentFormat::Pdf);
    }

    #[test]
    fn an_image_part_carrying_a_document_media_type_is_reported_not_reshaped() {
        // The workaround this adapter used to carry: a PDF arriving as an image
        // was rerouted into a document block. With a document part in the
        // vocabulary that guess is neither needed nor honest.
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![
                ContentPart::text("guarda"),
                ContentPart::image_base64("application/pdf", "aGk="),
            ],
        ));
        let converted = build_request(&request, &capable(), limits()).unwrap();
        assert_eq!(converted.messages[0].content.len(), 1);
        assert!(
            converted
                .warnings
                .contains(&dropped("unsupported_image_source"))
        );
    }

    #[test]
    fn only_an_s3_url_survives_as_an_attachment() {
        let s3 = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::image_url("s3://bucket/scan.png")],
        ));
        let converted = build_request(&s3, &capable(), limits()).unwrap();
        assert!(converted.messages[0].content[0].is_image());

        // A document reached the same way: the media type is stated, so no
        // extension has to be read off the key.
        let scanned = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::document_url(
                "s3://bucket/report",
                "application/pdf",
            )],
        ));
        let converted = build_request(&scanned, &capable(), limits()).unwrap();
        let ContentBlock::Document(block) = &converted.messages[0].content[0] else {
            panic!("an S3 document is a document block");
        };
        assert_eq!(block.format, DocumentFormat::Pdf);

        let https = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![
                ContentPart::text("guarda"),
                ContentPart::image_url("https://example.test/scan.png"),
            ],
        ));
        let converted = build_request(&https, &capable(), limits()).unwrap();
        assert_eq!(converted.messages[0].content.len(), 1);
    }

    #[test]
    fn tool_choice_none_withholds_the_tools_rather_than_hoping() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("ciao"))
            .with_tools(vec![ToolSpec::new("read", "Reads.", json!({}))])
            .with_tool_choice(ToolChoice::None);
        let converted = build_request(&request, &capable(), limits()).unwrap();
        assert!(converted.tool_config.is_none());
        assert!(
            converted
                .warnings
                .contains(&dropped("tool_choice_none_as_no_tools"))
        );
    }

    #[test]
    fn the_inference_configuration_carries_the_limits_the_caller_set() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("ciao"))
            .with_max_output_tokens(256)
            .with_temperature(0.2)
            .with_stop(vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "e".into(),
            ]);
        let converted =
            build_request(&request, &capable().with_temperature(true), limits()).unwrap();
        let inference = converted.inference.expect("an inference config");
        assert_eq!(inference.max_tokens, Some(256));
        assert_eq!(inference.temperature, Some(0.2));
        assert_eq!(inference.stop_sequences.as_ref().map(Vec::len), Some(4));
        assert!(
            converted
                .warnings
                .contains(&dropped("stop_sequences_over_limit"))
        );

        let bare = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("ciao"));
        assert!(
            build_request(&bare, &capable(), limits())
                .unwrap()
                .inference
                .is_none()
        );
    }

    #[test]
    fn the_request_id_travels_as_a_correlation_label() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_request_id(RequestId::nil())
            .with_message(Message::user("ciao"))
            .with_metadata("workflow", "trip")
            .unwrap();
        let converted = build_request(&request, &capable(), limits()).unwrap();
        assert_eq!(
            converted
                .request_metadata
                .get(REQUEST_ID_LABEL)
                .map(String::as_str),
            Some(RequestId::nil().to_string().as_str())
        );
        assert_eq!(
            converted
                .request_metadata
                .get("workflow")
                .map(String::as_str),
            Some("trip")
        );

        let silent = WireLimits {
            send_request_metadata: false,
            ..limits()
        };
        let converted = build_request(&request, &capable(), silent).unwrap();
        assert!(converted.request_metadata.is_empty());
        assert!(converted.warnings.contains(&dropped("request_metadata")));
    }

    #[test]
    fn a_label_bedrock_would_reject_is_dropped_rather_than_sent() {
        assert!(is_bedrock_label("turn-1.2"));
        assert!(!is_bedrock_label("tenant%1"));
        assert!(!is_bedrock_label(""));
    }

    #[test]
    fn a_cache_hint_places_a_breakpoint_only_where_caching_is_declared() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_system("stabile")
            .with_message(Message::user("prima"))
            .with_message(Message::assistant("ok"))
            .with_cache_hint(CacheHint::Prefix { messages: 1 });
        let converted = build_request(&request, &capable(), limits()).unwrap();
        assert!(matches!(
            converted.system.last(),
            Some(SystemContentBlock::CachePoint(_))
        ));
        assert!(matches!(
            converted.messages[0].content.last(),
            Some(ContentBlock::CachePoint(_))
        ));

        let blind = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::PromptOnly);
        let converted = build_request(&request, &blind, limits()).unwrap();
        assert_eq!(converted.system.len(), 1);
        assert!(converted.warnings.contains(&dropped("cache_hint")));
    }

    #[test]
    fn an_assistant_tool_call_round_trips_into_a_tool_use_block() {
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::new(
                Role::Assistant,
                vec![ContentPart::ToolCall(ToolCall::new(
                    "call_1",
                    "read",
                    json!({"target": "tok_1"}),
                ))],
            ))
            .with_tools(vec![ToolSpec::new("read", "Reads.", json!({}))]);
        let converted = build_request(&request, &capable(), limits()).unwrap();
        let ContentBlock::ToolUse(block) = &converted.messages[0].content[0] else {
            panic!("a call is a toolUse block");
        };
        assert_eq!(block.tool_use_id, "call_1");
        assert_eq!(block.name, "read");
    }
}
