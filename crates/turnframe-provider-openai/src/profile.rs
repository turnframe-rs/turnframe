//! Endpoint profiles: the small set of shapes this adapter speaks (spec §20.3, §20.5).
//!
//! One [`EndpointProfile`] answers four questions and nothing else:
//!
//! 1. **Where does the request go?** [`RouteShape`] turns a base URL and a
//!    deployment name into the chat-completions URL. OpenAI and every
//!    compatible gateway put the model in the body and the path is fixed;
//!    Azure OpenAI puts the deployment in the path and the API version in the
//!    query string.
//! 2. **How is the credential presented?** [`AuthScheme`] is
//!    `Authorization: Bearer` almost everywhere and `api-key` on Azure.
//! 3. **What does this provider-model pair actually do?**
//!    [`EndpointProfile::capabilities`] is the declaration everything
//!    downstream trusts. It is configuration, never an inference from the
//!    brand.
//! 4. **What does this endpoint get wrong?** [`Quirks`] holds the handful of
//!    field-level differences that are real: `max_completion_tokens` versus
//!    `max_tokens`, whether `stream_options` is understood, whether
//!    `parallel_tool_calls` may be sent, how many stop sequences fit.
//!
//! # Presets are a starting point, not a certificate
//!
//! [`Preset`] ships a named configuration for the gateways people actually
//! use. **A preset is a guess about a brand, and conformance is never about a
//! brand.** OpenRouter fronts hundreds of models; two of them behind the same
//! preset can differ on schema enforcement, on whether tool-call ids survive
//! and on whether usage is reported at all. The preset gets you to a first
//! request; the conformance suite (spec §20.8), run against *your* endpoint
//! and *your* model, is what lets you keep the declaration.
//!
//! ```
//! use turnframe_provider_openai::profile::{EndpointProfile, Preset};
//!
//! let profile = EndpointProfile::preset(Preset::Groq);
//! assert_eq!(profile.provider().as_str(), "groq");
//! // The preset does not claim schema enforcement on your behalf.
//! assert!(!profile.capabilities().structured_output.enforces_schema());
//! ```

use std::fmt;

use turnframe_provider::capabilities::{
    ProviderCapabilities, StructuredOutputCapability, ToolCallingCapability,
};
use turnframe_provider::ids::ProviderKey;
use turnframe_provider::secret::ApiKey;

/// Path appended to a base URL by [`RouteShape::ChatCompletions`].
pub const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

/// Public OpenAI base URL.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// The `api-version` an Azure profile uses when the caller names none.
///
/// Azure pins behaviour to a dated API version; `2024-10-21` is the first
/// stable one that carries `response_format: {"type": "json_schema"}`, which is
/// the whole reason this adapter can declare
/// [`StructuredOutputCapability::NativeJsonSchema`] against Azure.
pub const DEFAULT_AZURE_API_VERSION: &str = "2024-10-21";

/// Stop sequences accepted by OpenAI and by most gateways that copy it.
pub const DEFAULT_MAX_STOP_SEQUENCES: usize = 4;

/// How the credential is presented to the endpoint.
///
/// The value itself never leaves [`ApiKey`] except inside the header this
/// scheme builds, which is why the method that renders that value is
/// crate-private and [`AuthScheme::header_name`] is not (spec §25.2).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthScheme {
    /// `Authorization: Bearer <key>` — OpenAI and nearly every compatible
    /// gateway.
    Bearer,
    /// The raw key in a named header, e.g. Azure OpenAI's `api-key`.
    Header {
        /// Lower-case header name.
        name: String,
    },
}

impl AuthScheme {
    /// The header Azure OpenAI authenticates with.
    pub const AZURE_HEADER: &'static str = "api-key";

    /// A scheme that puts the raw key in `name`.
    #[must_use]
    pub fn header(name: impl Into<String>) -> Self {
        Self::Header {
            name: name.into().to_ascii_lowercase(),
        }
    }

