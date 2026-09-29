//! Normalized request → `/api/chat` body.
//!
//! The conversion is total and honest. Everything Ollama's native chat endpoint
//! can express is expressed; everything it cannot is either a
//! [`FeatureDropped`](ResponseWarning::FeatureDropped) warning the caller sees
//! on the response, or — when dropping it would change the *meaning* of the
//! call — a failure before a byte leaves the process.
//!
//! | Situation | What happens |
//! |---|---|
//! | A tool-choice mode Ollama has no field for (`required`, a named tool) | dropped, warned |
//! | A cache hint (the daemon reports no cache accounting) | dropped, warned |
//! | Metadata (`/api/chat` has no metadata field) | dropped, warned |
//! | An image on a profile that declares no vision | refused as a capability mismatch |
//! | A document, which `/api/chat` has no channel for | refused |
//! | A tool on a profile that declares no tool calling | refused as a capability mismatch |
//! | [`OutputSpec::Json`] on a profile that declares no structured output | refused as a capability mismatch |
//! | An image given as a URL | refused as [`Unsupported`](turnframe_provider::error::ProviderErrorKind::Unsupported) |
//!
//! The last row is the one peculiar to a local runtime. Ollama does not fetch
//! anything: an image reaches a model as base64 bytes on the message and in no
//! other way. Silently dropping the picture would answer a question the user
//! never asked, so the call is refused with `Fallback` class — a provider that
//! *can* fetch the URL may serve the same request.
//!
//! # `num_ctx` is part of the capability declaration
//!
//! Ollama defaults a model's context window to a small value (4096 at the time
//! of writing) and **silently truncates** anything longer. A profile that
//! declares [`max_context_tokens`](turnframe_provider::capabilities::ProviderCapabilities::max_context_tokens)
//! without telling the daemon to allocate that window would therefore be
//! declaring something untrue. So the declared window travels as `options.num_ctx`
//! on every request, and the builder's explicit context setting overrides it.

use serde::Serialize;
use serde_json::Value;
use turnframe_provider::capabilities::{
    CapabilityMismatch, MissingCapability, ProviderCapabilities, StructuredOutputCapability,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::CallId;
use turnframe_provider::request::{
    CacheHint, ContentPart, ImageSource, Message, ModelRequest, OutputSpec, Role, ToolChoice,
    ToolResult,
};
use turnframe_provider::response::ResponseWarning;

use std::collections::HashMap;

/// Prefix that marks a failed tool result in a `tool` message.
///
/// Ollama's chat format has no error flag on a tool result, and a model that
/// cannot tell a failure from a value will happily narrate the failure as a
/// fact. A short, stable marker is the least-bad encoding, and it is part of
/// this adapter's contract rather than an accident.
pub(crate) const TOOL_ERROR_PREFIX: &str = "[tool_error] ";

/// The value `format` takes when only JSON syntax is asked for.
pub(crate) const FORMAT_JSON: &str = "json";

/// Settings the builder fixes for every call, which the request body carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WireSettings {
    /// How long the daemon keeps the model resident after the call.
    pub(crate) keep_alive: Option<String>,
    /// The context window the daemon must allocate, in tokens.
    pub(crate) num_ctx: Option<u64>,
}

/// The `/api/chat` request body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) format: Option<Format>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ToolDeclaration>,
    #[serde(skip_serializing_if = "Options::is_empty")]
    pub(crate) options: Options,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) keep_alive: Option<String>,
}

/// The `format` field, in both shapes Ollama accepts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum Format {
    /// `"json"`: valid JSON syntax, no schema.
    Mode(&'static str),
    /// A JSON Schema the daemon constrains decoding with.
    Schema(Value),
}

/// The `options` object: the sampler and runner knobs.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct Options {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) seed: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) stop: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) num_ctx: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) num_predict: Option<u32>,
}

impl Options {
    /// Returns `true` when no knob is set, so the field is omitted entirely.
    pub(crate) fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.seed.is_none()
            && self.stop.is_empty()
            && self.num_ctx.is_none()
            && self.num_predict.is_none()
    }
}

