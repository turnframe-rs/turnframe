//! What a model task is: one narrow question, its context, and the shape of its answer.

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;
use turnframe_provider::request::Message;

/// The kind of a task, which is also the purpose a provider router routes it by.
pub use turnframe_provider::purpose::ModelPurpose as TaskKind;

/// One narrow question put to a model.
///
/// Implementations are pure: they render messages and schemas from their input and
/// check answers structurally. Everything about calling a model belongs to the engine.
pub trait ModelTask: Send + Sync {
    /// Everything the task's context is built from.
    type Input: Send + Sync;
    /// The answer, as the schema allows it.
    type Output: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;

    /// The task kind, which selects the profile and the route.
    fn kind(&self) -> TaskKind;

    /// The prompt name a prompt source is asked for, such as `understand.segment`.
    fn prompt_name(&self) -> &str;

    /// The built-in instructions, used when no source supplies the prompt.
    fn instructions(&self) -> &str;

    /// The answer's JSON Schema for this input, with its closed sets filled in.
    fn schema(&self, input: &Self::Input) -> serde_json::Value;

    /// The context messages, static parts first.
    fn render(&self, input: &Self::Input) -> Vec<Message>;

    /// Structural checks the schema cannot express, such as a pointer in range.
    ///
    /// # Errors
    ///
    /// A [`StructuralError`] naming what is wrong, which a repair round quotes.
    fn check(&self, _input: &Self::Input, _output: &Self::Output) -> Result<(), StructuralError> {
        Ok(())
    }

    /// Whether two answers are the same answer, for voting.
    fn agree(&self, left: &Self::Output, right: &Self::Output) -> bool {
        serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
    }
}

/// A structural problem with an answer, worded for the repair round that quotes it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct StructuralError {
    /// Stable code, for records and metrics.
    pub code: &'static str,
    /// What is wrong, in words the model can act on.
    pub message: String,
}

impl StructuralError {
    /// A structural error with its code and message.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Where a task call sits in its turn, as a path: `turn/segment`, `u2/extract`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(String);

impl TaskId {
    /// A top-level identifier.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    /// A child identifier: `u2` then `extract` gives `u2/extract`.
    #[must_use]
    pub fn child(&self, segment: impl fmt::Display) -> Self {
        Self(format!("{}/{segment}", self.0))
    }

    /// The same task with a suffix naming one call of it: `u2/extract#repair1`.
    #[must_use]
    pub fn call(&self, suffix: impl fmt::Display) -> String {
        format!("{}#{suffix}", self.0)
    }

    /// The path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_read_as_the_graph_they_ran_as() {
        let unit = TaskId::new("u2");
        let extract = unit.child("extract");
        assert_eq!(extract.as_str(), "u2/extract");
        assert_eq!(extract.call("repair1"), "u2/extract#repair1");
    }
}