    /// The header this scheme writes.
    ///
    /// ```
    /// use turnframe_provider_openai::profile::AuthScheme;
    ///
    /// assert_eq!(AuthScheme::Bearer.header_name(), "authorization");
    /// assert_eq!(AuthScheme::header("Api-Key").header_name(), "api-key");
    /// ```
    #[must_use]
    pub fn header_name(&self) -> &str {
        match self {
            Self::Bearer => "authorization",
            Self::Header { name } => name,
        }
    }

    /// Builds the header value. Crate-private: the returned `String` holds the
    /// credential and exists only long enough to become a header.
    pub(crate) fn header_value(&self, key: &ApiKey) -> String {
        match self {
            Self::Bearer => format!("Bearer {}", key.expose()),
            Self::Header { .. } => key.expose().to_owned(),
        }
    }
}

/// How the request URL is built from a base URL.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RouteShape {
    /// `{base}/chat/completions`, with the model in the body. OpenAI and every
    /// compatible endpoint.
    ChatCompletions,
    /// `{base}/openai/deployments/{deployment}/chat/completions?api-version={version}`.
    ///
    /// Azure routes by *deployment*, which is a name the subscription owner
    /// chose and only conventionally equals the model name — hence
    /// [`crate::OpenAiProviderBuilder::deployment`].
    AzureDeployment {
        /// The dated API version, e.g. [`DEFAULT_AZURE_API_VERSION`].
        api_version: String,
    },
}

impl RouteShape {
    /// An Azure route pinned to `api_version`.
    #[must_use]
    pub fn azure(api_version: impl Into<String>) -> Self {
        Self::AzureDeployment {
            api_version: api_version.into(),
        }
    }

    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::AzureDeployment { .. } => "azure_deployment",
        }
    }

    /// Returns `true` when the deployment name travels in the path, so it must
    /// be URL-safe.
    #[must_use]
    pub const fn routes_by_deployment(&self) -> bool {
        matches!(self, Self::AzureDeployment { .. })
    }

    /// Builds the chat-completions URL.
    ///
    /// `base_url` has no trailing slash (the builder normalizes it) and
    /// `deployment` is ignored by [`Self::ChatCompletions`].
    ///
    /// ```
    /// use turnframe_provider_openai::profile::RouteShape;
    ///
    /// let openai = RouteShape::ChatCompletions;
    /// assert_eq!(
    ///     openai.endpoint_url("https://api.openai.com/v1", "gpt-4o"),
    ///     "https://api.openai.com/v1/chat/completions"
    /// );
    ///
    /// let azure = RouteShape::azure("2024-10-21");
    /// assert_eq!(
    ///     azure.endpoint_url("https://contoso.openai.azure.com", "prod-4o"),
    ///     "https://contoso.openai.azure.com/openai/deployments/prod-4o/chat/completions\
    ///      ?api-version=2024-10-21"
    /// );
    /// ```
    #[must_use]
    pub fn endpoint_url(&self, base_url: &str, deployment: &str) -> String {
        match self {
            Self::ChatCompletions => format!("{base_url}{CHAT_COMPLETIONS_PATH}"),
            Self::AzureDeployment { api_version } => format!(
                "{base_url}/openai/deployments/{deployment}{CHAT_COMPLETIONS_PATH}\
                 ?api-version={api_version}"
            ),
        }
    }
}

/// Whether an OpenAI model belongs to a reasoning family: `gpt-5*`, `o1`, `o3`, `o4`.
///
/// Those take a reasoning effort and only their default temperature. A deployment name
/// that hides the family should declare its capabilities explicitly.
#[must_use]
pub fn is_reasoning_model(model: &str) -> bool {
    let name = model.rsplit('/').next().unwrap_or(model);
    name.starts_with("gpt-5")
        || ["o1", "o3", "o4"]
            .iter()
            .any(|family| name.starts_with(family))
}

/// The OpenAI endpoint's default declaration, adjusted to the model family.
pub(crate) fn declared_for_model(
    capabilities: ProviderCapabilities,
    model: &str,
) -> ProviderCapabilities {
    if is_reasoning_model(model) {
        capabilities
            .with_temperature(false)
            .with_seed(false)
            .with_reasoning_controls(true)
    } else {
        capabilities
    }
}

