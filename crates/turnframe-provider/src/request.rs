//! The normalized model request (spec §20.1).
//!
//! One [`ModelRequest`] describes one model call in vocabulary no vendor owns: its
//! [`ModelPurpose`], its [`Message`]s, the [`OutputSpec`] the answer must meet, the
//! read-only [`ToolSpec`]s on the table and its limits. An adapter translates it to its wire
//! format and back; nothing above the provider layer sees the wire (spec §0 rule 8).
//!
//! It holds no credentials: keys live in the adapter's configuration as
//! [`ApiKey`](crate::secret::ApiKey) (spec §25.2). And [`RequestMetadata`] takes short
//! label-shaped values only, refusing anything else, because metadata ends up in a
//! provider's dashboard and in metric tags (spec §25.5).
//!
//! ```
//! use turnframe_provider::prelude::*;
//!
//! let request = ModelRequest::new(ModelPurpose::Acknowledge)
//!     .with_system("Answer in the user's language.")
//!     .with_message(Message::user("Quanto manca alla partenza?"))
//!     .with_max_output_tokens(400);
//!
//! assert_eq!(request.messages.len(), 1);
//! assert!(matches!(request.output, OutputSpec::FreeText));
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::{CallId, RequestId};
use crate::purpose::ModelPurpose;

/// Default per-call deadline when the caller sets none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest metadata value [`RequestMetadata`] accepts, in bytes.
pub const MAX_METADATA_VALUE_LEN: usize = 64;

/// Who a message comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Framing instructions from the application. Most adapters hoist a
    /// [`ModelRequest::system`] here instead.
    System,
    /// The end user.
    User,
    /// The model's own previous output.
    Assistant,
    /// The result of a tool the model asked for.
    Tool,
}

impl Role {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the bytes of an image come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImageSource {
    /// An HTTPS URL the provider fetches itself.
    Url {
        /// The location.
        url: String,
    },
    /// Base64-encoded bytes carried inline.
    Base64 {
        /// IANA media type, e.g. `"image/png"`.
        media_type: String,
        /// Base64 payload without a data-URI prefix.
        data: String,
    },
}

/// Where the bytes of a document come from.
///
/// A document is not an image with a different media type, which is why it has
/// its own source: every form states the media type, including the referenced
/// one. A provider needs it — Bedrock wants a format enum, Anthropic wants a
/// `document` block, OpenAI wants a data URI with the type in it — and guessing
/// it from a URL's suffix is how a PDF ends up announced as a PNG.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DocumentSource {
    /// A URL the provider fetches itself.
    Url {
        /// The location.
        url: String,
        /// IANA media type of what the URL serves, e.g. `"application/pdf"`.
        /// Stated rather than derived from the path.
        media_type: String,
    },
    /// Base64-encoded bytes carried inline.
    Base64 {
        /// IANA media type, e.g. `"application/pdf"`.
        media_type: String,
        /// Base64 payload without a data-URI prefix.
        data: String,
    },
}

impl DocumentSource {
    /// The declared media type, whichever form this is.
    #[must_use]
    pub fn media_type(&self) -> &str {
        match self {
            Self::Url { media_type, .. } | Self::Base64 { media_type, .. } => media_type,
        }
    }
}

/// One tool call the model asked for.
///
/// `arguments` is a [`serde_json::Value`] on purpose: it is model-facing data
/// awaiting schema validation, not a domain API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// Provider-assigned identifier, echoed by the matching [`ToolResult`].
    pub id: CallId,
    /// Name of the tool, as declared in a [`ToolSpec`].
    pub name: String,
    /// Arguments, still untrusted.
    pub arguments: serde_json::Value,
}

