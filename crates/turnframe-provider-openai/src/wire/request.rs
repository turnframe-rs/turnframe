//! Normalized request → chat-completions body.
//!
//! The conversion is total and honest. Everything the endpoint can express is
//! expressed; everything it cannot is either a
//! [`FeatureDropped`](ResponseWarning::FeatureDropped) warning the caller sees
//! on the response, or — when dropping it would change the *meaning* of the
//! call — a [`CapabilityMismatch`] failure before a byte leaves the process.
//!
//! The line between the two is the point of this module:
//!
//! | Situation | What happens |
//! |---|---|
//! | A fifth stop sequence on an endpoint that takes four | dropped, warned |
//! | A cache hint on a profile that does not cache | dropped, warned |
//! | Metadata on an endpoint with no `metadata` field | dropped, warned |
//! | A document reached by URL rather than by bytes | dropped, warned |
//! | An image on a profile that declares no vision | refused |
//! | A document on a profile that declares no document input | refused |
//! | A tool on a profile that declares no tool calling | refused |
//! | [`OutputSpec::Json`] on a profile that declares no structured output | refused |
//! | A schema the declared grammar dialect cannot express | refused |
//!
//! A refusal is a [`ProviderErrorKind::CapabilityMismatch`](turnframe_provider::error::ProviderErrorKind::CapabilityMismatch),
//! whose retry class is `Fallback`: the router may offer another candidate
//! that satisfies the *same* requirements, which is not a downgrade.
//!
//! # The five structured-output transports
//!
//! Which one is used is decided by the **declared capability** and by nothing
//! else — not by what the caller asked for, and not by what the endpoint might
//! accept. That is the no-silent-downgrade rule (ADR-008) made mechanical.
//!
//! | Declaration | On the wire |
//! |---|---|
//! | `native_json_schema` | `response_format: {"type": "json_schema", …}` carrying the schema |
//! | `native_function_schema` | one declared function whose `parameters` are the schema, with `tool_choice` pinned to it |
//! | `grammar_constrained` | `guided_json` (vLLM) or `grammar` (`llama.cpp`), per the profile's [`GrammarDialect`] |
//! | `json_object` | `response_format: {"type": "json_object"}` and the schema described in the system prompt |
//! | `prompt_only` | the schema described in the system prompt, and nothing else |
//!
//! Every one of them travels on the streamed path exactly as it does on the
//! whole-response path: the same conversion builds both bodies, and a test
//! asserts the two agree field for field.
//!
//! The forced function is a **transport, never an execution** (spec §20.4):
//! the model is made to emit one document in the only slot the format leaves
//! open, and the adapter reads it back out. Nothing is dispatched, and a
//! request that also declares real tools is refused rather than silently
//! stripped of them.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use turnframe_provider::capabilities::{
    CapabilityMismatch, MissingCapability, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::request::{
    CacheHint, ContentPart, DocumentSource, ImageSource, Message, ModelRequest, OutputSpec,
    ReasoningEffort, Role, ToolChoice, ToolResult,
};
use turnframe_provider::response::ResponseWarning;

use crate::grammar;
use crate::profile::{GrammarDialect, Quirks};

/// What the forced-function transport tells the model the function is for.
///
/// The wording matters: the model is being asked to *fill in* a document, not
/// to call something that will run. A description that read like a real tool
/// would invite the model to reason about side effects it is not having.
pub(crate) const TRANSPORT_FUNCTION_DESCRIPTION: &str = concat!(
    "Return the answer as this function's arguments. It is the transport for ",
    "a single structured document and is never executed."
);

/// Prefix that marks a failed tool result in a `tool` message.
///
/// The chat-completions format has no error flag on a tool result, and a model
/// that cannot tell a failure from a value will happily narrate the failure as
/// a fact. A short, stable marker is the least-bad encoding, and it is part of
/// this adapter's contract rather than an accident.
pub(crate) const TOOL_ERROR_PREFIX: &str = "[tool_error] ";

/// The chat-completions request body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) stop: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<ResponseFormat>,
    /// vLLM's guided-decoding field: the JSON Schema itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) guided_json: Option<Value>,
    /// `llama.cpp`'s constrained-decoding field: GBNF text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) grammar: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ToolDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<ToolChoiceWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<BTreeMap<String, String>>,
}

/// `stream_options`, which asks for a usage chunk at the end of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct StreamOptions {
    pub(crate) include_usage: bool,
}

/// One message on the wire.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatMessage {
    pub(crate) role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tool_calls: Vec<ToolCallWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
}

/// A message body: a plain string, or the multimodal parts array.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum MessageContent {
    /// The common case, and the only one every compatible endpoint accepts.
    Text(String),
    /// Text and images interleaved, in the order the caller wrote them.
    Parts(Vec<ContentPartWire>),
}

/// One part of a multimodal message.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ContentPartWire {
    Text { text: String },
    ImageUrl { image_url: ImageUrlWire },
    File { file: FileWire },
}

/// The `image_url` object.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ImageUrlWire {
    pub(crate) url: String,
}

/// The `file` object a document travels in.
///
/// The endpoint takes the bytes as a data URI carrying the media type, plus a
/// filename it shows the model. The name is generated rather than taken from
/// the caller: the normalized part carries no name, and inventing one from a
/// URL would put a path in the prompt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct FileWire {
    pub(crate) filename: String,
    pub(crate) file_data: String,
}

/// A tool call echoed back into an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolCallWire {
    pub(crate) id: String,
    pub(crate) r#type: &'static str,
    pub(crate) function: ToolCallFunctionWire,
}