/// Which role name framing instructions travel under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SystemRole {
    /// `"system"`. Understood everywhere, including every compatible gateway.
    #[default]
    System,
    /// `"developer"`. Required by OpenAI's reasoning models, rejected by most
    /// gateways.
    Developer,
}

impl SystemRole {
    /// The role name that goes on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
        }
    }
}

impl fmt::Display for SystemRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an endpoint accepts a grammar-constrained request.
///
/// [`StructuredOutputCapability::GrammarConstrained`] says *that* decoding is
/// constrained; it does not say through which field, and the two self-hosted
/// runtimes disagree. A profile that declares the capability must name the
/// dialect, or [`build`](crate::OpenAiProviderBuilder::build) refuses it:
/// guessing would put a field the endpoint ignores on the wire and leave the
/// declaration claiming an enforcement that never happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum GrammarDialect {
    /// vLLM's `guided_json`: the JSON Schema itself, compiled into a grammar
    /// by the server's guided-decoding backend.
    GuidedJson,
    /// `llama.cpp`'s `grammar`: GBNF text, which this adapter compiles from the
    /// schema. A schema the translation cannot express is a loud failure, never
    /// a weaker request (see [`crate::profile`] and the crate README).
    Gbnf,
}

impl GrammarDialect {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GuidedJson => "guided_json",
            Self::Gbnf => "gbnf",
        }
    }

    /// The request field this dialect writes.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::GuidedJson => "guided_json",
            Self::Gbnf => "grammar",
        }
    }
}

impl fmt::Display for GrammarDialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The field-level differences between endpoints that all claim the same wire
/// format.
///
/// Every flag here exists because some endpoint returns a 400 without it, or
/// with it. They are deliberately few: a quirk that only changes performance
/// does not belong, and a quirk that changes *meaning* is a capability, not a
/// quirk.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Quirks {
    /// Send `max_completion_tokens` instead of `max_tokens`. OpenAI's
    /// reasoning models reject the older field; almost every gateway knows
    /// only the older field.
    pub max_completion_tokens: bool,
    /// Send `stream_options: {"include_usage": true}` so the stream ends with
    /// a usage chunk. Endpoints that do not know the field reject the request.
    pub stream_usage: bool,
    /// Send `parallel_tool_calls`. Several gateways reject the field outright.
    pub send_parallel_tool_calls: bool,
    /// Send `strict: true` inside each function declaration.
    pub strict_tool_schemas: bool,
    /// How many stop sequences the endpoint accepts. Extra ones are dropped
    /// with a [`FeatureDropped`](turnframe_provider::response::ResponseWarning::FeatureDropped)
    /// warning rather than silently truncated.
    pub max_stop_sequences: usize,
    /// The endpoint rejects `response_format: {"type": "json_object"}` unless
    /// the word JSON appears in the conversation. OpenAI does; the adapter
    /// then appends the schema to the system prompt, which mentions it.
    pub json_object_needs_prompt_hint: bool,
    /// Send the call's [`RequestId`](turnframe_provider::ids::RequestId) as an
    /// `Idempotency-Key` header (spec §20.7).
    pub idempotency_key_header: bool,
    /// Send [`RequestMetadata`](turnframe_provider::request::RequestMetadata)
    /// as the `metadata` object. OpenAI accepts it only beside `store: true`, which
    /// keeps the completion on OpenAI's side, so its preset leaves this off.
    pub send_metadata: bool,
    /// Role name framing instructions travel under.
    pub system_role: SystemRole,
    /// Which grammar field a
    /// [`GrammarConstrained`](StructuredOutputCapability::GrammarConstrained)
    /// declaration writes. `None` means the endpoint has no grammar transport,
    /// and such a declaration is refused at build time.
    pub grammar_dialect: Option<GrammarDialect>,
}