impl ToolCall {
    /// Builds a tool call.
    #[must_use]
    pub fn new(
        id: impl Into<CallId>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// The result of a tool call, sent back to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResult {
    /// The call this answers.
    pub call_id: CallId,
    /// Serialized result. Carries a source label from the read layer, so the
    /// model treats it as data, not as instructions (spec §25.3).
    pub content: String,
    /// Whether the tool failed.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    /// A successful result.
    #[must_use]
    pub fn ok(call_id: impl Into<CallId>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// A failed result.
    #[must_use]
    pub fn error(call_id: impl Into<CallId>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// One piece of a message.
///
/// A message is a sequence of parts rather than a string because a turn can
/// legitimately mix prose, an attachment and the tool exchange that preceded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentPart {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// An image. Requires
    /// [`vision`](crate::capabilities::ProviderCapabilities::vision).
    Image {
        /// Where the bytes come from.
        source: ImageSource,
    },
    /// A document — a PDF, a spreadsheet, a plain-text attachment. Requires
    /// [`documents`](crate::capabilities::ProviderCapabilities::documents),
    /// which is declared separately from `vision` because accepting a PNG says
    /// nothing about accepting a PDF.
    Document {
        /// Where the bytes come from, and what they are.
        source: DocumentSource,
    },
    /// A call the model made (in an assistant message).
    ToolCall(ToolCall),
    /// A result the runtime returned (in a tool message).
    ToolResult(ToolResult),
}

impl ContentPart {
    /// A text part.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// An image fetched by the provider from a URL.
    #[must_use]
    pub fn image_url(url: impl Into<String>) -> Self {
        Self::Image {
            source: ImageSource::Url { url: url.into() },
        }
    }

    /// An inline image.
    #[must_use]
    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            source: ImageSource::Base64 {
                media_type: media_type.into(),
                data: data.into(),
            },
        }
    }

    /// An inline part built from raw bytes, encoded here.
    ///
    /// Which of the two it becomes is decided by the media type, because the
    /// distinction the provider layer draws is exactly that one: `image/*` is an
    /// image, everything else is a document. A caller holding bytes should not
    /// have to know that accepting a PNG says nothing about accepting a PDF —
    /// the capability requirements are derived from the part, so the router does
    /// that part.
    ///
    /// Every inline form in this module takes base64 without a data-URI prefix,
    /// which is a rule a caller can only get wrong. So the encoding lives here,
    /// beside the contract that asks for it, rather than in every application
    /// that has a file.
    #[must_use]
    pub fn inline_bytes(media_type: impl Into<String>, bytes: &[u8]) -> Self {
        use base64::Engine as _;
        let media_type = media_type.into();
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        if media_type.starts_with("image/") {
            Self::Image {
                source: ImageSource::Base64 { media_type, data },
            }
        } else {
            Self::Document {
                source: DocumentSource::Base64 { media_type, data },
            }
        }
    }

    /// A document the provider fetches from a URL, with its media type stated.
    #[must_use]
    pub fn document_url(url: impl Into<String>, media_type: impl Into<String>) -> Self {
        Self::Document {
            source: DocumentSource::Url {
                url: url.into(),
                media_type: media_type.into(),
            },
        }
    }

    /// An inline document.
    #[must_use]
    pub fn document_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Document {
            source: DocumentSource::Base64 {
                media_type: media_type.into(),
                data: data.into(),
            },
        }
    }

    /// Borrows the text, when this is a text part.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }

    /// Borrows the call, when this is a tool call.
    #[must_use]
    pub fn as_tool_call(&self) -> Option<&ToolCall> {
        match self {
            Self::ToolCall(call) => Some(call),
            _ => None,
        }
    }

    /// Borrows the source, when this is a document part.
    #[must_use]
    pub fn as_document(&self) -> Option<&DocumentSource> {
        match self {
            Self::Document { source } => Some(source),
            _ => None,
        }
    }

    /// Stable snake-case label of the part kind.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Image { .. } => "image",
            Self::Document { .. } => "document",
            Self::ToolCall(_) => "tool_call",
            Self::ToolResult(_) => "tool_result",
        }
    }
}

/// One message in the conversation sent to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    /// Who is speaking.
    pub role: Role,
    /// What they said, in order.
    pub content: Vec<ContentPart>,
}

impl Message {
    /// A message with explicit parts.
    #[must_use]
    pub fn new(role: Role, content: Vec<ContentPart>) -> Self {
        Self { role, content }
    }