/// The function half of an echoed tool call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolCallFunctionWire {
    pub(crate) name: String,
    /// Arguments as a JSON *string*, which is how this format carries them.
    pub(crate) arguments: String,
}

/// A declared tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolDeclaration {
    pub(crate) r#type: &'static str,
    pub(crate) function: FunctionDeclaration,
}

/// The function half of a declared tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct FunctionDeclaration {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) strict: Option<bool>,
}

/// `tool_choice`, in both of its shapes.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ToolChoiceWire {
    Mode(&'static str),
    Named {
        r#type: &'static str,
        function: NamedTool,
    },
}

/// The named half of a forced `tool_choice`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct NamedTool {
    pub(crate) name: String,
}

/// `response_format`, in the three shapes this adapter sends.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseFormat {
    /// Schema-enforced output. Only sent when the profile declares
    /// [`StructuredOutputCapability::NativeJsonSchema`].
    JsonSchema { json_schema: JsonSchemaFormat },
    /// Syntactically valid JSON with no schema enforcement.
    JsonObject,
}

/// The `json_schema` object.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct JsonSchemaFormat {
    pub(crate) name: String,
    pub(crate) schema: Value,
    pub(crate) strict: bool,
}

/// A converted request and everything the conversion had to give up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConvertedRequest {
    pub(crate) body: ChatRequest,
    pub(crate) warnings: Vec<ResponseWarning>,
}

/// Builds the chat-completions body for one normalized request.
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

    if request.messages.iter().any(Message::has_image) && !capabilities.vision {
        missing.push(MissingCapability::Vision);
    }
    if request.messages.iter().any(Message::has_document) && !capabilities.documents {
        missing.push(MissingCapability::Documents);
    }
    if !request.tools.is_empty() && !capabilities.supports_tools() {
        missing.push(MissingCapability::ToolCalling);
    }
    // A transport failure and a capability mismatch can both be true at once.
    // The mismatch is reported first: it says the profile was never eligible,
    // which is the more fundamental fault and the one the router acts on.
    let output = match output_format(request, capabilities, quirks, &mut missing) {
        Ok(output) => output,
        Err(error) if missing.is_empty() => return Err(error),
        Err(_) => OutputConversion::none(),
    };
    if !missing.is_empty() {
        return Err(ProviderError::capability_mismatch(CapabilityMismatch {
            missing,
        }));
    }

    let system = system_text(request, output.prompt_hint);
    let messages = convert_messages(request, quirks, system, &mut warnings);
    let (max_tokens, max_completion_tokens) = if quirks.max_completion_tokens {
        (None, request.max_output_tokens)
    } else {
        (request.max_output_tokens, None)
    };
    let stop = truncate_stop(&request.stop, quirks.max_stop_sequences, &mut warnings);
    let declared: Vec<ToolDeclaration> = request
        .tools
        .iter()
        .map(|tool| ToolDeclaration {
            r#type: "function",
            function: FunctionDeclaration {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
                strict: quirks.strict_tool_schemas.then_some(true),
            },
        })
        .collect();
    let (tools, tool_choice, parallel_tool_calls) = tool_slots(
        declared,
        output.transport_function,
        request,
        capabilities,
        quirks,
    );

    if !matches!(request.cache_hint, CacheHint::None) && !capabilities.prompt_caching {
        warnings.push(dropped("cache_hint"));
    }
    let metadata = convert_metadata(request, quirks, &mut warnings);
    let sampling = request.sampling_for(capabilities);
    warnings.extend(sampling.dropped.iter().map(|name| dropped(name)));

    Ok(ConvertedRequest {
        body: ChatRequest {
            model: model.to_owned(),
            messages,
            stream: streaming,
            stream_options: (streaming && quirks.stream_usage).then_some(StreamOptions {
                include_usage: true,
            }),
            max_tokens,
            max_completion_tokens,
            temperature: sampling.temperature,
            seed: sampling.seed,
            reasoning_effort: sampling
                .reasoning_effort
                .map(|effort| effort_label(effort, model)),
            stop,
            response_format: output.response_format,
            guided_json: output.guided_json,
            grammar: output.grammar,
            tools,
            tool_choice,
            parallel_tool_calls,
            metadata,
        },
        warnings,
    })
}

/// The `reasoning_effort` value for `model`. The least effort is `minimal` on `gpt-5`,
/// `none` from `gpt-5.1` on, and `low` on the `o` families, which have neither.
fn effort_label(effort: ReasoningEffort, model: &str) -> &'static str {
    let name = model.rsplit('/').next().unwrap_or(model);
    let o_series = ["o1", "o3", "o4"]
        .iter()
        .any(|family| name.starts_with(family));
    match effort {
        ReasoningEffort::Minimal if o_series => ReasoningEffort::Low.as_str(),
        ReasoningEffort::Minimal if name.starts_with("gpt-5.") => "none",
        other => other.as_str(),
    }
}

/// Fills the three tool slots, which the forced-function transport takes over.
///
/// When the transport is in play there is exactly one function, the choice is
/// pinned to it, and parallel calls are switched **off** where the field may be
/// sent: a stage that expects one document must not be offered a way to return
/// two.
fn tool_slots(
    declared: Vec<ToolDeclaration>,
    transport: Option<FunctionDeclaration>,
    request: &ModelRequest,
    capabilities: &ProviderCapabilities,
    quirks: &Quirks,
) -> (Vec<ToolDeclaration>, Option<ToolChoiceWire>, Option<bool>) {
    if let Some(function) = transport {
        let name = function.name.clone();
        return (
            vec![ToolDeclaration {
                r#type: "function",
                function,
            }],
            Some(ToolChoiceWire::Named {
                r#type: "function",
                function: NamedTool { name },
            }),
            quirks.send_parallel_tool_calls.then_some(false),
        );
    }
    let choice = convert_tool_choice(&request.tool_choice, declared.is_empty());
    let parallel = (quirks.send_parallel_tool_calls && !declared.is_empty())
        .then_some(capabilities.parallel_tool_calls);
    (declared, choice, parallel)
}

