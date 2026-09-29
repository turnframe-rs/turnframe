//! Normalized request → `generateContent` body.
//!
//! Gemini's request shape disagrees with the normalized one in four places, and
//! each disagreement is resolved explicitly here rather than by hoping the
//! names line up.
//!
//! **Framing instructions are a field, not a message.** `systemInstruction`
//! sits beside `contents`; there is no `system` role. A
//! [`Role::System`] message *inside* the conversation is therefore hoisted into
//! that field and its position is reported as dropped — putting framing
//! instructions into a `user` turn instead would be exactly the shape spec
//! §25.3 warns about.
//!
//! **The role vocabulary is different, and short.** Gemini knows `user` and
//! `model`. There is no `assistant` and no `tool`:
//!
//! | Normalized | Gemini | Note |
//! |---|---|---|
//! | [`Role::User`] | `user` | |
//! | [`Role::Assistant`] | `model` | |
//! | [`Role::Tool`] | `user` | A function *response* is something the caller tells the model, so it comes from the user side. |
//! | [`Role::System`] | — | Hoisted into `systemInstruction`. |
//!
//! **Tool calls are parts, not messages.** A `functionCall` is a part of a
//! `model` content and a `functionResponse` is a part of a `user` content, so
//! one normalized message becomes one content whatever it carries. Consecutive
//! contents with the same role are merged, because Gemini expects the two roles
//! to alternate and merging preserves both order and content.
//!
//! **A function response is addressed by name, not by id.** Gemini's REST
//! surface has no call id to correlate on, so the name is recovered from the
//! `functionCall` the result answers. A result whose call cannot be found is a
//! loud failure: inventing a name would send the model an answer to a question
//! it did not ask.
//!
//! # Dropped, warned, refused
//!
//! | Situation | What happens |
//! |---|---|
//! | A sixth stop sequence | dropped, warned |
//! | A cache hint | dropped, warned — explicit caching needs a `cachedContent` resource this adapter does not manage |
//! | Metadata on the developer API, or a label Vertex would reject | dropped, warned |
//! | The position of a `system` message | dropped, warned |
//! | An image on a profile that declares no vision | refused |
//! | A tool on a profile that declares no tool calling | refused |
//! | [`OutputSpec::Json`] on a profile whose transport cannot carry it | refused |
//! | A response schema Gemini's dialect cannot express | refused, naming the keyword |
//! | A response schema *and* tools on a surface that takes one or the other | refused |

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};
use turnframe_provider::capabilities::{
    CapabilityMismatch, MissingCapability, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::CallId;
use turnframe_provider::request::{
    CacheHint, ContentPart, DocumentSource, ImageSource, Message, ModelRequest, OutputSpec,
    ReasoningEffort, Role, ToolChoice, ToolResult, ToolSpec,
};
use turnframe_provider::response::ResponseWarning;

use crate::profile::{Quirks, SafetySetting};
use crate::schema::translate_response_schema;

/// The mime type that turns on JSON output.
pub(crate) const JSON_MIME_TYPE: &str = "application/json";

/// Key a failed tool result travels under inside `functionResponse.response`.
///
/// Gemini's function response is an object with no error flag, and a model that
/// cannot tell a failure from a value will narrate the failure as a result. A
/// dedicated key makes the distinction part of this adapter's contract rather
/// than an accident of formatting.
pub(crate) const TOOL_ERROR_KEY: &str = "error";

/// Key a non-object tool result is wrapped under.
pub(crate) const TOOL_OUTPUT_KEY: &str = "output";

/// The `functionCallingConfig.mode` that leaves the model no choice but to call
/// one of the named functions.
pub(crate) const FORCED_FUNCTION_MODE: &str = "ANY";

/// Description of the synthetic function a structured answer travels in.
///
/// The model reads it, so it says what the call is for in the words the model
/// needs, and nothing about the adapter.
pub(crate) const OUTPUT_FUNCTION_DESCRIPTION: &str = "Return the answer by calling this function exactly once. Its arguments are the answer; \
     there is nothing else to do with it.";

/// Name the output function takes when the caller's label sanitizes to nothing.
pub(crate) const FALLBACK_OUTPUT_FUNCTION_NAME: &str = "turnframe_structured_output";

/// Longest function name Gemini accepts.
pub(crate) const MAX_FUNCTION_NAME_LEN: usize = 64;

/// The `generateContent` request body.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerateContentRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) system_instruction: Option<Content>,
    pub(crate) contents: Vec<Content>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ToolDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_config: Option<ToolConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "<[SafetySetting]>::is_empty")]
    pub(crate) safety_settings: Vec<SafetySetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) labels: Option<BTreeMap<String, String>>,
}

/// One turn of the conversation, or the system instruction.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Content {
    /// `"user"` or `"model"`. Omitted on the system instruction, which has no
    /// speaker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<&'static str>,
    pub(crate) parts: Vec<Part>,
}

/// One part of a content. Gemini models this as a `oneof`, so exactly one field
/// is present per part.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Part {
    Text(String),
    InlineData(Blob),
    FileData(FileData),
    FunctionCall(FunctionCall),
    FunctionResponse(FunctionResponse),
}

/// Bytes carried in the request.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Blob {
    pub(crate) mime_type: String,
    /// Base64, without a data-URI prefix.
    pub(crate) data: String,
}

/// Bytes the service fetches itself, from Cloud Storage or the Files API.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mime_type: Option<String>,
    pub(crate) file_uri: String,
}

/// A call the model made, echoed back into the conversation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct FunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    pub(crate) args: Value,
}

/// The result of a call, sent back to the model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct FunctionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    /// Always an object: Gemini's field is a `Struct`.
    pub(crate) response: Value,
}

/// The single `tools` entry this adapter sends.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolDeclaration {
    pub(crate) function_declarations: Vec<FunctionDeclaration>,
}

/// One declared function.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct FunctionDeclaration {
    pub(crate) name: String,
    pub(crate) description: String,
    /// Omitted for a function that takes no arguments; Gemini rejects an empty
    /// schema object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parameters: Option<Value>,
}

/// `toolConfig`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolConfig {
    pub(crate) function_calling_config: FunctionCallingConfig,
}

/// How free the model is to call a function.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FunctionCallingConfig {
    pub(crate) mode: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) allowed_function_names: Vec<String>,
}

/// `generationConfig`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_mime_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thinking_config: Option<ThinkingConfig>,
}

impl GenerationConfig {
    /// Returns `true` when nothing was configured, so the field can be omitted.
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// `generationConfig.thinkingConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThinkingConfig {
    pub(crate) thinking_budget: i32,
}