    /// A user message holding one text part.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![ContentPart::text(text)])
    }

    /// An assistant message holding one text part.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, vec![ContentPart::text(text)])
    }

    /// A system message holding one text part.
    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self::new(Role::System, vec![ContentPart::text(text)])
    }

    /// A tool message carrying one result.
    #[must_use]
    pub fn tool_result(result: ToolResult) -> Self {
        Self::new(Role::Tool, vec![ContentPart::ToolResult(result)])
    }

    /// Appends a part.
    #[must_use]
    pub fn with_part(mut self, part: ContentPart) -> Self {
        self.content.push(part);
        self
    }

    /// Concatenates every text part of the message.
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

    /// Returns `true` when the message carries an image part.
    #[must_use]
    pub fn has_image(&self) -> bool {
        self.content
            .iter()
            .any(|part| matches!(part, ContentPart::Image { .. }))
    }

    /// Returns `true` when the message carries a document part.
    #[must_use]
    pub fn has_document(&self) -> bool {
        self.content
            .iter()
            .any(|part| matches!(part, ContentPart::Document { .. }))
    }
}

/// The shape the answer must have (spec §20.3, §20.4).
///
/// `Json` is what a critical stage asks for. Whether the provider *enforces*
/// the schema on the wire or only sees it in a prompt is the difference between
/// [`StructuredOutputCapability::NativeJsonSchema`](crate::capabilities::StructuredOutputCapability::NativeJsonSchema)
/// and
/// [`PromptOnly`](crate::capabilities::StructuredOutputCapability::PromptOnly);
/// the router refuses to serve an understanding task with the
/// latter, and the adapter must never pretend otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutputSpec {
    /// Prose, for a request that needs no document back.
    FreeText,
    /// A single JSON document matching `schema`.
    Json {
        /// The JSON Schema the answer must satisfy. Also the schema
        /// [`parse_structured`](crate::structured::parse_structured) validates
        /// against, so it is never merely decorative.
        schema: serde_json::Value,
        /// Name the provider labels the schema with.
        name: String,
        /// Whether the provider must reject non-conforming output rather than
        /// producing a best effort. Adapters that cannot honour `strict` must
        /// declare a weaker
        /// [`StructuredOutputCapability`](crate::capabilities::StructuredOutputCapability).
        strict: bool,
    },
    /// One or more tool calls, taken as the answer.
    ToolCalls,
}

impl OutputSpec {
    /// A strict JSON-schema output.
    #[must_use]
    pub fn json(name: impl Into<String>, schema: serde_json::Value) -> Self {
        Self::Json {
            schema,
            name: name.into(),
            strict: true,
        }
    }

    /// Borrows the schema, when there is one.
    #[must_use]
    pub fn schema(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json { schema, .. } => Some(schema),
            _ => None,
        }
    }

    /// Returns `true` when the answer is expected to be JSON.
    #[must_use]
    pub const fn is_structured(&self) -> bool {
        matches!(self, Self::Json { .. })
    }

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::FreeText => "free_text",
            Self::Json { .. } => "json",
            Self::ToolCalls => "tool_calls",
        }
    }
}

/// Declaration of a tool the model may call.
///
/// Only read-only tools reach a model (spec §11.3, §21.1): an understanding
/// task never sees a write tool, and a declaration is never authorization to
/// execute anything (spec §21.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Name the model calls.
    pub name: String,
    /// What the tool does, in the model's language.
    pub description: String,
    /// JSON Schema of the arguments.
    pub parameters: serde_json::Value,
}

impl ToolSpec {
    /// Declares a tool.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// How free the model is to call tools.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolChoice {
    /// The model decides.
    #[default]
    Auto,
    /// The model must not call a tool.
    None,
    /// The model must call some tool.
    Required,
    /// The model must call this tool. Used to force a structured payload
    /// through a function schema.
    Named {
        /// Tool name.
        name: String,
    },
}

impl ToolChoice {
    /// Forces one named tool.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named { name: name.into() }
    }

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Required => "required",
            Self::Named { .. } => "named",
        }
    }
}