/// What the output specification became.
struct OutputConversion {
    response_format: Option<ResponseFormat>,
    /// Text appended to the system prompt when the transport needs the schema
    /// described rather than enforced.
    prompt_hint: Option<String>,
    /// vLLM's `guided_json` payload.
    guided_json: Option<Value>,
    /// `llama.cpp`'s `grammar` text.
    grammar: Option<String>,
    /// The single function a forced-function transport declares and pins.
    transport_function: Option<FunctionDeclaration>,
}

impl OutputConversion {
    /// Nothing: the request carries no structured-output transport.
    const fn none() -> Self {
        Self {
            response_format: None,
            prompt_hint: None,
            guided_json: None,
            grammar: None,
            transport_function: None,
        }
    }
}

/// Maps [`OutputSpec`] onto a `response_format` and, where the transport is
/// weaker than schema enforcement, onto a prompt hint.
///
/// The mapping is driven by the **declared capability**, never by what the
/// caller asked for: that is the whole no-silent-downgrade rule. A profile that
/// declares `JsonObject` cannot be talked into sending a schema by a request
/// that sets `strict: true`.
fn output_format(
    request: &ModelRequest,
    capabilities: &ProviderCapabilities,
    quirks: &Quirks,
    missing: &mut Vec<MissingCapability>,
) -> Result<OutputConversion, ProviderError> {
    let OutputSpec::Json {
        schema,
        name,
        strict,
    } = &request.output
    else {
        return Ok(OutputConversion::none());
    };
    match capabilities.structured_output {
        StructuredOutputCapability::NativeJsonSchema => Ok(OutputConversion {
            response_format: Some(ResponseFormat::JsonSchema {
                json_schema: JsonSchemaFormat {
                    name: name.clone(),
                    schema: native_json_schema(schema, *strict)?,
                    strict: *strict,
                },
            }),
            ..OutputConversion::none()
        }),
        StructuredOutputCapability::NativeFunctionSchema => forced_function(
            request,
            capabilities,
            quirks,
            schema,
            name,
            *strict,
            missing,
        ),
        StructuredOutputCapability::GrammarConstrained => grammar_constrained(quirks, schema),
        StructuredOutputCapability::JsonObject => Ok(OutputConversion {
            response_format: Some(ResponseFormat::JsonObject),
            prompt_hint: Some(schema_hint(schema)),
            ..OutputConversion::none()
        }),
        StructuredOutputCapability::PromptOnly => Ok(OutputConversion {
            prompt_hint: Some(schema_hint(schema)),
            ..OutputConversion::none()
        }),
        declared @ StructuredOutputCapability::None => {
            // No transport at all. A structured request to such a profile is a
            // mismatch, never a best-effort prompt.
            missing.push(MissingCapability::StructuredOutput {
                required: vec![
                    StructuredOutputCapability::NativeJsonSchema,
                    StructuredOutputCapability::NativeFunctionSchema,
                    StructuredOutputCapability::GrammarConstrained,
                    StructuredOutputCapability::JsonObject,
                    StructuredOutputCapability::PromptOnly,
                ],
                declared,
            });
            Ok(OutputConversion::none())
        }
    }
}

/// The schema to put in a `json_schema` response format.
///
/// Strict mode constrains decoding, which is why a profile may declare `NativeJsonSchema`,
/// but it accepts only a subset of JSON Schema that `schemars` output falls outside of
/// (`oneOf` enums, a `description` beside a `$ref`, optional fields out of `required`). So
/// the schema is rewritten first; the rewrite only narrows and refuses what it cannot
/// ([`crate::strict`]), rather than dropping `strict` and declaring enforcement the wire
/// lacks. Without strict mode the schema is guidance and is sent unchanged.
fn native_json_schema(schema: &Value, strict: bool) -> Result<Value, ProviderError> {
    if !strict {
        return Ok(schema.clone());
    }
    crate::strict::to_strict_schema(schema)
        .map(|rewritten| rewritten.schema)
        .map_err(|error| ProviderError::unsupported("strict_schema").with_code(error.as_str()))
}

/// Builds the forced-function transport: one function, and no way out of it.
///
/// Two refusals guard it. Without tool calling there is no function slot to
/// force, so the profile is simply not eligible. And a request that declares
/// *real* tools cannot also be answered through this transport: the model
/// would have to choose, and dropping either side silently — the caller's
/// tools, or the schema enforcement the profile promised — is the downgrade
/// this adapter exists to make impossible.
fn forced_function(
    request: &ModelRequest,
    capabilities: &ProviderCapabilities,
    quirks: &Quirks,
    schema: &Value,
    name: &str,
    strict: bool,
    missing: &mut Vec<MissingCapability>,
) -> Result<OutputConversion, ProviderError> {
    if !capabilities.supports_tools() {
        missing.push(MissingCapability::ToolCalling);
        return Ok(OutputConversion::none());
    }
    if !request.tools.is_empty() {
        return Err(ProviderError::unsupported("function_transport_with_tools"));
    }
    // A forced function under strict tool schemas is constrained decoding just
    // as `json_schema` is, and enforces the same dialect on `parameters`.
    let enforced = quirks.strict_tool_schemas && strict;
    Ok(OutputConversion {
        transport_function: Some(FunctionDeclaration {
            name: name.to_owned(),
            description: TRANSPORT_FUNCTION_DESCRIPTION.to_owned(),
            parameters: native_json_schema(schema, enforced)?,
            strict: quirks.strict_tool_schemas.then_some(strict),
        }),
        ..OutputConversion::none()
    })
}