impl Quirks {
    /// The conservative set: nothing optional is sent.
    ///
    /// This is the right starting point for an endpoint nobody has measured:
    /// every flag that could provoke a 400 is off, so the first request is as
    /// plain as the format allows.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            max_completion_tokens: false,
            stream_usage: false,
            send_parallel_tool_calls: false,
            strict_tool_schemas: false,
            max_stop_sequences: DEFAULT_MAX_STOP_SEQUENCES,
            json_object_needs_prompt_hint: false,
            idempotency_key_header: false,
            send_metadata: false,
            system_role: SystemRole::System,
            grammar_dialect: None,
        }
    }

    /// What OpenAI itself accepts.
    #[must_use]
    pub const fn openai() -> Self {
        Self {
            max_completion_tokens: false,
            stream_usage: true,
            send_parallel_tool_calls: true,
            strict_tool_schemas: true,
            max_stop_sequences: DEFAULT_MAX_STOP_SEQUENCES,
            json_object_needs_prompt_hint: true,
            idempotency_key_header: true,
            send_metadata: false,
            system_role: SystemRole::System,
            // OpenAI enforces schemas natively; it has no grammar field.
            grammar_dialect: None,
        }
    }

    /// Switches `max_tokens` for `max_completion_tokens`.
    #[must_use]
    pub const fn with_max_completion_tokens(mut self, enabled: bool) -> Self {
        self.max_completion_tokens = enabled;
        self
    }

    /// Switches the streaming usage chunk.
    #[must_use]
    pub const fn with_stream_usage(mut self, enabled: bool) -> Self {
        self.stream_usage = enabled;
        self
    }

    /// Switches the `parallel_tool_calls` field.
    #[must_use]
    pub const fn with_parallel_tool_calls(mut self, enabled: bool) -> Self {
        self.send_parallel_tool_calls = enabled;
        self
    }

    /// Switches `strict` on function declarations.
    #[must_use]
    pub const fn with_strict_tool_schemas(mut self, enabled: bool) -> Self {
        self.strict_tool_schemas = enabled;
        self
    }

    /// Sets the stop-sequence limit.
    #[must_use]
    pub const fn with_max_stop_sequences(mut self, limit: usize) -> Self {
        self.max_stop_sequences = limit;
        self
    }

    /// Switches the JSON-mode prompt hint.
    #[must_use]
    pub const fn with_json_object_prompt_hint(mut self, enabled: bool) -> Self {
        self.json_object_needs_prompt_hint = enabled;
        self
    }

    /// Switches the `Idempotency-Key` header.
    #[must_use]
    pub const fn with_idempotency_key_header(mut self, enabled: bool) -> Self {
        self.idempotency_key_header = enabled;
        self
    }

    /// Switches the `metadata` object.
    #[must_use]
    pub const fn with_metadata(mut self, enabled: bool) -> Self {
        self.send_metadata = enabled;
        self
    }

    /// Sets the role framing instructions travel under.
    #[must_use]
    pub const fn with_system_role(mut self, role: SystemRole) -> Self {
        self.system_role = role;
        self
    }

    /// Names the grammar field this endpoint accepts, or removes it.
    #[must_use]
    pub const fn with_grammar_dialect(mut self, dialect: Option<GrammarDialect>) -> Self {
        self.grammar_dialect = dialect;
        self
    }
}

impl Default for Quirks {
    fn default() -> Self {
        Self::conservative()
    }
}

/// A named configuration of the generic compatible profile.
///
/// Each preset fills in a provider key, a default base URL, the quirks that
/// endpoint is known to need, and a **deliberately conservative** capability
/// declaration.
///
/// # What a preset does not do
///
/// It does not certify anything. Conformance holds for a provider **and model**
/// combination; a preset is a statement about a brand, and a brand does not run
/// inference. Run the conformance suite against the endpoint and the model you
/// intend to deploy, then declare what you measured with
/// [`crate::OpenAiProviderBuilder::capabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Preset {
    /// OpenRouter, a routing gateway in front of many vendors.
    OpenRouter,
    /// Together AI.
    Together,
    /// Fireworks AI.
    Fireworks,
    /// DeepInfra.
    DeepInfra,
    /// A self-hosted vLLM server.
    Vllm,
    /// A self-hosted `llama.cpp` server.
    LlamaCpp,
    /// Groq.
    Groq,
    /// Mistral's own OpenAI-compatible surface.
    Mistral,
    /// xAI.
    Xai,
}