/// How much a reasoning model may think before it answers.
///
/// Sent only to a profile declaring
/// [`reasoning_controls`](crate::capabilities::ProviderCapabilities::reasoning_controls);
/// elsewhere it is dropped with a warning, like any parameter a profile does not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningEffort {
    /// The least the provider offers.
    Minimal,
    /// A little.
    Low,
    /// The provider's usual amount.
    Medium,
    /// As much as the provider allows.
    High,
}

impl ReasoningEffort {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// The sampling parameters a profile accepts, and the names of those it does not.
///
/// Every adapter maps the kept values to its own wire fields and reports each dropped
/// name as a warning, so a per-task setting never makes a call fail.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Sampling {
    /// The temperature to send.
    pub temperature: Option<f32>,
    /// The seed to send.
    pub seed: Option<u64>,
    /// The reasoning effort to send.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Parameters the request set and the profile does not take.
    pub dropped: Vec<&'static str>,
}

/// A hint that a prefix of the prompt is worth caching provider-side.
///
/// Only honoured when the profile declares
/// [`prompt_caching`](crate::capabilities::ProviderCapabilities::prompt_caching);
/// an adapter that cannot cache ignores it silently, because caching changes
/// cost and latency but never meaning.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CacheHint {
    /// No hint.
    #[default]
    None,
    /// Cache the system prompt.
    System,
    /// Cache the system prompt and the first `messages` messages.
    Prefix {
        /// How many leading messages belong to the stable prefix.
        messages: usize,
    },
}

/// Non-personal labels attached to a call.
///
/// Values are validated at insertion: at most [`MAX_METADATA_VALUE_LEN`] bytes,
/// no control characters and no whitespace. That is enough for a turn id, a
/// workflow key or an experiment label, and not enough for a sentence a user
/// typed (spec §25.5).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(transparent)]
pub struct RequestMetadata(BTreeMap<String, String>);

impl<'de> Deserialize<'de> for RequestMetadata {
    /// Deserialization runs the same validation as [`RequestMetadata::insert`],
    /// so a request read back from a fixture cannot carry a label the builder
    /// would have refused.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, String>::deserialize(deserializer)?;
        let mut metadata = Self::new();
        for (key, value) in raw {
            metadata
                .insert(key, value)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(metadata)
    }
}

/// A metadata value was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MetadataError {
    /// The key was empty.
    #[error("metadata key is empty")]
    EmptyKey,
    /// The value was longer than [`MAX_METADATA_VALUE_LEN`].
    #[error("metadata value for key {key} is {len} bytes, over the label limit")]
    ValueTooLong {
        /// The offending key.
        key: String,
        /// Its length.
        len: usize,
    },
    /// The value held whitespace or a control character, which is how free
    /// user text looks.
    #[error("metadata value for key {key} is not a label")]
    ValueNotALabel {
        /// The offending key.
        key: String,
    },
}

impl RequestMetadata {
    /// Empty metadata.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a label, replacing any previous value for `key`.
    ///
    /// ```
    /// use turnframe_provider::request::RequestMetadata;
    ///
    /// let mut metadata = RequestMetadata::new();
    /// metadata.insert("workflow", "trip").unwrap();
    /// // Free user text cannot be smuggled through metadata.
    /// assert!(metadata.insert("note", "the traveler said hello").is_err());
    /// ```
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), MetadataError> {
        let key = key.into();
        let value = value.into();
        if key.is_empty() {
            return Err(MetadataError::EmptyKey);
        }
        if value.len() > MAX_METADATA_VALUE_LEN {
            return Err(MetadataError::ValueTooLong {
                key,
                len: value.len(),
            });
        }
        if value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(MetadataError::ValueNotALabel { key });
        }
        self.0.insert(key, value);
        Ok(())
    }

    /// Looks a label up.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Iterates over the labels in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// How many labels are set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` when no label is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One model call (spec §20.1).