/// One message on the wire.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatMessage {
    pub(crate) role: &'static str,
    /// Always present; Ollama treats a missing `content` as a decode error.
    pub(crate) content: String,
    /// Base64 image payloads, without a data-URI prefix.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) images: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tool_calls: Vec<ToolCallWire>,
    /// Which tool a `tool` message answers. Ollama has no call ids, so the
    /// name is the whole correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_name: Option<String>,
}

/// A tool call echoed back into an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolCallWire {
    pub(crate) function: ToolCallFunctionWire,
}

/// The function half of an echoed tool call.
///
/// Arguments travel as a JSON **object**, not as the JSON-in-a-string the
/// OpenAI format uses.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ToolCallFunctionWire {
    pub(crate) name: String,
    pub(crate) arguments: Value,
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
}

/// A converted request and everything the conversion had to give up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConvertedRequest {
    pub(crate) body: ChatRequest,
    pub(crate) warnings: Vec<ResponseWarning>,
}

/// Builds the `/api/chat` body for one normalized request.
///
/// # Errors
///
/// Returns a [`ProviderError`] carrying a [`CapabilityMismatch`] when the
/// request needs something the profile does not declare, and an
/// [`Unsupported`](turnframe_provider::error::ProviderErrorKind::Unsupported)
/// failure when it needs something this endpoint cannot express at all.
pub(crate) fn build_request(
    request: &ModelRequest,
    model: &str,
    capabilities: &ProviderCapabilities,
    settings: &WireSettings,
    streaming: bool,
) -> Result<ConvertedRequest, ProviderError> {
    let mut warnings = Vec::new();
    let mut missing = Vec::new();

    if request.messages.iter().any(Message::has_image) && !capabilities.vision {
        missing.push(MissingCapability::Vision);
    }
    // `/api/chat` has one attachment channel, `images`, and no document one at
    // all. A profile that declared document input would be lying, so a document
    // part is a mismatch here rather than something reshaped into an image.
    if request.messages.iter().any(Message::has_document) && !capabilities.documents {
        missing.push(MissingCapability::Documents);
    }
    let wants_tools = !request.tools.is_empty() || matches!(request.output, OutputSpec::ToolCalls);
    if wants_tools && !capabilities.supports_tools() {
        missing.push(MissingCapability::ToolCalling);
    }
    let output = output_format(request, capabilities, &mut missing);
    if !missing.is_empty() {
        return Err(ProviderError::capability_mismatch(CapabilityMismatch {
            missing,
        }));
    }

    let system = system_text(request, output.prompt_hint);
    let messages = convert_messages(request, system)?;

    // `ToolChoice::None` is expressed exactly by declaring nothing: a model
    // that cannot see a tool cannot call one. The other two forcing modes have
    // no field on this endpoint, so they are dropped and reported.
    let suppress_tools = matches!(request.tool_choice, ToolChoice::None);
    match &request.tool_choice {
        ToolChoice::Auto | ToolChoice::None => {}
        ToolChoice::Required => warnings.push(dropped("tool_choice_required")),
        ToolChoice::Named { .. } => warnings.push(dropped("tool_choice_named")),
        _ => warnings.push(dropped("tool_choice_unknown")),
    }
    let tools: Vec<ToolDeclaration> = if suppress_tools {
        Vec::new()
    } else {
        request
            .tools
            .iter()
            .map(|tool| ToolDeclaration {
                r#type: "function",
                function: FunctionDeclaration {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                },
            })
            .collect()
    };

    if !matches!(request.cache_hint, CacheHint::None) {
        // A local runtime reuses its own KV cache and reports nothing about it,
        // so there is no hint to honour and none to bill for.
        warnings.push(dropped("cache_hint"));
    }
    if !request.metadata.is_empty() {
        warnings.push(dropped("metadata"));
    }

    let sampling = request.sampling_for(capabilities);
    warnings.extend(sampling.dropped.iter().map(|name| dropped(name)));
    let options = Options {
        temperature: sampling.temperature,
        seed: sampling.seed,
        stop: request.stop.clone(),
        num_ctx: settings.num_ctx.or(capabilities.max_context_tokens),
        num_predict: request.max_output_tokens,
    };

    Ok(ConvertedRequest {
        body: ChatRequest {
            model: model.to_owned(),
            messages,
            stream: streaming,
            format: output.format,
            tools,
            options,
            keep_alive: settings.keep_alive.clone(),
        },
        warnings,
    })
}