impl Preset {
    /// Every preset this crate ships.
    pub const ALL: [Self; 9] = [
        Self::OpenRouter,
        Self::Together,
        Self::Fireworks,
        Self::DeepInfra,
        Self::Vllm,
        Self::LlamaCpp,
        Self::Groq,
        Self::Mistral,
        Self::Xai,
    ];

    /// Stable snake-case label, also the profile's default [`ProviderKey`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::Together => "together",
            Self::Fireworks => "fireworks",
            Self::DeepInfra => "deepinfra",
            Self::Vllm => "vllm",
            Self::LlamaCpp => "llama_cpp",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::Xai => "xai",
        }
    }

    /// The endpoint's published base URL, when it has a fixed one.
    ///
    /// Self-hosted runtimes return `None`: there is no such thing as "the"
    /// vLLM URL, and guessing one would be the kind of brand-level claim this
    /// module exists to avoid.
    #[must_use]
    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::Together => Some("https://api.together.xyz/v1"),
            Self::Fireworks => Some("https://api.fireworks.ai/inference/v1"),
            Self::DeepInfra => Some("https://api.deepinfra.com/v1/openai"),
            Self::Vllm => None,
            Self::LlamaCpp => Some("http://127.0.0.1:8080/v1"),
            Self::Groq => Some("https://api.groq.com/openai/v1"),
            Self::Mistral => Some("https://api.mistral.ai/v1"),
            Self::Xai => Some("https://api.x.ai/v1"),
        }
    }

    /// Whether the endpoint refuses a request without a credential.
    ///
    /// A local runtime started without `--api-key` accepts anonymous calls, so
    /// the builder must not insist on one.
    #[must_use]
    pub const fn requires_credential(self) -> bool {
        !matches!(self, Self::Vllm | Self::LlamaCpp)
    }

    /// The full profile.
    #[must_use]
    pub fn profile(self) -> EndpointProfile {
        EndpointProfile::compatible(self.as_str())
            .with_default_base_url(self.default_base_url())
            .with_credential_required(self.requires_credential())
            .with_capabilities(self.capabilities())
            .with_quirks(self.quirks())
            .with_max_structured_output(self.max_structured_output())
    }

    /// The strongest structured-output transport this endpoint family is known
    /// to expose over the chat-completions surface.
    ///
    /// A declaration may never exceed it (see
    /// [`EndpointProfile::max_structured_output`]).
    /// `llama.cpp` enforces a schema only through a grammar, so its ceiling is
    /// exactly [`GrammarConstrained`](StructuredOutputCapability::GrammarConstrained)
    /// — this adapter now compiles one from the schema, but it will never send
    /// a `json_schema` response format that server does not read.
    ///
    /// vLLM keeps the stronger ceiling: `guided_json` is its grammar transport
    /// and recent releases also accept `response_format: {"type":
    /// "json_schema"}`, so an adopter may declare either after measuring.
    #[must_use]
    pub const fn max_structured_output(self) -> StructuredOutputCapability {
        match self {
            Self::LlamaCpp => StructuredOutputCapability::GrammarConstrained,
            _ => StructuredOutputCapability::NativeJsonSchema,
        }
    }

    /// The conservative declaration the preset starts from.
    #[must_use]
    pub fn capabilities(self) -> ProviderCapabilities {
        let tools = if self.preserves_call_ids() {
            ToolCallingCapability::Parallel
        } else {
            ToolCallingCapability::None
        };
        ProviderCapabilities::minimal()
            // `JsonObject` is the honest default: every one of these endpoints
            // answers a `json_object` request, and none of them guarantees the
            // *schema* for every model behind it.
            .with_structured_output(StructuredOutputCapability::JsonObject)
            .with_tool_calling(tools)
            .with_streaming(true)
            .with_preserves_call_ids(self.preserves_call_ids())
            .with_temperature(true)
    }

    /// Whether the gateway is known to echo tool-call ids unchanged.
    #[must_use]
    const fn preserves_call_ids(self) -> bool {
        !matches!(self, Self::Vllm | Self::LlamaCpp)
    }

    /// The wire quirks the endpoint needs.
    #[must_use]
    pub const fn quirks(self) -> Quirks {
        let base = Quirks::conservative();
        match self {
            // Gateways that mirror OpenAI's own tool surface closely enough to
            // accept the optional fields.
            Self::OpenRouter | Self::Together | Self::Fireworks | Self::Groq | Self::Xai => {
                base.with_stream_usage(true)
            }
            Self::Mistral | Self::DeepInfra => base,
            // The two self-hosted runtimes take unlimited stop sequences and
            // each has its own grammar field. Naming the dialect here is what
            // lets an adopter declare `grammar_constrained` at all.
            Self::Vllm => base
                .with_max_stop_sequences(usize::MAX)
                .with_grammar_dialect(Some(GrammarDialect::GuidedJson)),
            Self::LlamaCpp => base
                .with_max_stop_sequences(usize::MAX)
                .with_grammar_dialect(Some(GrammarDialect::Gbnf)),
        }
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One endpoint shape: where the request goes, how it authenticates, what it
/// can do and what it gets wrong.
///
/// Three built-in shapes cover the world this adapter serves:
/// [`EndpointProfile::openai`], [`EndpointProfile::azure_openai`] and
/// [`EndpointProfile::compatible`]. [`Preset`] is configuration of the third.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointProfile {
    provider: ProviderKey,
    route: RouteShape,
    auth: AuthScheme,
    credential_required: bool,
    default_base_url: Option<String>,
    capabilities: ProviderCapabilities,
    max_structured_output: StructuredOutputCapability,
    quirks: Quirks,
}