/// The thinking budget for an effort. `Minimal` is `0`, which only models that can stop
/// thinking accept, so declare `reasoning_controls` for those models alone.
fn thinking_budget_for(effort: ReasoningEffort) -> i32 {
    match effort {
        ReasoningEffort::Minimal => 0,
        ReasoningEffort::Low => 1_024,
        ReasoningEffort::Medium => 8_192,
        _ => 24_576,
    }
}

/// A converted request and everything the conversion had to give up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConvertedRequest {
    pub(crate) body: GenerateContentRequest,
    pub(crate) warnings: Vec<ResponseWarning>,
}

/// Everything the adapter configuration contributes to a request body.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RequestOptions<'a> {
    pub(crate) capabilities: &'a ProviderCapabilities,
    pub(crate) quirks: Quirks,
    pub(crate) safety_settings: &'a [SafetySetting],
    pub(crate) thinking_budget: Option<i32>,
}

/// Builds the `generateContent` body for one normalized request.
///
/// The model is not in the body: both surfaces put it in the URL.
///
/// # Errors
///
/// Returns a [`ProviderError`] carrying a [`CapabilityMismatch`] when the
/// request needs something the profile does not declare, an
/// [`Unsupported`](turnframe_provider::error::ProviderErrorKind::Unsupported)
/// failure when the response schema cannot be expressed in Gemini's dialect or
/// cannot travel beside tools, and an
/// [`InvalidRequest`](turnframe_provider::error::ProviderErrorKind::InvalidRequest)
/// when a tool result answers a call that is nowhere in the conversation.
pub(crate) fn build_request(
    request: &ModelRequest,
    options: RequestOptions<'_>,
) -> Result<ConvertedRequest, ProviderError> {
    let mut warnings = Vec::new();
    let mut missing = Vec::new();

    if request.messages.iter().any(Message::has_image) && !options.capabilities.vision {
        missing.push(MissingCapability::Vision);
    }
    if request.messages.iter().any(Message::has_document) && !options.capabilities.documents {
        missing.push(MissingCapability::Documents);
    }
    if !request.tools.is_empty() && !options.capabilities.supports_tools() {
        missing.push(MissingCapability::ToolCalling);
    }
    let output = output_config(request, options, &mut missing)?;
    if !missing.is_empty() {
        return Err(ProviderError::capability_mismatch(CapabilityMismatch {
            missing,
        }));
    }

    let names = call_names(request);
    let contents = convert_contents(request, &names, &mut warnings)?;
    if contents.is_empty() {
        return Err(ProviderError::invalid_request("empty_contents"));
    }
    let system_instruction = system_instruction(request, output.prompt_hint, &mut warnings);

    let mut tools = convert_tools(request);
    let tool_config = match output.function {
        Some(function) => {
            if request.tool_choice != ToolChoice::Auto {
                // The transport owns the choice; saying so beats obeying a
                // caller who asked for a document and for a free hand at once.
                warnings.push(dropped("tool_choice"));
            }
            let name = function.name.clone();
            match tools.first_mut() {
                Some(declaration) => declaration.function_declarations.push(function),
                None => tools.push(ToolDeclaration {
                    function_declarations: vec![function],
                }),
            }
            Some(ToolConfig {
                function_calling_config: FunctionCallingConfig {
                    mode: FORCED_FUNCTION_MODE,
                    // Named, not merely required: `ANY` on its own would let
                    // the model answer with a read tool instead of the
                    // document, and a transport that can be dodged is not one.
                    allowed_function_names: vec![name],
                },
            })
        }
        None => convert_tool_choice(&request.tool_choice, tools.is_empty()),
    };
    let stop_sequences = truncate_stop(
        &request.stop,
        options.quirks.max_stop_sequences,
        &mut warnings,
    );

    if !matches!(request.cache_hint, CacheHint::None) {
        // Gemini's explicit caching addresses a `cachedContent` resource that
        // has to be created and paid for out of band; a hint cannot conjure
        // one. Implicit caching still happens, and is reported as
        // `TokenUsage::cached_input`.
        warnings.push(dropped("cache_hint"));
    }
    let labels = convert_labels(request, options.quirks, &mut warnings);

    let sampling = request.sampling_for(options.capabilities);
    warnings.extend(sampling.dropped.iter().map(|name| dropped(name)));
    // A turn's reasoning effort wins over the builder's standing budget.
    let thinking_budget = sampling
        .reasoning_effort
        .map(thinking_budget_for)
        .or(options.thinking_budget);
    let generation_config = GenerationConfig {
        temperature: sampling.temperature,
        seed: sampling.seed,
        max_output_tokens: request.max_output_tokens,
        stop_sequences,
        response_mime_type: output.mime_type,
        response_schema: output.schema,
        thinking_config: thinking_budget
            .filter(|_| options.quirks.send_thinking_config)
            .map(|thinking_budget| ThinkingConfig { thinking_budget }),
    };
    if thinking_budget.is_some() && !options.quirks.send_thinking_config {
        warnings.push(dropped("thinking_config"));
    }

    Ok(ConvertedRequest {
        body: GenerateContentRequest {
            system_instruction,
            contents,
            tools,
            tool_config,
            generation_config: (!generation_config.is_empty()).then_some(generation_config),
            safety_settings: options.safety_settings.to_vec(),
            labels,
        },
        warnings,
    })
}

/// What the output specification became.
struct OutputConversion {
    mime_type: Option<&'static str>,
    schema: Option<Value>,
    /// Text appended to the system instruction when the transport describes the
    /// schema rather than enforcing it.
    prompt_hint: Option<String>,
    /// The synthetic function the schema travels in, when the transport is a
    /// forced function schema.
    function: Option<FunctionDeclaration>,
}