/// Builds the grammar-constrained transport for the profile's dialect.
///
/// The `llama.cpp` half compiles the schema into GBNF and **fails loudly** when
/// it cannot: a grammar that dropped a constraint would leave the profile
/// claiming enforcement the wire never carried.
fn grammar_constrained(quirks: &Quirks, schema: &Value) -> Result<OutputConversion, ProviderError> {
    match quirks.grammar_dialect {
        Some(GrammarDialect::GuidedJson) => Ok(OutputConversion {
            guided_json: Some(schema.clone()),
            ..OutputConversion::none()
        }),
        Some(GrammarDialect::Gbnf) => match grammar::to_gbnf(schema) {
            Ok(text) => Ok(OutputConversion {
                grammar: Some(text),
                ..OutputConversion::none()
            }),
            Err(error) => Err(ProviderError::unsupported("grammar_schema").with_code(error.code())),
        },
        // The builder refuses this combination, so reaching here means a
        // profile was assembled another way. Fail closed rather than send a
        // request with no constraint on it at all.
        None => Err(ProviderError::unsupported("grammar_dialect")),
    }
}

/// The instruction appended to the system prompt for a non-enforcing transport.
///
/// It names JSON explicitly, which is also what makes an endpoint's
/// `json_object` mode accept the request.
fn schema_hint(schema: &Value) -> String {
    format!(
        "Reply with a single JSON document and nothing else. \
         It must satisfy this JSON Schema:\n{schema}"
    )
}

/// Joins the caller's system prompt with the transport's own hint.
fn system_text(request: &ModelRequest, hint: Option<String>) -> Option<String> {
    match (request.system.as_deref(), hint) {
        (None, None) => None,
        (Some(system), None) => Some(system.to_owned()),
        (None, Some(hint)) => Some(hint),
        (Some(system), Some(hint)) => Some(format!("{system}\n\n{hint}")),
    }
}