/// What the output specification became.
struct OutputConversion {
    format: Option<Format>,
    /// Text appended to the system prompt when the transport needs the schema
    /// described rather than enforced.
    prompt_hint: Option<String>,
}

/// Maps [`OutputSpec`] onto a `format` value and, where the transport is weaker
/// than schema enforcement, onto a prompt hint.
///
/// The mapping is driven by the **declared capability**, never by what the
/// caller asked for: that is the whole no-silent-downgrade rule. A profile that
/// declares `JsonObject` for a small model cannot be talked into sending a
/// schema by a request that sets `strict: true`.
fn output_format(
    request: &ModelRequest,
    capabilities: &ProviderCapabilities,
    missing: &mut Vec<MissingCapability>,
) -> OutputConversion {
    let OutputSpec::Json { schema, .. } = &request.output else {
        return OutputConversion {
            format: None,
            prompt_hint: None,
        };
    };
    match capabilities.structured_output {
        StructuredOutputCapability::NativeJsonSchema => OutputConversion {
            format: Some(Format::Schema(schema.clone())),
            prompt_hint: None,
        },
        StructuredOutputCapability::JsonObject => OutputConversion {
            format: Some(Format::Mode(FORMAT_JSON)),
            prompt_hint: Some(schema_hint(schema)),
        },
        StructuredOutputCapability::PromptOnly => OutputConversion {
            format: None,
            prompt_hint: Some(schema_hint(schema)),
        },
        declared => {
            // `None` has no transport at all; `NativeFunctionSchema` and
            // `GrammarConstrained` are transports this adapter does not send,
            // and the builder refuses to declare them. Reaching here means a
            // profile was assembled another way, so fail closed.
            missing.push(MissingCapability::StructuredOutput {
                required: vec![
                    StructuredOutputCapability::NativeJsonSchema,
                    StructuredOutputCapability::JsonObject,
                    StructuredOutputCapability::PromptOnly,
                ],
                declared,
            });
            OutputConversion {
                format: None,
                prompt_hint: None,
            }
        }
    }
}

/// The instruction appended to the system prompt for a non-enforcing transport.
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
///
/// Ollama correlates a tool result with the call it answers by **name**, since
/// the endpoint assigns no call ids. The names of the calls seen earlier in the
/// conversation are therefore remembered as the walk goes, and a later result
/// is labelled with the name of the call whose id it carries.
fn convert_messages(
    request: &ModelRequest,
    system: Option<String>,
) -> Result<Vec<ChatMessage>, ProviderError> {
    let mut out = Vec::with_capacity(request.messages.len() + 1);
    if let Some(system) = system {
        out.push(ChatMessage {
            role: "system",
            content: system,
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_name: None,
        });
    }
    let mut names: HashMap<CallId, String> = HashMap::new();
    for message in &request.messages {
        let mut text = String::new();
        let mut images = Vec::new();
        let mut calls: Vec<ToolCallWire> = Vec::new();
        let mut results: Vec<&ToolResult> = Vec::new();
        for part in &message.content {
            match part {
                ContentPart::Text { text: fragment } => text.push_str(fragment),
                ContentPart::Image { source } => images.push(image_data(source)?),
                // Reachable only on a profile that declared document input,
                // which no honest declaration against this endpoint does. The
                // refusal names the feature rather than sending the bytes as an
                // image the runner would try to decode as a picture.
                ContentPart::Document { .. } => {
                    return Err(ProviderError::unsupported("document_content_part"));
                }
                ContentPart::ToolCall(call) => {
                    names.insert(call.id.clone(), call.name.clone());
                    calls.push(ToolCallWire {
                        function: ToolCallFunctionWire {
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        },
                    });
                }
                ContentPart::ToolResult(result) => results.push(result),
                // The part vocabulary is growable; an adapter that cannot
                // express a future part must not silently invent one.
                _ => {}
            }
        }
        if !text.is_empty() || !images.is_empty() || !calls.is_empty() {
            out.push(ChatMessage {
                role: wire_role(message.role),
                content: text,
                images,
                tool_calls: calls,
                tool_name: None,
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
                content: body,
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_name: names.get(&result.call_id).cloned(),
            });
        }
    }
    Ok(out)
}