/// Maps [`OutputSpec`] onto `responseMimeType`, `responseSchema` and, for a
/// weaker transport, a prompt hint.
///
/// The mapping is driven by the **declared capability**, never by what the
/// caller asked for: that is the whole no-silent-downgrade rule (spec §0 rule
/// 9). A profile declaring `JsonObject` cannot be talked into sending a schema
/// by a request that sets `strict: true`, and a profile declaring
/// `NativeJsonSchema` never sends a *partial* schema — it either carries the
/// whole thing or fails.
fn output_config(
    request: &ModelRequest,
    options: RequestOptions<'_>,
    missing: &mut Vec<MissingCapability>,
) -> Result<OutputConversion, ProviderError> {
    let empty = OutputConversion {
        mime_type: None,
        schema: None,
        prompt_hint: None,
        function: None,
    };
    let OutputSpec::Json {
        schema,
        name,
        strict,
    } = &request.output
    else {
        return Ok(empty);
    };
    // Gemini enforces a response schema unconditionally; there is no
    // best-effort mode to fall back to when `strict` is false. Enforcing more
    // than the caller asked for is not a downgrade, so it needs no warning.
    let _ = strict;

    let converted = match options.capabilities.structured_output {
        StructuredOutputCapability::NativeJsonSchema => {
            let mut translated = translate_response_schema(schema, options.quirks.schema_dialect)?;
            // The dialect has no name field, so the caller's label becomes the
            // schema title when it has none. Nothing is lost either way.
            if let Some(object) = translated.as_object_mut()
                && !object.contains_key("title")
                && !name.is_empty()
            {
                object.insert("title".to_owned(), Value::String(name.clone()));
            }
            OutputConversion {
                mime_type: Some(JSON_MIME_TYPE),
                schema: Some(translated),
                prompt_hint: None,
                function: None,
            }
        }
        // The same schema, carried the other way Gemini can enforce it: as one
        // function declaration with `functionCallingConfig` pinned to it. The
        // schema is translated exactly as the response-schema transport
        // translates it, because the declaration says the *schema* is enforced
        // — sending a dialect the decoder does not constrain on would make
        // `native_function_schema` a claim about nothing.
        StructuredOutputCapability::NativeFunctionSchema => OutputConversion {
            mime_type: None,
            schema: None,
            prompt_hint: None,
            function: Some(declare_output_function(
                name,
                &translate_response_schema(schema, options.quirks.schema_dialect)?,
                &request.tools,
            )),
        },
        StructuredOutputCapability::JsonObject => OutputConversion {
            mime_type: Some(JSON_MIME_TYPE),
            schema: None,
            prompt_hint: Some(schema_hint(schema)),
            function: None,
        },
        StructuredOutputCapability::PromptOnly => OutputConversion {
            mime_type: None,
            schema: None,
            prompt_hint: Some(schema_hint(schema)),
            function: None,
        },
        declared => {
            // `None` has no transport, and `GrammarConstrained` is one this
            // adapter does not send; the builder refuses to declare either.
            // Reaching here means a profile was assembled another way, so fail
            // closed.
            missing.push(MissingCapability::StructuredOutput {
                required: vec![
                    StructuredOutputCapability::NativeJsonSchema,
                    StructuredOutputCapability::NativeFunctionSchema,
                    StructuredOutputCapability::JsonObject,
                    StructuredOutputCapability::PromptOnly,
                ],
                declared,
            });
            return Ok(empty);
        }
    };

    // Only the response-schema transport collides with `tools`: the forced
    // function *is* a tool, so a surface that takes one or the other still
    // takes it. That is the practical reason to declare the function transport
    // on an older surface rather than losing the schema.
    if converted.mime_type.is_some()
        && !request.tools.is_empty()
        && !options.quirks.structured_output_with_tools
    {
        // One of the two has to go, and both choices are wrong: dropping the
        // schema is the silent downgrade rule 9 forbids, and dropping the tools
        // changes what the model is able to do. So neither happens.
        return Err(ProviderError::unsupported("structured_output_with_tools"));
    }
    Ok(converted)
}

/// Builds the one function a structured answer travels in.
///
/// The declaration's `parameters` **is** the caller's schema, translated into
/// Gemini's dialect — not a description of it and not a wrapper around it —
/// which is what makes
/// [`StructuredOutputCapability::NativeFunctionSchema`] a true statement here:
/// the model fills that schema in, and the runtime reads the `functionCall`
/// arguments as the document. The call is never executed and never authorizes
/// anything (spec §21.4); it is a shipping container for JSON.
pub(crate) fn declare_output_function(
    name: &str,
    schema: &Value,
    declared: &[ToolSpec],
) -> FunctionDeclaration {
    FunctionDeclaration {
        name: output_function_name(name, declared),
        description: OUTPUT_FUNCTION_DESCRIPTION.to_owned(),
        // Always sent, even for a schema with no properties: the pinned name is
        // what forces the call, and an absent `parameters` would be a function
        // that takes no arguments — an empty document rather than the answer.
        parameters: Some(schema.clone()),
    }
}

/// A function name Gemini accepts and no declared read tool already owns.
///
/// Gemini takes a name of letters, digits, underscores, dots and dashes, so
/// anything else becomes `_`. A collision with a read tool would make the
/// pinned choice ambiguous — the model could satisfy `ANY` by calling the read
/// tool — so it is resolved by suffixing rather than by hoping.
pub(crate) fn output_function_name(requested: &str, declared: &[ToolSpec]) -> String {
    let sanitized: String = requested
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .take(MAX_FUNCTION_NAME_LEN)
        .collect();
    let base = if sanitized.is_empty() {
        FALLBACK_OUTPUT_FUNCTION_NAME.to_owned()
    } else {
        sanitized
    };
    let taken = |candidate: &str| declared.iter().any(|tool| tool.name == candidate);
    if !taken(&base) {
        return base;
    }
    for attempt in 0..100_u32 {
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
    FALLBACK_OUTPUT_FUNCTION_NAME.to_owned()
}

/// Appends `suffix`, trimming `base` so the result still fits.
fn with_suffix(base: &str, suffix: &str) -> String {
    let room = MAX_FUNCTION_NAME_LEN.saturating_sub(suffix.len());
    let head: String = base.chars().take(room).collect();
    format!("{head}{suffix}")
}

/// The instruction appended for a transport that describes rather than
/// enforces. It names JSON explicitly, which is also what makes a bare
/// `application/json` response mime type behave.
fn schema_hint(schema: &Value) -> String {
    format!(
        "Reply with a single JSON document and nothing else. \
         It must satisfy this JSON Schema:\n{schema}"
    )
}

/// Builds `systemInstruction` from the request's own system prompt, any
/// `system`-role message, and the transport's hint.
fn system_instruction(
    request: &ModelRequest,
    hint: Option<String>,
    warnings: &mut Vec<ResponseWarning>,
) -> Option<Content> {
    let mut pieces: Vec<String> = Vec::new();
    if let Some(system) = request.system.as_deref().filter(|text| !text.is_empty()) {
        pieces.push(system.to_owned());
    }
    let mut hoisted = false;
    for message in &request.messages {
        if message.role == Role::System {
            let text = message.text();
            if !text.is_empty() {
                pieces.push(text);
            }
            hoisted = true;
        }
    }
    if hoisted {
        // The text travels; where it sat in the conversation does not, because
        // Gemini has no system role in `contents` and a framing instruction
        // wearing a user's voice is the injection shape §25.3 warns about.
        warnings.push(dropped("system_message_position"));
    }
    if let Some(hint) = hint {
        pieces.push(hint);
    }
    if pieces.is_empty() {
        return None;
    }
    Some(Content {
        role: None,
        parts: vec![Part::Text(pieces.join("\n\n"))],
    })
}

/// The name of every tool call in the conversation, by id.
fn call_names(request: &ModelRequest) -> BTreeMap<CallId, String> {
    let mut names = BTreeMap::new();
    for message in &request.messages {
        for part in &message.content {
            if let ContentPart::ToolCall(call) = part {
                names.insert(call.id.clone(), call.name.clone());
            }
        }
    }
    names
}

/// Converts the conversation, merging consecutive contents of the same role.
fn convert_contents(
    request: &ModelRequest,
    names: &BTreeMap<CallId, String>,
    warnings: &mut Vec<ResponseWarning>,
) -> Result<Vec<Content>, ProviderError> {
    let mut out: Vec<Content> = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        if message.role == Role::System {
            // Already hoisted into `systemInstruction`.
            continue;
        }
        let role = wire_role(message.role);
        let mut parts = Vec::with_capacity(message.content.len());
        for part in &message.content {
            if let Some(converted) = convert_part(part, request, names, warnings)? {
                parts.push(converted);
            }
        }
        if parts.is_empty() {
            continue;
        }
        // Gemini expects the two roles to alternate; merging preserves both the
        // order of the parts and their content, so nothing is lost.
        match out.last_mut() {
            Some(last) if last.role == Some(role) => last.parts.append(&mut parts),
            _ => out.push(Content {
                role: Some(role),
                parts,
            }),
        }
    }
    Ok(out)
}

