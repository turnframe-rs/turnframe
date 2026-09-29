# turnframe-provider-bedrock

The AWS Bedrock **Converse** adapter for [Turnframe](https://github.com/turnframe-rs/turnframe):
one implementation of `turnframe_provider::ModelProvider` over the `Converse` and
`ConverseStream` operations of the Bedrock Runtime API.

Part of the [Turnframe](https://github.com/turnframe-rs/turnframe) workspace. See the
workspace [README](../../README.md) and [architecture guide](../../docs/architecture.md).

## Scope

This crate translates, and does nothing else. A normalized `ModelRequest` goes in, a
Converse request goes out; a Converse response comes back, a normalized `ModelResponse`
comes out; an SDK failure comes back, a typed `ProviderError` comes out. It holds no
workflow policy, never sees a workflow view, has no opinion about which acts are safe,
and receives no command handler and no external credential.

It is built on the **AWS SDK for Rust** (`aws-sdk-bedrockruntime`) rather than on
hand-rolled HTTP. Every Bedrock request is signed with SigV4 over a canonical form of
its own headers and body, and credential resolution, session refresh, region and
endpoint resolution all hang off that signing. Reimplementing it would mean
reimplementing all of it, and holding the credential in order to do so. The SDK holds
the credential; this crate never does.

## Supplying a client

The builder takes a client, or the configuration to build one. Credentials are never
passed to this crate in any of the three forms.

```rust
use aws_sdk_bedrockruntime::config::{BehaviorVersion, Credentials, Region};
use turnframe_provider::capabilities::{StructuredOutputCapability, ToolCallingCapability};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider_bedrock::BedrockProvider;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
// In an application this is `aws_config::load_from_env().await`, handed to
// `.sdk_config(&sdk)`; spelled out here so the example needs no environment.
let aws = aws_sdk_bedrockruntime::Config::builder()
    .behavior_version(BehaviorVersion::latest())
    .region(Region::new("eu-central-1"))
    .credentials_provider(Credentials::new(
        "AKIAEXAMPLE", "not-a-real-secret", None, None, "example",
    ))
    .build();

let provider = BedrockProvider::builder()
    .service_config(aws)
    .model("anthropic.claude-sonnet-4-5-20250929-v1:0")
    .capabilities(
        BedrockProvider::converse_defaults()
            .with_structured_output(StructuredOutputCapability::NativeFunctionSchema)
            .with_tool_calling(ToolCallingCapability::Parallel)
            .with_vision(true)
            .with_documents(true)
            .with_max_context_tokens(200_000),
    )
    .build()?;

assert!(provider.capabilities().streaming);
# Ok(())
# }
```

| Setter | When to use it |
| --- | --- |
| `client(Client)` | You built the SDK client yourself and want full control of it. |
| `sdk_config(&SdkConfig)` | The usual path: one `aws_config::load_from_env().await`, one provider per model. It also fills in the routing region label. |
| `service_config(Config)` | A Bedrock-specific override: a VPC endpoint, its own retry policy, a test endpoint. |

The normalized layer owns retry policy (`turnframe_provider::fallback`), so configure the
SDK with `RetryConfig::disabled()` unless you deliberately want two layers of retry.

## The transport, and its limits

Converse has **no** response format, no JSON mode and no grammar. The only way to make
the endpoint enforce a schema is to declare one tool whose `inputSchema` *is* the
required schema and pin `toolChoice` to it. The model's `toolUse` block is then produced
against that schema, and the runtime reads the block's input as the document.

That is `StructuredOutputCapability::NativeFunctionSchema`: a native function schema used
**only as a structured-output transport, not direct execution**. The forced call is never
executed, never reaches a command handler and never authorizes anything.

Two limits follow, and the builder enforces the first two of three:

- `NativeJsonSchema`, `JsonObject` and `GrammarConstrained` are **refused at build
  time**. There is no other constructor, so no `BedrockProvider` can exist claiming a
  transport this adapter does not send.
- `NativeFunctionSchema` cannot be declared alongside `ToolCallingCapability::None`,
  because the transport *is* a tool call.
- While `toolChoice` is pinned, the model cannot call a read tool in the same turn. A
  stage that wants reads asks for `OutputSpec::ToolCalls` instead and gets the ordinary
  tool loop.

Streaming runs over `ConverseStream` and forwards every fragment as it arrives: nothing
is buffered and flushed at the end, and there is no fallback that dresses a finished
`Converse` answer up as a stream. The token counts from the trailing `metadata` event
land in the reassembled response, so the streamed and non-streamed paths report the same
usage for the same answer.

Other differences from the normalized vocabulary, all handled by the conversion:

| | Converse | Handled by |
| --- | --- | --- |
| The system prompt | a separate `system` array | hoisting `Role::System` into it |
| A tool result | a block in the **user** turn | mapping `Role::Tool` to `user`, results first |
| Consecutive same-role turns | rejected | merging them into one turn |
| Attachments | `image` and `document` blocks with an explicit format and inline bytes | the normalized part decides which block; the stated media type becomes the Converse format enum, and a non-S3 URL is dropped with a warning |
| `toolChoice: none` | does not exist | the tools are withheld, which is what the caller asked for |
| Prompt caching | explicit `cachePoint` blocks | `CacheHint` |
| Idempotency | no token | the request id travels as a `requestMetadata` correlation label |
| Usage | `inputTokens` excludes the cache counters | they are added back, so `input` means the whole prompt |

## Capabilities are per model, and Bedrock will not tell you them

**Bedrock is not a model, it is a marketplace.** The same Converse call reaches
Anthropic, Meta, Mistral, Amazon, Cohere and AI21 models, and they disagree about
images, documents, tools, parallel tool calls, prompt caching and context windows.
Bedrock exposes no capability endpoint that would answer for the model you configured,
and a declaration inferred from the brand would be a declaration about nothing.

So this adapter never guesses. `capabilities(...)` is where you record what you measured
for one model id, and the conformance suite in `tests/conformance.rs` is how you measure
it. A passing report for `anthropic.claude-sonnet-4-5-20250929-v1:0` says nothing about
`meta.llama3-70b-instruct-v1:0`.

The one exception is `BedrockProvider::converse_defaults()`, which claims the two things
the *protocol* decides rather than the model: streaming is available, and tool-call ids
round-trip. Everything model-specific starts off.

## Optional live smoke tests

`tests/live_smoke.rs` makes two real calls: one structured task through the forced tool over
`Converse`, and one streamed reply over `ConverseStream`. Mocks cannot tell you whether the
request this adapter builds is one the service still accepts; these can.

```bash
TURNFRAME_BEDROCK_LIVE_MODEL=anthropic.claude-3-5-haiku-20241022-v1:0 \
TURNFRAME_BEDROCK_LIVE_REGION=eu-central-1 \
    cargo test -p turnframe-provider-bedrock --test live_smoke -- --nocapture
```

They **never run in CI**: without the model id they print a note and pass, and they skip outright
when `CI` is set. The switch is the model rather than a credential because this crate never holds
one (the AWS SDK resolves it from the ordinary chain) and because a Bedrock model id is
account- and region-specific, so there is nothing sensible to default it to.

## Short-lived credentials are the normal case

Bedrock is signed with SigV4, and in every deployment that is not a long-lived access key
(an assumed role, an instance profile, a container task role, an SSO session), the
credential expires on a schedule. When it does, the endpoint answers `ExpiredTokenException`
on the same 403 family a genuinely wrong key produces.

This adapter maps that to `ProviderErrorKind::CredentialExpired`, never to
`Authentication`. The distinction is not pedantry: a caller holding a credential provider
can refresh and continue, and telling it the key is bad would strand it.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT) at your option.