///
/// Build it with [`ModelRequest::new`] and the `with_*` methods; every field is
/// public so an adapter can read it without ceremony.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRequest {
    /// Stable id of the logical call, unchanged across retries and fallbacks
    /// (spec §20.7).
    pub request_id: RequestId,
    /// Why the call is being made. Decides capability requirements and logging
    /// policy; adapters do not branch on it.
    pub purpose: ModelPurpose,
    /// The conversation, oldest first.
    pub messages: Vec<Message>,
    /// Framing instructions, hoisted out of `messages` because most providers
    /// carry them in a dedicated field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The shape of the answer.
    pub output: OutputSpec,
    /// Read-only tools the model may call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// How free the model is to call them.
    #[serde(default)]
    pub tool_choice: ToolChoice,
    /// Cap on generated tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature. `None` leaves the provider's default in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// How much a reasoning model may think. `None` leaves the provider's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Sampling seed, for providers that honour one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Non-personal labels.
    #[serde(default, skip_serializing_if = "RequestMetadata::is_empty")]
    pub metadata: RequestMetadata,
    /// Deadline for the whole call, including reading a streamed body.
    pub timeout: Duration,
    /// Prompt-caching hint.
    #[serde(default)]
    pub cache_hint: CacheHint,
}

impl ModelRequest {
    /// A free-text request for `purpose` with a fresh
    /// [`RequestId`] and the [`DEFAULT_TIMEOUT`].
    #[must_use]
    pub fn new(purpose: ModelPurpose) -> Self {
        Self {
            request_id: RequestId::new(),
            purpose,
            messages: Vec::new(),
            system: None,
            output: OutputSpec::FreeText,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: None,
            temperature: None,
            reasoning_effort: None,
            seed: None,
            stop: Vec::new(),
            metadata: RequestMetadata::new(),
            timeout: DEFAULT_TIMEOUT,
            cache_hint: CacheHint::None,
        }
    }