/// The Gemini role a normalized role travels under.
///
/// `System` never reaches here: it is hoisted before conversion.
const fn wire_role(role: Role) -> &'static str {
    match role {
        Role::Assistant => "model",
        // A function response is something the caller tells the model, so it
        // comes from the user side of the conversation.
        Role::System | Role::User | Role::Tool => "user",
    }
}

/// Converts one content part, or returns `None` for a part with no equivalent.
fn convert_part(
    part: &ContentPart,
    request: &ModelRequest,
    names: &BTreeMap<CallId, String>,
    warnings: &mut Vec<ResponseWarning>,
) -> Result<Option<Part>, ProviderError> {
    let converted = match part {
        ContentPart::Text { text } if text.is_empty() => None,
        ContentPart::Text { text } => Some(Part::Text(text.clone())),
        ContentPart::Image { source } => Some(convert_image(source, warnings)),
        ContentPart::Document { source } => Some(convert_document(source, warnings)),
        ContentPart::ToolCall(call) => Some(Part::FunctionCall(FunctionCall {
            id: (!call.id.is_empty()).then(|| call.id.as_str().to_owned()),
            name: call.name.clone(),
            args: call.arguments.clone(),
        })),
        ContentPart::ToolResult(result) => Some(Part::FunctionResponse(function_response(
            result, request, names,
        )?)),
        // The part vocabulary is growable; an adapter that cannot express a
        // future part must not silently invent one.
        _ => {
            warnings.push(dropped("unknown_content_part"));
            None
        }
    };
    Ok(converted)
}

/// Inline bytes become a `Blob`; a URI becomes `FileData`.
///
/// Gemini does not fetch arbitrary `http(s)` URLs: a `fileUri` is a Cloud
/// Storage object or a Files API resource. A URL source is passed through
/// unchanged so the service can say so itself, rather than being silently
/// dropped here.
fn convert_image(source: &ImageSource, warnings: &mut Vec<ResponseWarning>) -> Part {
    match source {
        ImageSource::Base64 { media_type, data } => Part::InlineData(Blob {
            mime_type: media_type.clone(),
            data: data.clone(),
        }),
        ImageSource::Url { url } => {
            let mime_type = guess_mime_type(url);
            if mime_type.is_none() {
                warnings.push(dropped("file_data_mime_type"));
            }
            Part::FileData(FileData {
                mime_type,
                file_uri: url.clone(),
            })
        }
        _ => {
            warnings.push(dropped("unknown_image_source"));
            Part::Text(String::new())
        }
    }
}

/// Inline document bytes become a `Blob`; a referenced one becomes `FileData`.
///
/// Gemini takes a PDF the same way it takes an image — as `inlineData` with the
/// media type — so the conversion is the image one without the guessing: both
/// forms of a document state their media type, and a `fileUri` no longer has to
/// have its type read off the path.
fn convert_document(source: &DocumentSource, warnings: &mut Vec<ResponseWarning>) -> Part {
    match source {
        DocumentSource::Base64 { media_type, data } => Part::InlineData(Blob {
            mime_type: media_type.clone(),
            data: data.clone(),
        }),
        DocumentSource::Url { url, media_type } => Part::FileData(FileData {
            mime_type: Some(media_type.clone()),
            file_uri: url.clone(),
        }),
        _ => {
            warnings.push(dropped("unknown_document_source"));
            Part::Text(String::new())
        }
    }
}

/// The media type a URI's extension implies, when it implies one.
fn guess_mime_type(uri: &str) -> Option<String> {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    let mime = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "pdf" => "application/pdf",
        _ => return None,
    };
    Some(mime.to_owned())
}

/// Builds a `functionResponse`, recovering the function's name.
///
/// Gemini addresses a result by name; the normalized layer addresses it by id.
/// The name comes from the call the result answers, or — for a caller that
/// never had an id to preserve — from a declared tool whose name *is* the id.
/// Neither working is a failure, not a guess: answering a question the model
/// did not ask is worse than refusing the turn.
fn function_response(
    result: &ToolResult,
    request: &ModelRequest,
    names: &BTreeMap<CallId, String>,
) -> Result<FunctionResponse, ProviderError> {
    let name = names
        .get(&result.call_id)
        .cloned()
        .or_else(|| {
            request
                .tools
                .iter()
                .find(|tool| tool.name == result.call_id.as_str())
                .map(|tool| tool.name.clone())
        })
        .ok_or_else(|| ProviderError::invalid_request("tool_result_without_matching_call"))?;
    Ok(FunctionResponse {
        id: (!result.call_id.is_empty()).then(|| result.call_id.as_str().to_owned()),
        name,
        response: tool_response_object(result),
    })
}

/// Shapes a tool result into the object Gemini's `response` field requires.
fn tool_response_object(result: &ToolResult) -> Value {
    let parsed: Option<Value> = serde_json::from_str(&result.content).ok();
    let key = if result.is_error {
        TOOL_ERROR_KEY
    } else {
        TOOL_OUTPUT_KEY
    };
    match parsed {
        // A successful object result travels as itself, which is the shape the
        // model was shown when the tool was declared.
        Some(Value::Object(object)) if !result.is_error => Value::Object(object),
        Some(value) => {
            let mut wrapper = Map::new();
            wrapper.insert(key.to_owned(), value);
            Value::Object(wrapper)
        }
        None => {
            let mut wrapper = Map::new();
            wrapper.insert(key.to_owned(), Value::String(result.content.clone()));
            Value::Object(wrapper)
        }
    }
}

