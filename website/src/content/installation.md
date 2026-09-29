# Installation

An application depends on one crate, `turnframe`, and turns on the parts it uses with feature flags.
The rest of the family comes in behind them.

## Add the dependency

```sh
cargo add turnframe --features openai
```

or, in `Cargo.toml`:

```toml
[dependencies]
turnframe = { version = "0.1", features = ["openai", "postgres", "telemetry"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

To follow the repository between releases, depend on it instead:

```sh
cargo add turnframe --git {{repo}} --features openai
```

## Requirements

- Rust {{msrv}} or newer. The workspace uses the 2024 edition.
- Tokio. The runtime is asynchronous and runs on it; the core crate has no async runtime of its own.
- A model provider for real turns: OpenAI, Azure OpenAI or a compatible endpoint, Anthropic, Gemini
  or Vertex AI, AWS Bedrock, or a local Ollama daemon. The examples and the test kit need none.
- PostgreSQL only if you choose the reference store. The in-memory store needs nothing, and any
  database can back the store traits.

## Feature flags

None is on by default, and none changes the runtime's safety semantics.

{{features}}

## The crate family

Each crate can be used on its own, but the facade is the supported way in: its modules are named for
what an adopter writes (`flow`, `turn`, `interaction`, `command`, `event`, `response`, `provider`,
`store`, `runtime`), and every public module of the runtime appears under `runtime`, which the
facade's own tests check.

{{crates}}

`turnframe-macros` is reserved. No macros ship in the 0.1 series, by policy: a domain is plain Rust
types and trait implementations.

## A provider

Each adapter is a builder that ends in a `ModelProvider`. This is the OpenAI one, as the console
example builds it from the environment:

```rust
use std::sync::Arc;

use turnframe::provider::openai::OpenAiProvider;
use turnframe::provider::provider::ModelProvider;
use turnframe::provider::secret::ApiKey;

let provider: Arc<dyn ModelProvider> = Arc::new(
    OpenAiProvider::openai()
        .api_key(ApiKey::new(std::env::var("OPENAI_API_KEY")?))
        .model("gpt-5.4-mini")
        .build()?,
);
```

The provider goes into a `ProviderPool`, which routes each task to a model that declares the
capabilities it needs. How routing, fallback and each adapter work is in
[provider adapters](/docs/provider-adapters).

## Next

The [quickstart](/docs/quickstart) runs one turn end to end with the test kit, which is the fastest
way to see every piece in place. Turn on the `test-kit` feature to run it.
