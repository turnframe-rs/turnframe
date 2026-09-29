//! The model and the configuration, from the environment.

use std::sync::Arc;

use anyhow::Context as _;
use turnframe::provider::provider::ModelProvider;
use turnframe::provider::secret::ApiKey;
use turnframe::runtime::config::{NarrationConfig, OrchestratorConfig};

/// The provider named by whichever key is in the environment; Ollama needs none.
pub fn provider_from_environment() -> anyhow::Result<(Arc<dyn ModelProvider>, String)> {
    let model = std::env::var("TURNFRAME_MODEL").ok();
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let model = model.unwrap_or_else(|| "gpt-4o-mini".to_owned());
        let provider = turnframe::provider::openai::OpenAiProvider::openai()
            .api_key(ApiKey::new(key))
            .model(model.clone())
            .build()?;
        return Ok((Arc::new(provider), format!("openai / {model}")));
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let model = model.unwrap_or_else(|| "claude-haiku-4-5-20251001".to_owned());
        let provider = turnframe::provider::anthropic::AnthropicProvider::anthropic()
            .api_key(ApiKey::new(key))
            .model(model.clone())
            .build()?;
        return Ok((Arc::new(provider), format!("anthropic / {model}")));
    }
    if let Ok(key) = std::env::var("GEMINI_API_KEY") {
        let model = model.unwrap_or_else(|| "gemini-2.0-flash".to_owned());
        let provider = turnframe::provider::gemini::GeminiProvider::gemini()
            .api_key(ApiKey::new(key))
            .model(model.clone())
            .build()?;
        return Ok((Arc::new(provider), format!("gemini / {model}")));
    }
    let model = model.unwrap_or_else(|| "qwen3:8b".to_owned());
    let builder = match std::env::var("OLLAMA_URL").ok().as_deref() {
        Some(url) => turnframe::provider::ollama::OllamaProvider::at(url),
        None => turnframe::provider::ollama::OllamaProvider::local(),
    };
    let provider = builder.model(model.clone()).build()?;
    Ok((Arc::new(provider), format!("ollama / {model}")))
}

/// The conservative configuration with narration on, and the file `TURNFRAME_CONFIG`
/// names merged over it: a partial file changes only what it states.
pub fn config_from_environment() -> anyhow::Result<OrchestratorConfig> {
    let base = OrchestratorConfig::conservative().with_narration(NarrationConfig::conservative());
    let Ok(path) = std::env::var("TURNFRAME_CONFIG") else {
        return Ok(base);
    };
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    let overrides: toml::Table =
        toml::from_str(&text).with_context(|| format!("parsing {path}"))?;
    let mut merged = toml::Table::try_from(&base)?;
    merge(&mut merged, overrides);
    let config: OrchestratorConfig = toml::Value::Table(merged)
        .try_into()
        .with_context(|| format!("{path} is not a valid configuration"))?;
    config.validate()?;
    Ok(config)
}

fn merge(into: &mut toml::Table, from: toml::Table) {
    for (key, value) in from {
        match (into.get_mut(&key), value) {
            (Some(toml::Value::Table(inner)), toml::Value::Table(table)) => merge(inner, table),
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}