/// Declares the read-only tools, as the one `tools` entry Gemini takes.
fn convert_tools(request: &ModelRequest) -> Vec<ToolDeclaration> {
    if request.tools.is_empty() {
        return Vec::new();
    }
    let function_declarations = request
        .tools
        .iter()
        .map(|tool| FunctionDeclaration {
            name: tool.name.clone(),
            description: tool.description.clone(),
            // A tool schema is not a response schema: Gemini is far more
            // forgiving here, and rejecting a caller's parameter schema over a
            // keyword the decoder never enforces would be officious. An empty
            // schema means "no arguments", which the field's absence says.
            parameters: is_empty_schema(&tool.parameters).then_some(tool.parameters.clone()),
        })
        .collect();
    vec![ToolDeclaration {
        function_declarations,
    }]
}

/// Returns `true` when a parameter schema says something, so it is worth
/// sending.
fn is_empty_schema(schema: &Value) -> bool {
    !matches!(schema, Value::Object(object) if object.is_empty()) && !schema.is_null()
}

/// Maps the tool-choice mode. `Auto` is Gemini's own default, so it is omitted;
/// every mode is omitted when no tool is declared, because `toolConfig` without
/// `tools` is a request the service rejects.
fn convert_tool_choice(choice: &ToolChoice, no_tools: bool) -> Option<ToolConfig> {
    if no_tools {
        return None;
    }
    let config = match choice {
        ToolChoice::Auto => return None,
        ToolChoice::None => FunctionCallingConfig {
            mode: "NONE",
            allowed_function_names: Vec::new(),
        },
        ToolChoice::Required => FunctionCallingConfig {
            mode: "ANY",
            allowed_function_names: Vec::new(),
        },
        ToolChoice::Named { name } => FunctionCallingConfig {
            mode: "ANY",
            allowed_function_names: vec![name.clone()],
        },
        _ => return None,
    };
    Some(ToolConfig {
        function_calling_config: config,
    })
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

/// Maps request metadata onto Vertex's `labels`, or explains why it did not.
///
/// Vertex enforces a narrow grammar on labels and rejects the whole request
/// over one bad entry, so the map travels whole or not at all.
fn convert_labels(
    request: &ModelRequest,
    quirks: Quirks,
    warnings: &mut Vec<ResponseWarning>,
) -> Option<BTreeMap<String, String>> {
    if request.metadata.is_empty() {
        return None;
    }
    if !quirks.send_labels {
        warnings.push(dropped("metadata"));
        return None;
    }
    let labels: BTreeMap<String, String> = request
        .metadata
        .iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    if labels
        .iter()
        .all(|(key, value)| is_label_key(key) && is_label_value(value))
    {
        Some(labels)
    } else {
        warnings.push(dropped("metadata_labels_unsafe"));
        None
    }
}

/// Vertex label keys: a lowercase letter, then up to 62 more label characters.
fn is_label_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && key.len() <= 63
        && key.chars().all(is_label_char)
}

/// Vertex label values: up to 63 label characters, possibly none.
fn is_label_value(value: &str) -> bool {
    value.len() <= 63 && value.chars().all(is_label_char)
}

