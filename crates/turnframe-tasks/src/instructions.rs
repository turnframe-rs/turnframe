//! Which instruction text a task call runs under, and the reference the record cites.
//!
//! A configured prompt source is asked for `<name>.<locale>`, then `<name>`. Without a
//! source, or when it has neither, the task's built-in text is used under a reference
//! whose version is `builtin` and whose hash is the text's, so a record always says
//! which words a call ran under.

use std::sync::Arc;

use turnframe_core::locale::Locale;
use turnframe_core::prompt::{PromptName, PromptRef, PromptSelector, PromptSource};

/// The version every built-in instruction text is recorded under.
pub const BUILT_IN_VERSION: &str = "builtin";

/// Instruction text and the reference to record for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instructions {
    /// The text sent to the model.
    pub text: String,
    /// What the task record cites.
    pub reference: PromptRef,
}

impl Instructions {
    /// Built-in text under its own content-hash reference.
    #[must_use]
    pub fn built_in(name: &str, text: &str) -> Self {
        Self {
            text: text.to_owned(),
            reference: PromptRef::of_text(name, BUILT_IN_VERSION, text),
        }
    }
}

/// Resolves the instructions for `name` in `locale`.
pub async fn resolve(
    source: Option<&Arc<dyn PromptSource>>,
    selector: &PromptSelector,
    name: &str,
    locale: &Locale,
    built_in: &str,
) -> Instructions {
    let Some(source) = source else {
        return Instructions::built_in(name, built_in);
    };
    for candidate in [format!("{name}.{}", locale.as_str()), name.to_owned()] {
        match source
            .load(&PromptName::new(candidate.as_str()), selector)
            .await
        {
            Ok(loaded) => {
                let (reference, text) = loaded.into_parts();
                return Instructions { text, reference };
            }
            Err(error) => {
                tracing::debug!(
                    target: "turnframe.tasks",
                    prompt = candidate.as_str(),
                    error_code = error.code(),
                    "prompt source has no text for this name"
                );
            }
        }
    }
    Instructions::built_in(name, built_in)
}

#[cfg(test)]
mod tests {
    use turnframe_core::prompt::{LoadedPrompt, PromptError};

    use super::*;

    #[derive(Debug)]
    struct Italian;

    #[async_trait::async_trait]
    impl PromptSource for Italian {
        async fn load(
            &self,
            name: &PromptName,
            _selector: &PromptSelector,
        ) -> Result<LoadedPrompt, PromptError> {
            if name.as_str() == "understand.segment.it-IT" {
                return Ok(LoadedPrompt::new(
                    name.clone(),
                    "v2",
                    "Dividi il messaggio.",
                ));
            }
            Err(PromptError::NotFound { name: name.clone() })
        }

        fn describe(&self) -> &'static str {
            "italian"
        }
    }

    #[tokio::test]
    async fn a_locale_specific_prompt_wins_and_the_built_in_text_is_the_fallback() {
        let source: Arc<dyn PromptSource> = Arc::new(Italian);
        let italian = resolve(
            Some(&source),
            &PromptSelector::Latest,
            "understand.segment",
            &Locale::from("it-IT"),
            "Split the message.",
        )
        .await;
        assert_eq!(italian.text, "Dividi il messaggio.");
        assert_eq!(italian.reference.version.as_str(), "v2");

        let english = resolve(
            Some(&source),
            &PromptSelector::Latest,
            "understand.segment",
            &Locale::from("en-GB"),
            "Split the message.",
        )
        .await;
        assert_eq!(english.text, "Split the message.");
        assert_eq!(english.reference.version.as_str(), BUILT_IN_VERSION);
        assert!(english.reference.matches("Split the message."));
    }
}