impl EndpointProfile {
    /// OpenAI itself: `https://api.openai.com/v1`, bearer authentication,
    /// schema enforcement, parallel tool calls, vision, document input,
    /// streaming and prompt caching.
    ///
    /// The context window is deliberately left undeclared: it belongs to the
    /// model, not to the vendor, and an undeclared window fails a context
    /// requirement closed.
    #[must_use]
    pub fn openai() -> Self {
        Self {
            provider: ProviderKey::from("openai"),
            route: RouteShape::ChatCompletions,
            auth: AuthScheme::Bearer,
            credential_required: true,
            default_base_url: Some(OPENAI_BASE_URL.to_owned()),
            capabilities: ProviderCapabilities::minimal()
                .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
                .with_tool_calling(ToolCallingCapability::Parallel)
                .with_streaming(true)
                .with_vision(true)
                .with_documents(true)
                .with_prompt_caching(true)
                .with_preserves_call_ids(true)
                .with_temperature(true)
                .with_seed(true),
            max_structured_output: StructuredOutputCapability::NativeJsonSchema,
            quirks: Quirks::openai(),
        }
    }

    /// Azure OpenAI: the same model family behind a deployment-shaped path, an
    /// `api-version` query parameter and the `api-key` header.
    ///
    /// There is no default base URL — it is the resource's own hostname.
    #[must_use]
    pub fn azure_openai(api_version: impl Into<String>) -> Self {
        Self {
            provider: ProviderKey::from("azure-openai"),
            route: RouteShape::azure(api_version),
            auth: AuthScheme::header(AuthScheme::AZURE_HEADER),
            credential_required: true,
            default_base_url: None,
            ..Self::openai()
        }
    }