/// One character Vertex accepts inside a label.
const fn is_label_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_')
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
    use turnframe_provider::request::{Message, ToolCall, ToolSpec};

    fn caps() -> ProviderCapabilities {
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
            .with_tool_calling(ToolCallingCapability::Parallel)
            .with_vision(true)
            .with_documents(true)
            .with_streaming(true)
    }

    fn options<'a>(
        capabilities: &'a ProviderCapabilities,
        quirks: &'a Quirks,
    ) -> RequestOptions<'a> {
        RequestOptions {
            capabilities,
            quirks: *quirks,
            safety_settings: &[],
            thinking_budget: None,
        }
    }

    fn convert(request: &ModelRequest) -> ConvertedRequest {
        let capabilities = caps();
        let quirks = Quirks::gemini();
        build_request(request, options(&capabilities, &quirks)).expect("converts")
    }

    fn body(request: &ModelRequest) -> Value {
        serde_json::to_value(convert(request).body).expect("serializes")
    }

    #[test]
    fn a_reasoning_effort_becomes_a_thinking_budget_where_declared() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("ciao"))
            .with_temperature(0.0)
            .with_seed(9)
            .with_reasoning_effort(ReasoningEffort::Minimal);
        let thinking = caps()
            .with_temperature(true)
            .with_seed(true)
            .with_reasoning_controls(true);
        let quirks = Quirks::gemini();
        let converted = build_request(&request, options(&thinking, &quirks)).expect("converts");
        let body = serde_json::to_value(&converted.body).expect("serializes");
        let config = &body["generationConfig"];
        assert_eq!(config["temperature"], 0.0);
        assert_eq!(config["seed"], 9);
        if quirks.send_thinking_config {
            assert_eq!(config["thinkingConfig"]["thinkingBudget"], 0);
        }

        let plain = caps().with_temperature(true);
        let converted = build_request(&request, options(&plain, &quirks)).expect("converts");
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert!(
            body["generationConfig"].get("thinkingConfig").is_none(),
            "{body}"
        );
        assert!(body["generationConfig"].get("seed").is_none(), "{body}");
        assert!(converted.warnings.contains(&dropped("reasoning_effort")));
        assert!(converted.warnings.contains(&dropped("seed")));
    }

    fn function_caps() -> ProviderCapabilities {
        caps().with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
    }

    fn convert_with(
        request: &ModelRequest,
        capabilities: &ProviderCapabilities,
    ) -> ConvertedRequest {
        let quirks = Quirks::gemini();
        build_request(request, options(capabilities, &quirks)).expect("converts")
    }

    fn plan_request() -> ModelRequest {
        ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("sposta il volo"))
            .with_output(OutputSpec::json(
                "plan",
                json!({
                    "type": "object",
                    "properties": {"acts": {"type": "array", "items": {"type": "string"}}},
                    "required": ["acts"]
                }),
            ))
    }

    #[test]
    fn a_schema_travels_as_the_parameters_of_one_pinned_function() {
        let capabilities = function_caps();
        let wire = serde_json::to_value(convert_with(&plan_request(), &capabilities).body)
            .expect("serializes");

        let declarations = wire["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("declarations");
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0]["name"], "plan");
        // Translated into the dialect, exactly as the response-schema transport
        // translates it: the declaration says the schema is enforced.
        assert_eq!(declarations[0]["parameters"]["type"], "OBJECT");
        assert_eq!(
            wire["toolConfig"]["functionCallingConfig"]["mode"],
            FORCED_FUNCTION_MODE
        );
        assert_eq!(
            wire["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
            json!(["plan"])
        );
        // And the other transport is not sent alongside it.
        assert!(wire["generationConfig"].get("responseSchema").is_none());
        assert!(wire["generationConfig"].get("responseMimeType").is_none());
    }

    #[test]
    fn the_pinned_function_joins_the_read_tools_without_colliding_with_one() {
        let capabilities = function_caps();
        let request = plan_request().with_tools(vec![ToolSpec::new(
            "plan",
            "a read tool that already owns the name",
            json!({"type": "object", "properties": {}}),
        )]);
        let converted = convert_with(&request, &capabilities);
        let wire = serde_json::to_value(converted.body).expect("serializes");

        let declarations = wire["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("declarations");
        assert_eq!(declarations.len(), 2, "the read tool and the output one");
        assert_eq!(declarations[0]["name"], "plan");
        assert_eq!(declarations[1]["name"], "plan_output");
        // The pin names the output function, never the read tool: a choice the
        // model could satisfy with a read is not a transport.
        assert_eq!(
            wire["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
            json!(["plan_output"])
        );
    }

    #[test]
    fn the_transport_owns_the_tool_choice_and_says_so() {
        let capabilities = function_caps();
        let converted = convert_with(
            &plan_request().with_tool_choice(ToolChoice::None),
            &capabilities,
        );
        assert!(converted.warnings.contains(&dropped("tool_choice")));
        let wire = serde_json::to_value(converted.body).expect("serializes");
        assert_eq!(wire["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
    }

    #[test]
    fn a_function_name_the_endpoint_would_reject_is_sanitized() {
        assert_eq!(
            output_function_name("user turn/plan", &[]),
            "user_turn_plan"
        );
        assert_eq!(output_function_name("", &[]), FALLBACK_OUTPUT_FUNCTION_NAME);
        let long = "n".repeat(200);
        assert_eq!(
            output_function_name(&long, &[]).len(),
            MAX_FUNCTION_NAME_LEN
        );
        // A name still free is left alone.
        let taken = [ToolSpec::new("other", "d", json!({}))];
        assert_eq!(output_function_name("plan", &taken), "plan");
    }

    #[test]
    fn a_schema_the_dialect_cannot_express_is_refused_on_this_transport_too() {
        // The same refusal as the response-schema transport: sending a schema
        // the decoder will not constrain on would make the declaration false.
        let capabilities = function_caps();
        let quirks = Quirks::gemini();
        let request = plan_request().with_output(OutputSpec::json(
            "plan",
            json!({"oneOf": [{"type": "object"}, {"type": "object"}]}),
        ));
        let error = build_request(&request, options(&capabilities, &quirks))
            .expect_err("an overlapping union has no equivalent");
        assert!(error.to_string().contains("oneOf"), "{error}");
    }

    #[test]
    fn the_role_vocabulary_is_mapped_explicitly() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("ciao"))
            .with_message(Message::assistant("salve"))
            .with_message(Message::user("ancora"));
        let wire = body(&request);
        assert_eq!(wire["contents"][0]["role"], "user");
        assert_eq!(wire["contents"][1]["role"], "model");
        assert_eq!(wire["contents"][2]["role"], "user");
        assert_eq!(wire["contents"][0]["parts"][0]["text"], "ciao");
        // Never `assistant`, which is the word the normalized layer uses.
        assert!(!wire.to_string().contains("assistant"));
    }

    #[test]
    fn consecutive_turns_of_one_role_are_merged_so_the_roles_alternate() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("prima"))
            .with_message(Message::user("seconda"))
            .with_message(Message::assistant("ok"));
        let wire = body(&request);
        assert_eq!(wire["contents"].as_array().expect("array").len(), 2);
        assert_eq!(wire["contents"][0]["parts"][0]["text"], "prima");
        assert_eq!(wire["contents"][0]["parts"][1]["text"], "seconda");
        assert_eq!(wire["contents"][1]["role"], "model");
    }

    #[test]
    fn the_system_prompt_is_a_field_and_a_system_message_is_hoisted_into_it() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_system("Answer in Italian.")
            .with_message(Message::user("ciao"))
            .with_message(Message::system("Never invent identifiers."));
        let converted = convert(&request);
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        let instruction = wire["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .expect("an instruction");
        assert!(instruction.contains("Answer in Italian."), "{instruction}");
        assert!(
            instruction.contains("Never invent identifiers."),
            "{instruction}"
        );
        // The system instruction has no speaker.
        assert!(wire["systemInstruction"].get("role").is_none());
        // Only the user turn is left in the conversation.
        assert_eq!(wire["contents"].as_array().expect("array").len(), 1);
        assert!(
            converted
                .warnings
                .contains(&dropped("system_message_position"))
        );
    }

    #[test]
    fn tool_calls_and_results_are_parts_of_a_model_and_a_user_content() {
        let call = ToolCall::new("call_0", "case.get", json!({"target": "tok_1"}));
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::user("che stato ha?"))
            .with_message(Message::new(
                Role::Assistant,
                vec![
                    ContentPart::text("controllo"),
                    ContentPart::ToolCall(call.clone()),
                ],
            ))
            .with_message(Message::tool_result(
                turnframe_provider::request::ToolResult::ok(
                    call.id.clone(),
                    "{\"state\":\"open\"}",
                ),
            ))
            .with_tools(vec![ToolSpec::new("case.get", "load", json!({}))]);
        let wire = body(&request);

        let model_turn = &wire["contents"][1];
        assert_eq!(model_turn["role"], "model");
        assert_eq!(model_turn["parts"][0]["text"], "controllo");
        assert_eq!(model_turn["parts"][1]["functionCall"]["name"], "case.get");
        assert_eq!(
            model_turn["parts"][1]["functionCall"]["args"]["target"],
            "tok_1"
        );

        // The result is a part of a *user* content, addressed by name.
        let result_turn = &wire["contents"][2];
        assert_eq!(result_turn["role"], "user");
        let response = &result_turn["parts"][0]["functionResponse"];
        assert_eq!(response["name"], "case.get");
        assert_eq!(response["response"]["state"], "open");
        assert_eq!(response["id"], "call_0");
    }

    #[test]
    fn a_failed_tool_result_is_marked_so_the_model_cannot_read_it_as_a_value() {
        let call = ToolCall::new("call_0", "case.get", json!({}));
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::new(
                Role::Assistant,
                vec![ContentPart::ToolCall(call.clone())],
            ))
            .with_message(Message::tool_result(
                turnframe_provider::request::ToolResult::error(call.id, "not found"),
            ));
        let wire = body(&request);
        let response = &wire["contents"][1]["parts"][0]["functionResponse"]["response"];
        assert_eq!(response[TOOL_ERROR_KEY], "not found");
        assert!(response.get(TOOL_OUTPUT_KEY).is_none());
    }

    #[test]
    fn a_non_object_result_is_wrapped_rather_than_reshaped() {
        let call = ToolCall::new("c", "t", json!({}));
        let with = |content: &str| {
            let request = ModelRequest::new(ModelPurpose::Investigate)
                .with_message(Message::new(
                    Role::Assistant,
                    vec![ContentPart::ToolCall(call.clone())],
                ))
                .with_message(Message::tool_result(
                    turnframe_provider::request::ToolResult::ok("c", content),
                ));
            body(&request)["contents"][1]["parts"][0]["functionResponse"]["response"].clone()
        };
        assert_eq!(with("[1,2]")[TOOL_OUTPUT_KEY], json!([1, 2]));
        assert_eq!(with("plain text")[TOOL_OUTPUT_KEY], "plain text");
        assert_eq!(with("{\"a\":1}"), json!({"a": 1}));
    }

    #[test]
    fn a_result_for_a_call_that_is_nowhere_is_refused_rather_than_guessed() {
        let request = ModelRequest::new(ModelPurpose::Investigate).with_message(
            Message::tool_result(turnframe_provider::request::ToolResult::ok("ghost", "{}")),
        );
        let capabilities = caps();
        let quirks = Quirks::gemini();
        let error =
            build_request(&request, options(&capabilities, &quirks)).expect_err("no such call");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("tool_result_without_matching_call".to_owned())
        );

        // A caller that used the tool name as the id is understood, because on
        // this surface a call has no id of its own to preserve.
        let by_name = ModelRequest::new(ModelPurpose::Investigate)
            .with_tools(vec![ToolSpec::new("case.get", "load", json!({}))])
            .with_message(Message::tool_result(
                turnframe_provider::request::ToolResult::ok("case.get", "{}"),
            ));
        let wire = body(&by_name);
        assert_eq!(
            wire["contents"][0]["parts"][0]["functionResponse"]["name"],
            "case.get"
        );
    }

    #[test]
    fn a_schema_enforcing_profile_sends_the_translated_schema_and_the_json_mime_type() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("sposta il volo"))
            .with_output(OutputSpec::json(
                "user_turn_plan",
                json!({
                    "type": "object",
                    "properties": {"acts": {"type": "array", "items": {"type": "string"}}},
                    "required": ["acts"],
                    "additionalProperties": false
                }),
            ));
        let wire = body(&request);
        let config = &wire["generationConfig"];
        assert_eq!(config["responseMimeType"], JSON_MIME_TYPE);
        assert_eq!(config["responseSchema"]["type"], "OBJECT");
        assert_eq!(
            config["responseSchema"]["properties"]["acts"]["type"],
            "ARRAY"
        );
        // The caller's label survives as the schema title.
        assert_eq!(config["responseSchema"]["title"], "user_turn_plan");
        assert!(
            config["responseSchema"]
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn a_schema_the_dialect_cannot_express_is_refused_and_never_weakened() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("x"))
            .with_output(OutputSpec::json(
                "plan",
                json!({
                    "type": "object",
                    "properties": {
                        "act": {"oneOf": [{"type": "object"}, {"type": "object"}]}
                    }
                }),
            ));
        let capabilities = caps();
        let quirks = Quirks::gemini();
        let error = build_request(&request, options(&capabilities, &quirks))
            .expect_err("an overlapping union has no equivalent");
        let rendered = error.to_string();
        assert!(rendered.contains("response_schema:oneOf"), "{rendered}");
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fallback
        );
    }

    #[test]
    fn a_weaker_transport_describes_the_schema_and_never_sends_one() {
        let request = ModelRequest::new(ModelPurpose::Answer)
            .with_message(Message::user("x"))
            .with_output(OutputSpec::json("plan", json!({"type": "object"})));
        let capabilities = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::JsonObject);
        let quirks = Quirks::gemini();
        let converted = build_request(&request, options(&capabilities, &quirks)).expect("converts");
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(wire["generationConfig"]["responseMimeType"], JSON_MIME_TYPE);
        assert!(wire["generationConfig"].get("responseSchema").is_none());
        let instruction = wire["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .expect("a hint");
        assert!(instruction.contains("JSON Schema"), "{instruction}");

        // `PromptOnly` does not even claim the mime type.
        let prompt_only = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::PromptOnly);
        let converted = build_request(&request, options(&prompt_only, &quirks)).expect("converts");
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        assert!(wire.get("generationConfig").is_none());
    }

    #[test]
    fn a_transport_this_adapter_does_not_send_fails_closed() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("x"))
            .with_output(OutputSpec::json("plan", json!({"type": "object"})));
        let capabilities = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::GrammarConstrained);
        let quirks = Quirks::gemini();
        let error = build_request(&request, options(&capabilities, &quirks)).expect_err("not sent");
        assert!(error.to_string().contains("grammar_constrained"), "{error}");
    }

    #[test]
    fn a_schema_and_tools_together_are_refused_where_the_surface_takes_one_or_the_other() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("x"))
            .with_tools(vec![ToolSpec::new("case.get", "load", json!({}))])
            .with_output(OutputSpec::json("plan", json!({"type": "object"})));
        let capabilities = caps();
        let modern = Quirks::gemini();
        assert!(build_request(&request, options(&capabilities, &modern)).is_ok());

        let old = Quirks::gemini().with_structured_output_with_tools(false);
        let error = build_request(&request, options(&capabilities, &old)).expect_err("refused");
        assert!(
            error.to_string().contains("structured_output_with_tools"),
            "{error}"
        );
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fallback
        );
    }

    #[test]
    fn an_undeclared_capability_is_a_mismatch_before_a_byte_leaves() {
        let minimal = ProviderCapabilities::minimal();
        let quirks = Quirks::gemini();

        let with_image = ModelRequest::new(ModelPurpose::Acknowledge).with_message(
            Message::user("guarda").with_part(ContentPart::image_url("gs://bucket/a.png")),
        );
        let error = build_request(&with_image, options(&minimal, &quirks)).expect_err("no vision");
        assert!(error.to_string().contains("vision"), "{error}");

        let with_tools = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::user("x"))
            .with_tools(vec![ToolSpec::new("case.get", "load", json!({}))]);
        let error = build_request(&with_tools, options(&minimal, &quirks)).expect_err("no tools");
        assert!(error.to_string().contains("tool_calling"), "{error}");
    }

    #[test]
    fn documents_state_their_media_type_instead_of_having_it_guessed() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(
            Message::user("leggi")
                .with_part(ContentPart::document_base64("application/pdf", "JVBERi0="))
                // No suffix anywhere: the part says what it is, so nothing has
                // to be read off the path.
                .with_part(ContentPart::document_url(
                    "gs://bucket/report",
                    "application/pdf",
                )),
        );
        let converted = convert(&request);
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        let parts = &wire["contents"][0]["parts"];
        assert_eq!(parts[1]["inlineData"]["mimeType"], "application/pdf");
        assert_eq!(parts[1]["inlineData"]["data"], "JVBERi0=");
        assert_eq!(parts[2]["fileData"]["fileUri"], "gs://bucket/report");
        assert_eq!(parts[2]["fileData"]["mimeType"], "application/pdf");
        assert!(converted.warnings.is_empty());
    }

    #[test]
    fn a_document_needs_a_declaration_of_its_own() {
        // Reading a PNG says nothing about reading a PDF, and the mismatch
        // names the capability that is actually missing.
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(
            Message::user("leggi").with_part(ContentPart::document_base64("application/pdf", "AA")),
        );
        let sighted = caps().with_documents(false);
        let error =
            build_request(&request, options(&sighted, &Quirks::gemini())).expect_err("refused");
        assert!(error.to_string().contains("documents"), "{error}");
        assert!(!error.to_string().contains("vision"), "{error}");
    }

    #[test]
    fn images_become_inline_data_or_file_data() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(
            Message::user("guarda")
                .with_part(ContentPart::image_base64("image/png", "QUJD"))
                .with_part(ContentPart::image_url("gs://bucket/receipt.jpeg")),
        );
        let converted = convert(&request);
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        let parts = &wire["contents"][0]["parts"];
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], "QUJD");
        assert_eq!(parts[2]["fileData"]["fileUri"], "gs://bucket/receipt.jpeg");
        assert_eq!(parts[2]["fileData"]["mimeType"], "image/jpeg");
        assert!(converted.warnings.is_empty());

        // An extension nobody can read is reported rather than guessed.
        let opaque = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("x").with_part(ContentPart::image_url("gs://b/o")));
        let converted = convert(&opaque);
        assert!(converted.warnings.contains(&dropped("file_data_mime_type")));
    }

    #[test]
    fn tool_choice_maps_onto_the_function_calling_modes() {
        let base = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::user("x"))
            .with_tools(vec![ToolSpec::new("case.get", "load", json!({}))]);

        // `Auto` is Gemini's own default, so nothing is sent.
        assert!(body(&base).get("toolConfig").is_none());

        let required = base.clone().with_tool_choice(ToolChoice::Required);
        assert_eq!(
            body(&required)["toolConfig"]["functionCallingConfig"]["mode"],
            "ANY"
        );

        let none = base.clone().with_tool_choice(ToolChoice::None);
        assert_eq!(
            body(&none)["toolConfig"]["functionCallingConfig"]["mode"],
            "NONE"
        );

        let named = base.clone().with_tool_choice(ToolChoice::named("case.get"));
        let config = &body(&named)["toolConfig"]["functionCallingConfig"];
        assert_eq!(config["mode"], "ANY");
        assert_eq!(config["allowedFunctionNames"], json!(["case.get"]));

        // Without tools there is no tool config to send.
        let no_tools = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("x"))
            .with_tool_choice(ToolChoice::Required);
        assert!(body(&no_tools).get("toolConfig").is_none());
    }

    #[test]
    fn a_tool_with_no_arguments_omits_its_schema() {
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::user("x"))
            .with_tools(vec![
                ToolSpec::new("empty", "takes nothing", json!({})),
                ToolSpec::new(
                    "typed",
                    "takes one",
                    json!({"type": "object", "properties": {"a": {"type": "string"}}}),
                ),
            ]);
        let declarations = &body(&request)["tools"][0]["functionDeclarations"];
        assert!(declarations[0].get("parameters").is_none());
        assert_eq!(declarations[1]["parameters"]["type"], "object");
    }

    #[test]
    fn what_cannot_be_expressed_is_warned_about_rather_than_hidden() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("x"))
            .with_stop(vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "e".into(),
                "f".into(),
            ])
            .with_cache_hint(CacheHint::System)
            .with_metadata("workflow", "trip")
            .expect("a label");
        let converted = convert(&request);
        assert!(
            converted
                .warnings
                .contains(&dropped("stop_sequences_over_limit"))
        );
        assert!(converted.warnings.contains(&dropped("cache_hint")));
        // The developer API has no `labels`.
        assert!(converted.warnings.contains(&dropped("metadata")));
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(
            wire["generationConfig"]["stopSequences"]
                .as_array()
                .expect("array")
                .len(),
            5
        );
        assert!(wire.get("labels").is_none());
    }

    #[test]
    fn vertex_labels_travel_whole_or_not_at_all() {
        let capabilities = caps();
        let quirks = Quirks::vertex();
        let safe = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("x"))
            .with_metadata("workflow", "trip-2026")
            .expect("a label");
        let converted = build_request(&safe, options(&capabilities, &quirks)).expect("converts");
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(wire["labels"]["workflow"], "trip-2026");

        // One entry Vertex would reject takes the whole map with it, because a
        // rejected label fails the entire request.
        let unsafe_label = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("x"))
            .with_metadata("workflow", "Trip")
            .expect("a label");
        let converted =
            build_request(&unsafe_label, options(&capabilities, &quirks)).expect("converts");
        assert!(
            converted
                .warnings
                .contains(&dropped("metadata_labels_unsafe"))
        );
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        assert!(wire.get("labels").is_none());
    }

    #[test]
    fn safety_settings_and_a_thinking_budget_reach_the_body() {
        use crate::profile::{HarmBlockThreshold, HarmCategory};
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("x"));
        let capabilities = caps();
        let quirks = Quirks::gemini();
        let settings = [SafetySetting::new(
            HarmCategory::HarmCategoryDangerousContent,
            HarmBlockThreshold::BlockOnlyHigh,
        )];
        let converted = build_request(
            &request,
            RequestOptions {
                capabilities: &capabilities,
                quirks,
                safety_settings: &settings,
                thinking_budget: Some(1024),
            },
        )
        .expect("converts");
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(
            wire["safetySettings"][0]["category"],
            "HARM_CATEGORY_DANGEROUS_CONTENT"
        );
        assert_eq!(
            wire["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            1024
        );

        // A surface that does not take the field says so instead of sending it.
        let old = Quirks::conservative();
        let converted = build_request(
            &request,
            RequestOptions {
                capabilities: &capabilities,
                quirks: old,
                safety_settings: &[],
                thinking_budget: Some(1024),
            },
        )
        .expect("converts");
        assert!(converted.warnings.contains(&dropped("thinking_config")));
        let wire = serde_json::to_value(&converted.body).expect("serializes");
        assert!(wire.get("generationConfig").is_none());
    }

    #[test]
    fn a_conversation_with_nothing_in_it_is_refused() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge);
        let capabilities = caps();
        let quirks = Quirks::gemini();
        let error = build_request(&request, options(&capabilities, &quirks)).expect_err("empty");
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("empty_contents".to_owned())
        );
    }
}