/// Converts the conversation, expanding each tool result into its own message.
fn convert_messages(
    request: &ModelRequest,
    quirks: &Quirks,
    system: Option<String>,
    warnings: &mut Vec<ResponseWarning>,
) -> Vec<ChatMessage> {
    let mut documents = 0_usize;
    let mut out = Vec::with_capacity(request.messages.len() + 1);
    if let Some(system) = system {
        out.push(ChatMessage {
            role: quirks.system_role.as_str(),
            content: Some(MessageContent::Text(system)),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    for message in &request.messages {
        let mut parts: Vec<ContentPartWire> = Vec::new();
        let mut has_attachment = false;
        let mut calls: Vec<ToolCallWire> = Vec::new();
        let mut results: Vec<&ToolResult> = Vec::new();
        for part in &message.content {
            match part {
                ContentPart::Text { text } => {
                    parts.push(ContentPartWire::Text { text: text.clone() })
                }
                ContentPart::Image { source } => {
                    has_attachment = true;
                    parts.push(ContentPartWire::ImageUrl {
                        image_url: ImageUrlWire {
                            url: image_url(source),
                        },
                    });
                }
                ContentPart::Document { source } => match file_part(source, &mut documents) {
                    Some(part) => {
                        has_attachment = true;
                        parts.push(part);
                    }
                    None => warnings.push(dropped("document_url_source")),
                },
                ContentPart::ToolCall(call) => calls.push(ToolCallWire {
                    id: call.id.as_str().to_owned(),
                    r#type: "function",
                    function: ToolCallFunctionWire {
                        name: call.name.clone(),
                        arguments: call.arguments.to_string(),
                    },
                }),
                ContentPart::ToolResult(result) => results.push(result),
                // The part vocabulary is growable; an adapter that cannot
                // express a future part must not silently invent one.
                _ => {}
            }
        }
        let content = collapse(parts, has_attachment);
        if content.is_some() || !calls.is_empty() {
            out.push(ChatMessage {
                role: wire_role(message.role, quirks),
                content,
                tool_calls: calls,
                tool_call_id: None,
            });
        }
        for result in results {
            let body = if result.is_error {
                format!("{TOOL_ERROR_PREFIX}{}", result.content)
            } else {
                result.content.clone()
            };
            out.push(ChatMessage {
                role: "tool",
                content: Some(MessageContent::Text(body)),
                tool_calls: Vec::new(),
                tool_call_id: Some(result.call_id.as_str().to_owned()),
            });
        }
    }
    out
}

/// A parts array collapses to a plain string when no attachment is present,
/// because several compatible endpoints only accept the string shape.
fn collapse(parts: Vec<ContentPartWire>, has_attachment: bool) -> Option<MessageContent> {
    if parts.is_empty() {
        return None;
    }
    if has_attachment {
        return Some(MessageContent::Parts(parts));
    }
    let mut text = String::new();
    for part in &parts {
        if let ContentPartWire::Text { text: fragment } = part {
            text.push_str(fragment);
        }
    }
    Some(MessageContent::Text(text))
}

/// The role name a normalized role travels under.
fn wire_role(role: Role, quirks: &Quirks) -> &'static str {
    match role {
        Role::System => quirks.system_role.as_str(),
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// Inline bytes become a data URI; a URL travels as itself.
fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Url { url } => url.clone(),
        ImageSource::Base64 { media_type, data } => {
            format!("data:{media_type};base64,{data}")
        }
        // A future source this adapter cannot express becomes an empty URL
        // rather than a guess; the endpoint will reject it loudly.
        _ => String::new(),
    }
}

/// Inline document bytes become a `file` part carrying a data URI.
///
/// A URL document has nowhere to go: the endpoint fetches images from a URL and
/// files only as bytes or as an uploaded file id, and this adapter uploads
/// nothing. It is reported as dropped rather than passed off as an image.
fn file_part(source: &DocumentSource, documents: &mut usize) -> Option<ContentPartWire> {
    match source {
        DocumentSource::Base64 { media_type, data } => {
            *documents += 1;
            Some(ContentPartWire::File {
                file: FileWire {
                    filename: document_name(media_type, *documents),
                    file_data: format!("data:{media_type};base64,{data}"),
                },
            })
        }
        _ => None,
    }
}

/// A filename for an inline document: generated, and suffixed from the media
/// type where the endpoint's own list has an extension for it.
fn document_name(media_type: &str, ordinal: usize) -> String {
    let extension = match media_type.trim().to_ascii_lowercase().as_str() {
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "text/html" => "html",
        "application/json" => "json",
        _ => return format!("document-{ordinal}"),
    };
    format!("document-{ordinal}.{extension}")
}

/// Maps the tool-choice mode. `Auto` is the endpoint's own default, so it is
/// omitted; every mode is omitted when no tool is declared, because the format
/// rejects `tool_choice` without `tools`.
fn convert_tool_choice(choice: &ToolChoice, no_tools: bool) -> Option<ToolChoiceWire> {
    if no_tools {
        return None;
    }
    match choice {
        ToolChoice::Auto => None,
        ToolChoice::None => Some(ToolChoiceWire::Mode("none")),
        ToolChoice::Required => Some(ToolChoiceWire::Mode("required")),
        ToolChoice::Named { name } => Some(ToolChoiceWire::Named {
            r#type: "function",
            function: NamedTool { name: name.clone() },
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

/// Maps request metadata, or warns that it was dropped.
fn convert_metadata(
    request: &ModelRequest,
    quirks: &Quirks,
    warnings: &mut Vec<ResponseWarning>,
) -> Option<BTreeMap<String, String>> {
    if request.metadata.is_empty() {
        return None;
    }
    if !quirks.send_metadata {
        warnings.push(dropped("metadata"));
        return None;
    }
    Some(
        request
            .metadata
            .iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    )
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

    fn convert(request: &ModelRequest, capabilities: &ProviderCapabilities) -> ConvertedRequest {
        build_request(request, "m", capabilities, &Quirks::openai(), false).expect("converts")
    }

    #[test]
    fn a_schema_travels_as_a_strict_response_format() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_system("Be precise.")
            .with_message(Message::user("ciao"))
            .with_output(OutputSpec::json("plan", json!({"type": "object"})));
        let converted = convert(&request, &caps());
        let format = converted.body.response_format.expect("a response format");
        match format {
            ResponseFormat::JsonSchema { json_schema } => {
                assert_eq!(json_schema.name, "plan");
                assert!(json_schema.strict);
                assert_eq!(json_schema.schema, json!({"type": "object"}));
            }
            ResponseFormat::JsonObject => panic!("expected a schema format"),
        }
        // The system prompt travels untouched: nothing describes the schema in
        // prose when the endpoint enforces it.
        assert_eq!(converted.body.messages[0].role, "system");
        assert_eq!(
            converted.body.messages[0].content,
            Some(MessageContent::Text("Be precise.".to_owned()))
        );
    }

    #[test]
    fn a_json_object_profile_describes_the_schema_instead_of_enforcing_it() {
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_output(OutputSpec::json("plan", json!({"type": "object"})))
            .with_message(Message::user("ciao"));
        let weak = caps().with_structured_output(StructuredOutputCapability::JsonObject);
        let converted = convert(&request, &weak);
        assert!(matches!(
            converted.body.response_format,
            Some(ResponseFormat::JsonObject)
        ));
        let system = &converted.body.messages[0];
        let Some(MessageContent::Text(text)) = &system.content else {
            panic!("the hint must be a plain system message");
        };
        assert!(text.contains("JSON"), "{text}");
        assert!(text.contains("\"type\""), "{text}");
    }

    /// The schema every transport test sends, marker property included.
    fn marked_schema() -> Value {
        json!({
            "type": "object",
            "properties": {"target": {"type": "string"}},
            "required": ["target"],
            "additionalProperties": false
        })
    }

    fn structured(name: &str) -> ModelRequest {
        ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("sposta il volo"))
            .with_output(OutputSpec::json(name, marked_schema()))
    }

    #[test]
    fn a_forced_function_carries_the_schema_and_pins_the_choice_to_it() {
        let transport =
            caps().with_structured_output(StructuredOutputCapability::NativeFunctionSchema);
        let converted = convert(&structured("plan"), &transport);
        // Exactly one function, and it is the document rather than a tool.
        assert_eq!(converted.body.tools.len(), 1);
        let function = &converted.body.tools[0].function;
        assert_eq!(function.name, "plan");
        assert_eq!(function.parameters, marked_schema());
        assert_eq!(function.description, TRANSPORT_FUNCTION_DESCRIPTION);
        assert!(function.description.contains("never executed"));
        // The model has no other move: the choice names this function.
        assert_eq!(
            converted.body.tool_choice,
            Some(ToolChoiceWire::Named {
                r#type: "function",
                function: NamedTool {
                    name: "plan".to_owned()
                }
            })
        );
        // One document means one call, so parallel calls are switched off.
        assert_eq!(converted.body.parallel_tool_calls, Some(false));
        // And nothing describes the schema in prose: it is enforced, not asked for.
        assert!(converted.body.response_format.is_none());
        assert_eq!(converted.body.messages.len(), 1);
    }

    #[test]
    fn a_forced_function_transport_refuses_to_share_a_turn_with_real_tools() {
        // Serving both would mean dropping one of them silently: either the
        // caller's tools, or the schema enforcement the profile promised.
        let request = structured("plan").with_tools(vec![ToolSpec::new(
            "read_case",
            "Reads a case.",
            json!({"type": "object"}),
        )]);
        let transport =
            caps().with_structured_output(StructuredOutputCapability::NativeFunctionSchema);
        let error = build_request(&request, "m", &transport, &Quirks::openai(), false)
            .expect_err("refused");
        assert!(
            error.to_string().contains("function_transport_with_tools"),
            "{error}"
        );
    }

    #[test]
    fn a_forced_function_needs_a_function_slot_to_force() {
        let toolless = caps()
            .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
            .with_tool_calling(ToolCallingCapability::None);
        let error = build_request(
            &structured("plan"),
            "m",
            &toolless,
            &Quirks::openai(),
            false,
        )
        .expect_err("refused");
        assert!(error.to_string().contains("tool_calling"), "{error}");
    }

    #[test]
    fn a_guided_json_profile_sends_the_schema_in_the_field_vllm_reads() {
        let quirks = Quirks::conservative().with_grammar_dialect(Some(GrammarDialect::GuidedJson));
        let grammar_caps =
            caps().with_structured_output(StructuredOutputCapability::GrammarConstrained);
        let converted = build_request(&structured("plan"), "m", &grammar_caps, &quirks, false)
            .expect("converts");
        assert_eq!(converted.body.guided_json, Some(marked_schema()));
        assert!(converted.body.grammar.is_none());
        // A constrained decoder needs no prose and no weaker response format.
        assert!(converted.body.response_format.is_none());
        assert_eq!(converted.body.messages.len(), 1);
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(body["guided_json"]["required"][0], "target");
    }

    #[test]
    fn a_gbnf_profile_sends_a_grammar_compiled_from_the_schema() {
        let quirks = Quirks::conservative().with_grammar_dialect(Some(GrammarDialect::Gbnf));
        let grammar_caps =
            caps().with_structured_output(StructuredOutputCapability::GrammarConstrained);
        let converted = build_request(&structured("plan"), "m", &grammar_caps, &quirks, false)
            .expect("converts");
        let grammar = converted.body.grammar.expect("a grammar");
        assert!(grammar.starts_with("root ::= "), "{grammar}");
        assert!(grammar.contains("\\\"target\\\""), "{grammar}");
        assert!(converted.body.guided_json.is_none());
        assert!(converted.body.response_format.is_none());
    }

    #[test]
    fn a_schema_the_grammar_cannot_express_fails_loudly_rather_than_weakly() {
        let quirks = Quirks::conservative().with_grammar_dialect(Some(GrammarDialect::Gbnf));
        let grammar_caps =
            caps().with_structured_output(StructuredOutputCapability::GrammarConstrained);
        let request = ModelRequest::new(ModelPurpose::Extract).with_output(OutputSpec::json(
            "plan",
            json!({
                "type": "object",
                "properties": {"code": {"type": "string", "pattern": "^[A-Z]+$"}},
                "required": ["code"]
            }),
        ));
        let error =
            build_request(&request, "m", &grammar_caps, &quirks, false).expect_err("refused");
        assert!(error.to_string().contains("grammar_schema"), "{error}");
        // The code says which keyword and where, so the schema can be fixed.
        assert_eq!(
            error.code().map(|code| code.as_str().to_owned()),
            Some("pattern_at__code".to_owned())
        );
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fallback
        );
    }

    #[test]
    fn a_grammar_declaration_with_no_dialect_fails_closed() {
        // The builder refuses this combination; if a profile were assembled
        // another way, the conversion must not send an unconstrained request.
        let grammar_caps =
            caps().with_structured_output(StructuredOutputCapability::GrammarConstrained);
        let error = build_request(
            &structured("plan"),
            "m",
            &grammar_caps,
            &Quirks::conservative(),
            false,
        )
        .expect_err("refused");
        assert!(error.to_string().contains("grammar_dialect"), "{error}");
    }

    #[test]
    fn every_transport_survives_the_streaming_flag_unchanged() {
        // A transport that only worked with `stream: false` would be a trap:
        // the conformance suite's reconstruction row would pass and every
        // streamed call would go out unconstrained.
        let cases = [
            StructuredOutputCapability::NativeJsonSchema,
            StructuredOutputCapability::NativeFunctionSchema,
            StructuredOutputCapability::GrammarConstrained,
        ];
        let quirks = Quirks::openai().with_grammar_dialect(Some(GrammarDialect::Gbnf));
        for declared in cases {
            let with = build_request(
                &structured("plan"),
                "m",
                &caps().with_structured_output(declared),
                &quirks,
                true,
            )
            .expect("converts");
            let without = build_request(
                &structured("plan"),
                "m",
                &caps().with_structured_output(declared),
                &quirks,
                false,
            )
            .expect("converts");
            assert!(with.body.stream, "{declared}");
            assert_eq!(with.body.response_format, without.body.response_format);
            assert_eq!(with.body.guided_json, without.body.guided_json);
            assert_eq!(with.body.grammar, without.body.grammar);
            assert_eq!(with.body.tools, without.body.tools);
            assert_eq!(with.body.tool_choice, without.body.tool_choice);
        }
    }

    #[test]
    fn a_prompt_only_profile_sends_no_response_format_at_all() {
        let request = ModelRequest::new(ModelPurpose::OfflineEvaluate)
            .with_output(OutputSpec::json("plan", json!({"type": "object"})));
        let weak = caps().with_structured_output(StructuredOutputCapability::PromptOnly);
        let converted = convert(&request, &weak);
        assert!(converted.body.response_format.is_none());
        assert_eq!(converted.body.messages.len(), 1);
    }

    #[test]
    fn a_structured_request_to_a_profile_without_a_transport_is_refused() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_output(OutputSpec::json("plan", json!({"type": "object"})));
        let none = caps().with_structured_output(StructuredOutputCapability::None);
        let error =
            build_request(&request, "m", &none, &Quirks::openai(), false).expect_err("refused");
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fallback
        );
        assert!(error.to_string().contains("structured_output"));
    }

    #[test]
    fn a_document_travels_as_a_file_part_and_needs_its_own_declaration() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![
                ContentPart::text("leggi"),
                ContentPart::document_base64("application/pdf", "JVBERi0="),
            ],
        ));

        // A profile that reads images says nothing about reading PDFs, and the
        // refusal names the capability that is missing rather than vision.
        let sighted = caps().with_documents(false);
        let error =
            build_request(&request, "m", &sighted, &Quirks::openai(), false).expect_err("refused");
        assert!(error.to_string().contains("documents"), "{error}");
        assert!(!error.to_string().contains("vision"), "{error}");

        let converted = convert(&request, &caps());
        let Some(MessageContent::Parts(parts)) = &converted.body.messages[0].content else {
            panic!("a document message must use the parts array");
        };
        assert_eq!(parts.len(), 2);
        let ContentPartWire::File { file } = &parts[1] else {
            panic!("a document is a file part, never an image with a lying media type");
        };
        assert_eq!(file.file_data, "data:application/pdf;base64,JVBERi0=");
        assert_eq!(file.filename, "document-1.pdf");
        assert!(converted.warnings.is_empty());
    }

    #[test]
    fn a_document_this_endpoint_cannot_fetch_is_dropped_and_reported() {
        // The endpoint fetches images from a URL and files only as bytes. A URL
        // document is reported rather than sent as an image, which is what the
        // media type on the source is for.
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::document_url(
                "https://x.test/report.pdf",
                "application/pdf",
            )],
        ));
        let converted = convert(&request, &caps());
        assert!(converted.warnings.contains(&dropped("document_url_source")));
        assert!(
            converted.body.messages.is_empty(),
            "a message left with nothing in it is not sent, and nothing is invented"
        );
    }

    #[test]
    fn an_image_needs_a_vision_declaration() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![
                ContentPart::text("guarda"),
                ContentPart::image_base64("image/png", "AAAA"),
            ],
        ));
        let blind = caps().with_vision(false);
        let error =
            build_request(&request, "m", &blind, &Quirks::openai(), false).expect_err("refused");
        assert!(error.to_string().contains("vision"), "{error}");

        // With vision declared, the parts array survives in order and inline
        // bytes become a data URI.
        let converted = convert(&request, &caps());
        let Some(MessageContent::Parts(parts)) = &converted.body.messages[0].content else {
            panic!("an image message must use the parts array");
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[1],
            ContentPartWire::ImageUrl {
                image_url: ImageUrlWire {
                    url: "data:image/png;base64,AAAA".to_owned()
                }
            }
        );
    }

    #[test]
    fn tool_calls_and_results_round_trip_into_their_wire_shapes() {
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::user("stato?"))
            .with_message(Message::new(
                Role::Assistant,
                vec![ContentPart::ToolCall(ToolCall::new(
                    "call_1",
                    "read_case",
                    json!({"id": "c1"}),
                ))],
            ))
            .with_message(Message::tool_result(ToolResult::error("call_1", "boom")))
            .with_tools(vec![ToolSpec::new(
                "read_case",
                "Reads a case.",
                json!({"type": "object"}),
            )]);
        let converted = convert(&request, &caps());
        let assistant = &converted.body.messages[1];
        assert_eq!(assistant.role, "assistant");
        assert!(assistant.content.is_none(), "a pure tool turn has no text");
        assert_eq!(assistant.tool_calls[0].id, "call_1");
        assert_eq!(assistant.tool_calls[0].function.name, "read_case");
        assert_eq!(
            assistant.tool_calls[0].function.arguments,
            "{\"id\":\"c1\"}"
        );

        let tool = &converted.body.messages[2];
        assert_eq!(tool.role, "tool");
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_1"));
        let Some(MessageContent::Text(body)) = &tool.content else {
            panic!("a tool result is text");
        };
        assert!(body.starts_with(TOOL_ERROR_PREFIX), "{body}");

        assert_eq!(converted.body.tools.len(), 1);
        assert_eq!(converted.body.tools[0].function.strict, Some(true));
        assert_eq!(converted.body.parallel_tool_calls, Some(true));
        // `Auto` is the endpoint's default and is left unsaid.
        assert!(converted.body.tool_choice.is_none());
    }

    #[test]
    fn a_tool_on_a_tool_less_profile_is_refused() {
        let request = ModelRequest::new(ModelPurpose::Investigate).with_tools(vec![ToolSpec::new(
            "read_case",
            "Reads a case.",
            json!({"type": "object"}),
        )]);
        let plain = caps().with_tool_calling(ToolCallingCapability::None);
        let error =
            build_request(&request, "m", &plain, &Quirks::openai(), false).expect_err("refused");
        assert!(error.to_string().contains("tool_calling"), "{error}");
    }

    #[test]
    fn quirks_decide_the_optional_fields() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("ciao"))
            .with_max_output_tokens(128)
            .with_stop(vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "e".into(),
            ]);
        let modern = Quirks::openai().with_max_completion_tokens(true);
        let converted = build_request(&request, "m", &caps(), &modern, true).expect("converts");
        assert_eq!(converted.body.max_completion_tokens, Some(128));
        assert!(converted.body.max_tokens.is_none());
        assert_eq!(converted.body.stop.len(), 4);
        assert!(
            converted
                .warnings
                .contains(&dropped("stop_sequences_over_limit"))
        );
        assert_eq!(
            converted.body.stream_options,
            Some(StreamOptions {
                include_usage: true
            })
        );

        let plain = Quirks::conservative();
        let converted = build_request(&request, "m", &caps(), &plain, true).expect("converts");
        assert_eq!(converted.body.max_tokens, Some(128));
        assert!(converted.body.stream_options.is_none());
    }

    #[test]
    fn dropped_features_are_warned_about_rather_than_hidden() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("ciao"))
            .with_cache_hint(CacheHint::System)
            .with_metadata("workflow", "trip")
            .expect("label");
        let no_extras = caps();
        let converted = build_request(&request, "m", &no_extras, &Quirks::conservative(), false)
            .expect("converts");
        assert!(converted.warnings.contains(&dropped("cache_hint")));
        assert!(converted.warnings.contains(&dropped("metadata")));
        assert!(converted.body.metadata.is_none());

        let caching = no_extras.with_prompt_caching(true);
        let labelled = Quirks::openai().with_metadata(true);
        let converted = build_request(&request, "m", &caching, &labelled, false).expect("converts");
        assert!(!converted.warnings.contains(&dropped("cache_hint")));
        assert_eq!(
            converted
                .body
                .metadata
                .as_ref()
                .and_then(|m| m.get("workflow")),
            Some(&"trip".to_owned())
        );
    }

    #[test]
    fn a_reasoning_model_gets_its_effort_and_no_temperature() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("ciao"))
            .with_temperature(0.0)
            .with_seed(3)
            .with_reasoning_effort(ReasoningEffort::Minimal);
        let reasoning = crate::profile::declared_for_model(
            caps().with_temperature(true).with_seed(true),
            "gpt-5.4-mini",
        );
        let converted = build_request(
            &request,
            "gpt-5.4-mini",
            &reasoning,
            &Quirks::openai(),
            false,
        )
        .expect("converts");
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert!(body.get("temperature").is_none(), "{body}");
        assert!(body.get("seed").is_none(), "{body}");
        assert_eq!(
            body["reasoning_effort"], "none",
            "from gpt-5.1 on the least effort is none"
        );
        assert!(converted.warnings.contains(&dropped("temperature")));
        assert!(converted.warnings.contains(&dropped("seed")));

        let first = build_request(&request, "gpt-5-mini", &reasoning, &Quirks::openai(), false)
            .expect("converts");
        let body = serde_json::to_value(&first.body).expect("serializes");
        assert_eq!(body["reasoning_effort"], "minimal");

        let o_series = build_request(&request, "o4-mini", &reasoning, &Quirks::openai(), false)
            .expect("converts");
        let body = serde_json::to_value(&o_series.body).expect("serializes");
        assert_eq!(
            body["reasoning_effort"], "low",
            "the o families have no minimal effort"
        );
    }

    #[test]
    fn a_chat_model_gets_temperature_and_seed_and_no_effort() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("ciao"))
            .with_temperature(0.0)
            .with_seed(3)
            .with_reasoning_effort(ReasoningEffort::Minimal);
        let chat = crate::profile::declared_for_model(
            caps().with_temperature(true).with_seed(true),
            "gpt-4o-mini",
        );
        let converted = build_request(&request, "gpt-4o-mini", &chat, &Quirks::openai(), false)
            .expect("converts");
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["seed"], 3);
        assert!(body.get("reasoning_effort").is_none(), "{body}");
        assert!(converted.warnings.contains(&dropped("reasoning_effort")));
    }

    #[test]
    fn the_serialized_body_is_the_shape_the_endpoint_expects() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::user("ciao"))
            .with_temperature(0.0);
        let converted = convert(&request, &caps().with_temperature(true));
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(body["model"], "m");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "ciao");
        assert_eq!(body["temperature"], 0.0);
        // Absent options do not appear as nulls.
        assert!(body.get("tools").is_none());
        assert!(body.get("response_format").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn a_forced_tool_choice_serializes_as_the_named_object() {
        let choice = convert_tool_choice(&ToolChoice::named("read_case"), false);
        let value = serde_json::to_value(choice).expect("serializes");
        assert_eq!(value["type"], "function");
        assert_eq!(value["function"]["name"], "read_case");
        assert_eq!(
            serde_json::to_value(convert_tool_choice(&ToolChoice::Required, false))
                .expect("serializes"),
            serde_json::json!("required")
        );
        assert!(convert_tool_choice(&ToolChoice::Required, true).is_none());
    }
}