    /// Pins the request id, so a retry or a fallback reuses it.
    #[must_use]
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = request_id;
        self
    }

    /// Sets the system prompt.
    #[must_use]
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Appends one message.
    #[must_use]
    pub fn with_message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Replaces the conversation.
    #[must_use]
    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    /// Sets the expected output shape.
    #[must_use]
    pub fn with_output(mut self, output: OutputSpec) -> Self {
        self.output = output;
        self
    }

    /// Declares the read-only tools.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    /// Sets the tool-choice mode.
    #[must_use]
    pub fn with_tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = tool_choice;
        self
    }

    /// Caps generated tokens.
    #[must_use]
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// Sets the sampling temperature.
    #[must_use]
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// The sampling parameters `capabilities` accepts, with the rest named as dropped.
    #[must_use]
    pub fn sampling_for(
        &self,
        capabilities: &crate::capabilities::ProviderCapabilities,
    ) -> Sampling {
        let mut dropped = Vec::new();
        let mut accept = |accepted: bool, name: &'static str| {
            if !accepted {
                dropped.push(name);
            }
            accepted
        };
        let temperature = self
            .temperature
            .filter(|_| accept(capabilities.temperature, "temperature"));
        let seed = self.seed.filter(|_| accept(capabilities.seed, "seed"));
        let reasoning_effort = self
            .reasoning_effort
            .filter(|_| accept(capabilities.reasoning_controls, "reasoning_effort"));
        Sampling {
            temperature,
            seed,
            reasoning_effort,
            dropped,
        }
    }

    /// Sets how much a reasoning model may think.
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Sets the sampling seed.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Sets the stop sequences.
    #[must_use]
    pub fn with_stop(mut self, stop: Vec<String>) -> Self {
        self.stop = stop;
        self
    }

    /// Sets the deadline.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the caching hint.
    #[must_use]
    pub fn with_cache_hint(mut self, cache_hint: CacheHint) -> Self {
        self.cache_hint = cache_hint;
        self
    }

    /// Adds a metadata label.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataError`] when the value is not label-shaped.
    pub fn with_metadata(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, MetadataError> {
        self.metadata.insert(key, value)?;
        Ok(self)
    }

    /// The capability requirements this request implies on its own, before the
    /// purpose's own requirements are folded in.
    ///
    /// It asks for tools when tools are declared, vision when any message
    /// carries an image, document input when any message carries a document,
    /// and the structured-output transports the purpose accepts when
    /// [`OutputSpec::Json`] is requested.
    #[must_use]
    pub fn requirements(&self) -> crate::capabilities::CapabilityRequirements {
        let mut requirements = self.purpose.requirements();
        if !self.tools.is_empty() || matches!(self.output, OutputSpec::ToolCalls) {
            requirements.needs_tools = true;
        }
        if self.messages.iter().any(Message::has_image) {
            requirements.needs_vision = true;
        }
        if self.messages.iter().any(Message::has_document) {
            requirements.needs_documents = true;
        }
        requirements
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sampling_keeps_what_the_profile_takes_and_names_the_rest() {
        use crate::capabilities::ProviderCapabilities;
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_temperature(0.0)
            .with_seed(7)
            .with_reasoning_effort(ReasoningEffort::Minimal);
        let reasoning_model = ProviderCapabilities::minimal().with_reasoning_controls(true);
        let sampling = request.sampling_for(&reasoning_model);
        assert_eq!(sampling.temperature, None);
        assert_eq!(sampling.seed, None);
        assert_eq!(sampling.reasoning_effort, Some(ReasoningEffort::Minimal));
        assert_eq!(sampling.dropped, vec!["temperature", "seed"]);

        let chat_model = ProviderCapabilities::minimal()
            .with_temperature(true)
            .with_seed(true);
        let sampling = request.sampling_for(&chat_model);
        assert_eq!(sampling.temperature, Some(0.0));
        assert_eq!(sampling.seed, Some(7));
        assert_eq!(sampling.dropped, vec!["reasoning_effort"]);

        let nothing_set = ModelRequest::new(ModelPurpose::Extract).sampling_for(&chat_model);
        assert!(
            nothing_set.dropped.is_empty(),
            "an unset parameter is never dropped"
        );
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn builder_defaults_are_conservative() {
        let request = ModelRequest::new(ModelPurpose::Extract);
        assert_eq!(request.timeout, DEFAULT_TIMEOUT);
        assert_eq!(request.tool_choice, ToolChoice::Auto);
        assert_eq!(request.cache_hint, CacheHint::None);
        assert!(request.temperature.is_none());
        assert!(request.metadata.is_empty());
        assert_eq!(request.output.as_str(), "free_text");
    }

    #[test]
    fn request_id_is_stable_when_pinned() {
        let id = RequestId::nil();
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_request_id(id);
        let retried = request.clone().with_temperature(0.7);
        assert_eq!(request.request_id, retried.request_id);
    }

    #[test]
    fn metadata_rejects_anything_that_is_not_a_label() {
        let mut metadata = RequestMetadata::new();
        metadata.insert("turn", "0192f0aa-1b2c-7def").unwrap();
        assert_eq!(metadata.get("turn"), Some("0192f0aa-1b2c-7def"));
        assert_eq!(metadata.len(), 1);

        assert!(matches!(
            metadata.insert("", "x"),
            Err(MetadataError::EmptyKey)
        ));
        assert!(matches!(
            metadata.insert("note", "hello world"),
            Err(MetadataError::ValueNotALabel { .. })
        ));
        assert!(matches!(
            metadata.insert("note", "x".repeat(MAX_METADATA_VALUE_LEN + 1)),
            Err(MetadataError::ValueTooLong { .. })
        ));
        assert!(matches!(
            metadata.insert("note", "line\nbreak"),
            Err(MetadataError::ValueNotALabel { .. })
        ));
        // Nothing invalid was stored.
        assert_eq!(metadata.len(), 1);
        let collected: Vec<_> = metadata.iter().collect();
        assert_eq!(collected, vec![("turn", "0192f0aa-1b2c-7def")]);
    }

    #[test]
    fn requirements_follow_the_content_of_the_request() {
        let plain = ModelRequest::new(ModelPurpose::Acknowledge);
        let requirements = plain.requirements();
        assert!(!requirements.needs_tools);
        assert!(!requirements.needs_vision);

        let with_tools = ModelRequest::new(ModelPurpose::Investigate)
            .with_tools(vec![ToolSpec::new("case.get", "load a case", json!({}))])
            .with_message(
                Message::user("look at this").with_part(ContentPart::image_url("https://x.test/a")),
            );
        let requirements = with_tools.requirements();
        assert!(requirements.needs_tools);
        assert!(requirements.needs_vision);
        assert!(!requirements.structured_output.is_empty());
    }

    #[test]
    fn messages_flatten_their_text_parts() {
        let message = Message::user("hello ").with_part(ContentPart::text("world"));
        assert_eq!(message.text(), "hello world");
        assert!(!message.has_image());
        assert_eq!(message.role, Role::User);
        assert_eq!(message.content[0].kind(), "text");
    }

    #[test]
    fn a_document_states_its_media_type_on_both_forms() {
        let inline = ContentPart::document_base64("application/pdf", "JVBERi0=");
        let referenced = ContentPart::document_url("https://x.test/a", "application/pdf");
        for part in [&inline, &referenced] {
            assert_eq!(part.kind(), "document");
            assert_eq!(
                part.as_document().map(DocumentSource::media_type),
                Some("application/pdf"),
                "a document never leaves its media type to be guessed"
            );
            assert!(part.as_text().is_none());
        }

        let message = Message::new(Role::User, vec![inline]);
        assert!(message.has_document());
        assert!(!message.has_image(), "a document is not an image");

        // And the requirement follows the content, so a profile that accepts
        // images but not PDFs is refused rather than tried.
        let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(message);
        let requirements = request.requirements();
        assert!(requirements.needs_documents);
        assert!(!requirements.needs_vision);
    }

    #[test]
    fn output_spec_exposes_its_schema() {
        let spec = OutputSpec::json("user_turn_plan", json!({"type": "object"}));
        assert!(spec.is_structured());
        assert_eq!(spec.schema(), Some(&json!({"type": "object"})));
        assert!(matches!(spec, OutputSpec::Json { strict: true, .. }));
        assert_eq!(OutputSpec::FreeText.schema(), None);
    }

    #[test]
    fn requests_round_trip_through_serde() {
        let request = ModelRequest::new(ModelPurpose::Extract)
            .with_request_id(RequestId::nil())
            .with_system("be precise")
            .with_message(Message::user("ciao"))
            .with_message(Message::tool_result(ToolResult::ok("call_1", "{}")))
            .with_output(OutputSpec::json("plan", json!({"type": "object"})))
            .with_tools(vec![ToolSpec::new("case.get", "load", json!({}))])
            .with_tool_choice(ToolChoice::named("case.get"))
            .with_cache_hint(CacheHint::Prefix { messages: 1 })
            .with_metadata("workflow", "trip")
            .unwrap();
        let json = serde_json::to_string(&request).unwrap();
        let back: ModelRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, request);
        assert!(!json.contains("\"stop\""), "empty vectors are skipped");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = json!({
            "request_id": "00000000-0000-0000-0000-000000000000",
            "purpose": "acknowledge",
            "messages": [],
            "output": {"kind": "free_text"},
            "timeout": {"secs": 1, "nanos": 0},
            "surprise": true
        });
        assert!(serde_json::from_value::<ModelRequest>(json).is_err());
    }

    #[test]
    fn tool_calls_and_results_pair_by_id() {
        let call = ToolCall::new("call_7", "case.get", json!({"id": "c1"}));
        let result = ToolResult::ok(call.id.clone(), "{\"ok\":true}");
        assert_eq!(result.call_id, call.id);
        assert!(!result.is_error);
        assert!(ToolResult::error("call_7", "boom").is_error);
        let part = ContentPart::ToolCall(call);
        assert_eq!(part.kind(), "tool_call");
        assert!(part.as_tool_call().is_some());
        assert!(part.as_text().is_none());
    }
}