/// The role name a normalized role travels under.
fn wire_role(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// Inline bytes travel as bare base64; anything else this endpoint cannot take.
///
/// # Errors
///
/// Returns an [`Unsupported`](turnframe_provider::error::ProviderErrorKind::Unsupported)
/// failure for a URL source. Its class is `Fallback`, which is the truth: a
/// provider that fetches URLs may serve the same request, and dropping the
/// image would answer a different question.
fn image_data(source: &ImageSource) -> Result<String, ProviderError> {
    match source {
        ImageSource::Base64 { data, .. } => Ok(data.clone()),
        ImageSource::Url { .. } => Err(ProviderError::unsupported("image_url_source")),
        _ => Err(ProviderError::unsupported("image_source")),
    }
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

    const MODEL: &str = "qwen3:8b";

    fn capable() -> ProviderCapabilities {
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
            .with_tool_calling(ToolCallingCapability::Parallel)
            .with_vision(true)
            .with_streaming(true)
            .with_max_context_tokens(32_768)
    }

    #[test]
    fn temperature_and_seed_reach_the_options_when_declared() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("ciao"))
            .with_temperature(0.0)
            .with_seed(5);
        let declared = capable().with_temperature(true).with_seed(true);
        let converted = convert(&request, &declared).expect("converts");
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(body["options"]["temperature"], 0.0);
        assert_eq!(body["options"]["seed"], 5);

        let converted = convert(&request, &capable()).expect("converts");
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert!(body["options"].get("seed").is_none(), "{body}");
        assert!(converted.warnings.contains(&dropped("seed")));
        assert!(converted.warnings.contains(&dropped("temperature")));
    }

    fn json_object() -> ProviderCapabilities {
        ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::JsonObject)
    }

    fn convert(
        request: &ModelRequest,
        capabilities: &ProviderCapabilities,
    ) -> Result<ConvertedRequest, ProviderError> {
        build_request(
            request,
            MODEL,
            capabilities,
            &WireSettings::default(),
            false,
        )
    }

    fn body(request: &ModelRequest, capabilities: &ProviderCapabilities) -> Value {
        let converted = convert(request, capabilities).expect("converts");
        serde_json::to_value(&converted.body).expect("serializes")
    }

    #[test]
    fn a_conversation_becomes_ollama_messages_with_their_roles() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_system("Answer in the user's language.")
            .with_message(Message::user("ciao"))
            .with_message(Message::assistant("buongiorno"));
        let body = body(&request, &capable());
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["stream"], false);
        let messages = body["messages"].as_array().expect("an array");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Answer in the user's language.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "ciao");
        assert_eq!(messages[2]["role"], "assistant");
    }

    #[test]
    fn an_image_travels_as_bare_base64_on_the_message() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![
                ContentPart::text("cosa vedi?"),
                ContentPart::image_base64("image/png", "aGVsbG8="),
            ],
        ));
        let body = body(&request, &capable());
        assert_eq!(body["messages"][0]["content"], "cosa vedi?");
        // Bare base64: no data-URI prefix, because the daemon decodes the
        // bytes itself and rejects anything else.
        assert_eq!(body["messages"][0]["images"][0], "aGVsbG8=");
    }

    #[test]
    fn an_image_given_as_a_url_is_refused_rather_than_dropped() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::image_url("https://example.test/x.png")],
        ));
        let error = convert(&request, &capable()).expect_err("the daemon fetches nothing");
        assert!(
            error.to_string().contains("unsupported(image_url_source)"),
            "{error}"
        );
        // Fallback, not fatal: a provider that fetches URLs may serve this.
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fallback
        );
    }

    #[test]
    fn a_document_is_refused_because_this_endpoint_has_no_channel_for_one() {
        // `/api/chat` carries `images` and nothing else. An honest declaration
        // therefore never sets `documents`, and the mismatch says so before a
        // byte leaves rather than sending a PDF into the image array.
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::document_base64("application/pdf", "JVBERi0=")],
        ));
        let error = convert(&request, &json_object()).expect_err("no document channel");
        assert!(error.to_string().contains("documents"), "{error}");
        assert_eq!(
            error.retry_class(),
            turnframe_provider::error::RetryClass::Fallback,
            "another provider may take the same document"
        );
    }

    #[test]
    fn a_profile_without_vision_refuses_an_image_as_a_capability_mismatch() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::new(
            Role::User,
            vec![ContentPart::image_base64("image/png", "aGVsbG8=")],
        ));
        let error = convert(&request, &json_object()).expect_err("no vision declared");
        assert!(error.to_string().contains("vision"), "{error}");
    }

    #[test]
    fn tools_and_tool_calls_use_ollamas_own_shape() {
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_message(Message::user("che stato ha la pratica?"))
            .with_message(Message::new(
                Role::Assistant,
                vec![ContentPart::ToolCall(ToolCall::new(
                    "call_0",
                    "load_case",
                    json!({"target": "tok_1"}),
                ))],
            ))
            .with_message(Message::tool_result(ToolResult::ok("call_0", "{\"n\":1}")))
            .with_tools(vec![ToolSpec::new(
                "load_case",
                "Loads a case. Read-only.",
                json!({"type": "object"}),
            )]);
        let body = body(&request, &capable());
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "load_case");
        // Arguments are an object here, not the JSON-in-a-string the OpenAI
        // format uses.
        let echoed = &body["messages"][1]["tool_calls"][0]["function"];
        assert_eq!(echoed["name"], "load_case");
        assert_eq!(echoed["arguments"], json!({"target": "tok_1"}));
        // The result is correlated by name, because there is no id to use.
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_name"], "load_case");
        assert_eq!(body["messages"][2]["content"], "{\"n\":1}");
    }

    #[test]
    fn a_failed_tool_result_is_marked_so_the_model_cannot_narrate_it_as_a_fact() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_message(Message::tool_result(ToolResult::error("call_0", "denied")));
        let body = body(&request, &capable());
        assert_eq!(
            body["messages"][0]["content"],
            format!("{TOOL_ERROR_PREFIX}denied")
        );
    }

    #[test]
    fn a_schema_transport_puts_the_schema_in_format() {
        let schema = json!({"type": "object", "properties": {"acts": {"type": "array"}}});
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("sposta il volo"))
            .with_output(OutputSpec::json("plan", schema.clone()));
        let body = body(&request, &capable());
        assert_eq!(body["format"], schema);
        // Nothing was smuggled into the prompt: the schema is enforced, not
        // described.
        assert!(
            body["messages"].as_array().expect("an array").is_empty() || {
                let first = body["messages"][0]["content"].as_str().unwrap_or_default();
                !first.contains("JSON Schema")
            }
        );
    }

    #[test]
    fn a_json_object_transport_asks_for_json_and_describes_the_schema() {
        let schema = json!({"type": "object", "properties": {"acts": {"type": "array"}}});
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_message(Message::user("sposta il volo"))
            .with_output(OutputSpec::json("plan", schema));
        let body = body(&request, &json_object());
        assert_eq!(body["format"], FORMAT_JSON);
        let system = body["messages"][0]["content"].as_str().expect("a system");
        assert!(system.contains("JSON Schema"), "{system}");
        assert!(system.contains("acts"), "{system}");
    }

    #[test]
    fn a_prompt_only_transport_sends_no_format_at_all() {
        let capabilities = ProviderCapabilities::minimal()
            .with_structured_output(StructuredOutputCapability::PromptOnly);
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_output(OutputSpec::json("plan", json!({"type": "object"})));
        let body = body(&request, &capabilities);
        assert!(body.get("format").is_none());
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .expect("a system")
                .contains("JSON Schema")
        );
    }

    #[test]
    fn a_profile_with_no_structured_output_refuses_a_json_request() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_output(OutputSpec::json("plan", json!({"type": "object"})));
        let error = convert(&request, &ProviderCapabilities::minimal())
            .expect_err("nothing to send it with");
        assert!(error.to_string().contains("structured_output"), "{error}");
    }

    #[test]
    fn the_options_object_carries_temperature_stop_and_the_context_window() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_temperature(0.2)
            .with_stop(vec!["FINE".to_owned()])
            .with_max_output_tokens(256);
        let body = body(&request, &capable().with_temperature(true));
        let temperature = body["options"]["temperature"].as_f64().expect("a number");
        assert!((temperature - 0.2).abs() < 1e-6, "{temperature}");
        assert_eq!(body["options"]["stop"][0], "FINE");
        assert_eq!(body["options"]["num_predict"], 256);
        // The declared window travels, or the daemon would truncate to its own
        // small default and the declaration would be about nothing.
        assert_eq!(body["options"]["num_ctx"], 32_768);
    }

    #[test]
    fn an_explicit_context_setting_overrides_the_declared_window() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge);
        let converted = build_request(
            &request,
            MODEL,
            &capable(),
            &WireSettings {
                keep_alive: Some("5m".to_owned()),
                num_ctx: Some(8_192),
            },
            true,
        )
        .expect("converts");
        let body = serde_json::to_value(&converted.body).expect("serializes");
        assert_eq!(body["options"]["num_ctx"], 8_192);
        assert_eq!(body["keep_alive"], "5m");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn an_empty_options_object_is_omitted_entirely() {
        let request =
            ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("ciao"));
        let body = body(&request, &ProviderCapabilities::minimal());
        assert!(body.get("options").is_none(), "{body}");
    }

    #[test]
    fn a_tool_choice_this_endpoint_cannot_express_is_dropped_and_reported() {
        let tools = vec![ToolSpec::new("t", "d", json!({"type": "object"}))];
        let request = ModelRequest::new(ModelPurpose::Investigate)
            .with_tools(tools.clone())
            .with_tool_choice(turnframe_provider::request::ToolChoice::Required);
        let converted = convert(&request, &capable()).expect("converts");
        assert!(
            converted
                .warnings
                .contains(&dropped("tool_choice_required"))
        );
        // The tools still travel: the mode was dropped, not the declaration.
        assert_eq!(converted.body.tools.len(), 1);

        // `None` is expressed exactly, by declaring nothing at all.
        let forbidden = ModelRequest::new(ModelPurpose::Investigate)
            .with_tools(tools)
            .with_tool_choice(turnframe_provider::request::ToolChoice::None);
        let converted = convert(&forbidden, &capable()).expect("converts");
        assert!(converted.body.tools.is_empty());
        assert!(converted.warnings.is_empty());
    }

    #[test]
    fn a_cache_hint_and_metadata_are_dropped_and_reported() {
        let request = ModelRequest::new(ModelPurpose::Acknowledge)
            .with_cache_hint(CacheHint::System)
            .with_metadata("workflow", "trip")
            .expect("a label");
        let converted = convert(&request, &capable()).expect("converts");
        assert!(converted.warnings.contains(&dropped("cache_hint")));
        assert!(converted.warnings.contains(&dropped("metadata")));
    }
}