    /// A generic OpenAI-compatible endpoint the adopter configures.
    ///
    /// It declares [`StructuredOutputCapability::JsonObject`], no tool calling
    /// and no id preservation, because nothing is known about it yet. Raise
    /// each of those only after the conformance suite passes against the
    /// endpoint and the model you intend to use.
    #[must_use]
    pub fn compatible(provider: impl Into<ProviderKey>) -> Self {
        Self {
            provider: provider.into(),
            route: RouteShape::ChatCompletions,
            auth: AuthScheme::Bearer,
            credential_required: true,
            default_base_url: None,
            capabilities: ProviderCapabilities::minimal()
                .with_structured_output(StructuredOutputCapability::JsonObject)
                .with_streaming(true)
                .with_temperature(true),
            max_structured_output: StructuredOutputCapability::NativeJsonSchema,
            quirks: Quirks::conservative(),
        }
    }

    /// The profile of a named [`Preset`].
    #[must_use]
    pub fn preset(preset: Preset) -> Self {
        preset.profile()
    }

    /// Overrides the provider key metrics and replay records are labelled with.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<ProviderKey>) -> Self {
        self.provider = provider.into();
        self
    }

    /// Replaces the capability declaration.
    ///
    /// This is a *proposal*: [`crate::OpenAiProviderBuilder::build`] refuses it
    /// when it exceeds [`Self::max_structured_output`] or names a transport
    /// this adapter cannot put on the wire, so no provider can be constructed
    /// around a declaration the profile does not back.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Replaces the quirks.
    #[must_use]
    pub fn with_quirks(mut self, quirks: Quirks) -> Self {
        self.quirks = quirks;
        self
    }

    /// Sets the base URL used when the builder is given none.
    #[must_use]
    pub fn with_default_base_url(mut self, base_url: Option<&str>) -> Self {
        self.default_base_url = base_url.map(str::to_owned);
        self
    }

    /// Sets whether a credential is mandatory.
    #[must_use]
    pub const fn with_credential_required(mut self, required: bool) -> Self {
        self.credential_required = required;
        self
    }

    /// Sets the authentication scheme.
    #[must_use]
    pub fn with_auth(mut self, auth: AuthScheme) -> Self {
        self.auth = auth;
        self
    }

    /// Lowers the strongest transport this profile admits.
    ///
    /// Only ever lowers: a value stronger than the one the constructor chose is
    /// ignored, because a ceiling a caller can raise is not a ceiling. The
    /// ordering is the one [`StructuredOutputCapability`] declares, strongest
    /// first.
    #[must_use]
    pub fn with_max_structured_output(mut self, ceiling: StructuredOutputCapability) -> Self {
        if ceiling > self.max_structured_output {
            self.max_structured_output = ceiling;
        }
        self
    }

    /// The provider key.
    #[must_use]
    pub fn provider(&self) -> &ProviderKey {
        &self.provider
    }

    /// The URL shape.
    #[must_use]
    pub fn route(&self) -> &RouteShape {
        &self.route
    }

    /// The authentication scheme.
    #[must_use]
    pub fn auth(&self) -> &AuthScheme {
        &self.auth
    }

    /// Whether a credential is mandatory.
    #[must_use]
    pub const fn credential_required(&self) -> bool {
        self.credential_required
    }

    /// The base URL used when the builder is given none.
    #[must_use]
    pub fn default_base_url(&self) -> Option<&str> {
        self.default_base_url.as_deref()
    }

    /// The declared capabilities of this profile.
    #[must_use]
    pub fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// The strongest structured-output transport this profile admits.
    #[must_use]
    pub const fn max_structured_output(&self) -> StructuredOutputCapability {
        self.max_structured_output
    }

    /// The wire quirks.
    #[must_use]
    pub const fn quirks(&self) -> &Quirks {
        &self.quirks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_shapes_differ_where_they_are_meant_to() {
        let openai = EndpointProfile::openai();
        let azure = EndpointProfile::azure_openai(DEFAULT_AZURE_API_VERSION);
        let generic = EndpointProfile::compatible("aurora");

        assert_eq!(openai.auth().header_name(), "authorization");
        assert_eq!(azure.auth().header_name(), "api-key");
        assert!(azure.route().routes_by_deployment());
        assert!(!openai.route().routes_by_deployment());
        assert_eq!(openai.default_base_url(), Some(OPENAI_BASE_URL));
        assert_eq!(azure.default_base_url(), None);
        assert_eq!(generic.provider().as_str(), "aurora");

        // The same capability family, because it is the same model family.
        assert_eq!(openai.capabilities(), azure.capabilities());
        // And a generic endpoint claims far less.
        assert!(!generic.capabilities().supports_tools());
        assert_eq!(
            generic.capabilities().structured_output,
            StructuredOutputCapability::JsonObject
        );
    }

    #[test]
    fn azure_puts_the_deployment_in_the_path_and_the_version_in_the_query() {
        let route = RouteShape::azure("2025-01-01");
        let url = route.endpoint_url("https://contoso.openai.azure.com", "prod-4o");
        assert_eq!(
            url,
            "https://contoso.openai.azure.com/openai/deployments/prod-4o/chat/completions\
             ?api-version=2025-01-01"
        );
        assert_eq!(
            RouteShape::ChatCompletions.endpoint_url("http://localhost:8080/v1", "ignored"),
            "http://localhost:8080/v1/chat/completions"
        );
    }

    #[test]
    fn every_preset_is_internally_consistent() {
        let mut keys: Vec<&str> = Vec::new();
        for preset in Preset::ALL {
            let profile = preset.profile();
            keys.push(preset.as_str());
            assert_eq!(profile.provider().as_str(), preset.as_str());
            // A preset never declares more than its own ceiling admits.
            assert!(
                profile.capabilities().structured_output >= profile.max_structured_output(),
                "{preset} declares above its ceiling"
            );
            // And never claims schema enforcement on a brand's behalf.
            assert!(
                !profile.capabilities().structured_output.enforces_schema(),
                "{preset} claims schema enforcement out of the box"
            );
            if let Some(url) = profile.default_base_url() {
                assert!(url.starts_with("http"), "{preset} has a strange base URL");
                assert!(
                    !url.ends_with('/'),
                    "{preset} base URL has a trailing slash"
                );
            }
        }
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), Preset::ALL.len(), "preset keys must be unique");
    }

    #[test]
    fn self_hosted_presets_do_not_demand_a_credential() {
        assert!(!Preset::Vllm.requires_credential());
        assert!(!Preset::LlamaCpp.requires_credential());
        assert!(Preset::Groq.requires_credential());
        assert_eq!(Preset::Vllm.default_base_url(), None);
    }

    #[test]
    fn the_ceiling_only_ever_lowers() {
        let profile = EndpointProfile::compatible("aurora");
        assert_eq!(
            profile.max_structured_output(),
            StructuredOutputCapability::NativeJsonSchema
        );
        let lowered = profile
            .clone()
            .with_max_structured_output(StructuredOutputCapability::JsonObject);
        assert_eq!(
            lowered.max_structured_output(),
            StructuredOutputCapability::JsonObject
        );
        // Raising it back is ignored: a ceiling a caller can lift is not one.
        let attempted =
            lowered.with_max_structured_output(StructuredOutputCapability::NativeJsonSchema);
        assert_eq!(
            attempted.max_structured_output(),
            StructuredOutputCapability::JsonObject
        );
    }

    #[test]
    fn auth_values_are_shaped_per_scheme() {
        let key = ApiKey::new("sk-test-value");
        assert_eq!(
            AuthScheme::Bearer.header_value(&key),
            "Bearer sk-test-value"
        );
        assert_eq!(
            AuthScheme::header("Api-Key").header_value(&key),
            "sk-test-value"
        );
    }

    #[test]
    fn quirk_presets_are_what_they_claim() {
        let conservative = Quirks::conservative();
        assert!(!conservative.stream_usage);
        assert!(!conservative.send_parallel_tool_calls);
        assert_eq!(conservative.system_role, SystemRole::System);

        let openai = Quirks::openai();
        assert!(openai.stream_usage);
        assert!(openai.strict_tool_schemas);
        assert!(openai.idempotency_key_header);
        assert_eq!(
            openai.with_system_role(SystemRole::Developer).system_role,
            SystemRole::Developer
        );
        assert_eq!(SystemRole::Developer.to_string(), "developer");
    }
}
