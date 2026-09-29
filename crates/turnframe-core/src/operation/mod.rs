//! What a workflow offers to do: operations, their arguments, and examples of each.
//!
//! An [`OperationSpec`] is built once per view with a builder, so a field added later
//! breaks nobody's code. Its arguments come from a typed arguments struct, and each can
//! be given labels, a source and a shape. Examples show a message and the arguments it
//! carries, or the arguments it leaves out; [`OperationSpec::validate`] checks every
//! example against the arguments type when the registry is built, so an example cannot
//! go stale unnoticed.

mod argument;
mod value;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use schemars::{JsonSchema, Schema};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ids::{OperationKey, ReadToolKey, WorkflowKey};
use crate::locale::Locale;
use crate::plan::{ActAvailability, ActMutability, TargetPolicy};

pub use argument::{ArgumentLabel, ArgumentSource, ArgumentSpec, ValueShape, shape_of};
pub use value::{
    DateDirection, DateError, DateExpr, DatePeriod, DateUnit, DayOfWeek, Money, MoneyError,
    PeriodOccurrence, WeekdayOccurrence,
};

type Validator = Arc<dyn Fn(&serde_json::Value) -> Result<(), String> + Send + Sync>;

/// A word a workflow's users say, and what it means there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossaryTerm {
    /// The word, as users say it.
    pub term: String,
    /// What it means in this workflow.
    pub meaning: String,
}

impl GlossaryTerm {
    /// A term and its meaning.
    #[must_use]
    pub fn new(term: impl Into<String>, meaning: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            meaning: meaning.into(),
        }
    }
}

/// A message and what it means for one operation, shown to the model as an example.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OperationExample {
    /// What a user wrote.
    pub message: String,
    /// The arguments it gives, by name.
    #[serde(default)]
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The arguments it names or implies without giving a value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_given: Vec<String>,
}

/// Why an operation's declaration cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("operation {operation}: {reason}")]
pub struct OperationSpecError {
    /// The operation.
    pub operation: OperationKey,
    /// What is wrong with its declaration.
    pub reason: String,
}

/// One operation a workflow offers in a view.
#[derive(Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OperationSpec {
    /// Its key, unique across the registry.
    pub key: OperationKey,
    /// The workflow offering it; the registry fills it in.
    pub workflow: WorkflowKey,
    /// One line saying what it does, for choosing among operations.
    pub summary: String,
    /// The summary in other languages, by locale; the one above serves every other.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub summaries: BTreeMap<Locale, String>,
    /// Longer guidance for filling its arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    /// Which records it may aim at.
    pub target_policy: TargetPolicy,
    /// Whether it changes a record.
    pub mutability: ActMutability,
    /// Whether a model may propose it, or only a card.
    pub availability: ActAvailability,
    /// The JSON Schema of its arguments type.
    pub arguments_schema: Schema,
    /// Its arguments, in the schema's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<ArgumentSpec>,
    /// Examples of messages and what they give.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<OperationExample>,
    /// Reads whose results the argument filler is to be shown; declared, not yet run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_reads: Vec<ReadToolKey>,
    #[serde(skip)]
    validator: Option<Validator>,
    #[serde(skip)]
    problems: Vec<String>,
}

impl fmt::Debug for OperationSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperationSpec")
            .field("key", &self.key)
            .field("workflow", &self.workflow)
            .field("summary", &self.summary)
            .field("target_policy", &self.target_policy)
            .field("arguments", &self.arguments)
            .field("examples", &self.examples.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq for OperationSpec {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.workflow == other.workflow
            && self.summary == other.summary
            && self.summaries == other.summaries
            && self.guidance == other.guidance
            && self.target_policy == other.target_policy
            && self.mutability == other.mutability
            && self.availability == other.availability
            && self.arguments_schema == other.arguments_schema
            && self.arguments == other.arguments
            && self.examples == other.examples
            && self.context_reads == other.context_reads
    }
}

impl OperationSpec {
    /// An operation named `key` that takes no arguments and changes an existing record.
    #[must_use]
    pub fn new(key: impl Into<OperationKey>) -> Self {
        Self {
            key: key.into(),
            workflow: WorkflowKey::from(""),
            summary: String::new(),
            summaries: BTreeMap::new(),
            guidance: None,
            target_policy: TargetPolicy::RequiresExistingCase,
            mutability: ActMutability::Mutating,
            availability: ActAvailability::Proposable,
            arguments_schema: schemars::schema_for!(()),
            arguments: Vec::new(),
            examples: Vec::new(),
            context_reads: Vec::new(),
            validator: None,
            problems: Vec::new(),
        }
    }

    /// Says in one line what it does.
    #[must_use]
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    /// Says what it does in `locale`'s language, for a turn in that language.
    #[must_use]
    pub fn summary_in(mut self, locale: impl Into<Locale>, summary: impl Into<String>) -> Self {
        self.summaries.insert(locale.into(), summary.into());
        self
    }

    /// Its summary for a turn in `locale`: that locale's, then its language's, then the
    /// summary every other language reads.
    #[must_use]
    pub fn summary_for(&self, locale: &Locale) -> &str {
        self.summaries
            .get(locale)
            .or_else(|| {
                self.summaries
                    .iter()
                    .find(|(candidate, _)| candidate.same_language(locale))
                    .map(|(_, summary)| summary)
            })
            .map_or(self.summary.as_str(), String::as_str)
    }

    /// Adds guidance for filling its arguments.
    #[must_use]
    pub fn guidance(mut self, guidance: impl Into<String>) -> Self {
        self.guidance = Some(guidance.into());
        self
    }

    /// Sets which records it may aim at.
    #[must_use]
    pub const fn target(mut self, policy: TargetPolicy) -> Self {
        self.target_policy = policy;
        self
    }

    /// Marks it as changing nothing.
    #[must_use]
    pub const fn read_only(mut self) -> Self {
        self.mutability = ActMutability::ReadOnly;
        self
    }

    /// Marks it as changing a record, which is the default.
    #[must_use]
    pub const fn mutating(mut self) -> Self {
        self.mutability = ActMutability::Mutating;
        self
    }

    /// Lets only a card run it.
    #[must_use]
    pub const fn card_only(mut self) -> Self {
        self.availability = ActAvailability::CardOnly;
        self
    }

    /// Takes its arguments from `A`, deriving one [`ArgumentSpec`] per top-level field.
    #[must_use]
    pub fn arguments<A: JsonSchema + DeserializeOwned + 'static>(mut self) -> Self {
        self.arguments_schema = schemars::schema_for!(A);
        self.arguments = derive_arguments(&self.arguments_schema);
        self.validator = Some(Arc::new(|value| {
            serde_json::from_value::<A>(value.clone())
                .map(|_| ())
                .map_err(|error| error.to_string())
        }));
        self
    }

    /// Adjusts the argument named `name`.
    #[must_use]
    pub fn argument(
        mut self,
        name: &str,
        adjust: impl FnOnce(ArgumentSpec) -> ArgumentSpec,
    ) -> Self {
        match self
            .arguments
            .iter()
            .position(|argument| argument.name == name)
        {
            Some(index) => {
                let current = self.arguments.remove(index);
                self.arguments.insert(index, adjust(current));
            }
            None => self
                .problems
                .push(format!("`{name}` is not a field of its arguments type")),
        }
        self
    }

    /// Adds an example message and the arguments it gives.
    #[must_use]
    pub fn example(mut self, message: impl Into<String>, arguments: serde_json::Value) -> Self {
        let arguments = match arguments {
            serde_json::Value::Object(map) => map,
            serde_json::Value::Null => serde_json::Map::new(),
            other => {
                self.problems.push(format!(
                    "an example's arguments must be an object, not {other}"
                ));
                serde_json::Map::new()
            }
        };
        self.examples.push(OperationExample {
            message: message.into(),
            arguments,
            not_given: Vec::new(),
        });
        self
    }

    /// Adds an example message that names or implies `names` without giving them.
    #[must_use]
    pub fn example_not_given<'a>(
        mut self,
        message: impl Into<String>,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        self.examples.push(OperationExample {
            message: message.into(),
            arguments: serde_json::Map::new(),
            not_given: names.into_iter().map(str::to_owned).collect(),
        });
        self
    }

    /// Names a read whose result the argument filler is to be shown. Declared only:
    /// no stage runs reads in 0.1 (`docs/roadmap.md`).
    #[must_use]
    pub fn context_read(mut self, read: impl Into<ReadToolKey>) -> Self {
        self.context_reads.push(read.into());
        self
    }

    /// The arguments document an act passes: `null` for an operation declared with no
    /// arguments type, an object otherwise.
    #[must_use]
    pub fn arguments_value(
        &self,
        values: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> serde_json::Value {
        let values: serde_json::Map<String, serde_json::Value> = values.into_iter().collect();
        let takes_null = self
            .arguments_schema
            .as_value()
            .get("type")
            .and_then(serde_json::Value::as_str)
            == Some("null");
        if takes_null && values.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Object(values)
        }
    }

    /// Checks an act's arguments against the arguments type.
    ///
    /// # Errors
    ///
    /// Why they do not fit, as a path and not as the value.
    pub fn check_arguments(&self, arguments: &serde_json::Value) -> Result<(), String> {
        if let Some(validator) = &self.validator {
            return validator(arguments);
        }
        crate::schema::validate_against(&self.arguments_schema, arguments)
            .map_err(|error| error.to_string())
    }

    /// The argument named `name`.
    #[must_use]
    pub fn argument_named(&self, name: &str) -> Option<&ArgumentSpec> {
        self.arguments.iter().find(|argument| argument.name == name)
    }

    /// Checks the declaration: every adjusted argument exists, and every example gives
    /// arguments of the right type and names only real ones.
    ///
    /// # Errors
    ///
    /// The first problem found, naming the operation.
    pub fn validate(&self) -> Result<(), OperationSpecError> {
        let refuse = |reason: String| OperationSpecError {
            operation: self.key.clone(),
            reason,
        };
        if let Some(problem) = self.problems.first() {
            return Err(refuse(problem.clone()));
        }
        if self.summary.trim().is_empty() {
            return Err(refuse("has no summary".to_owned()));
        }
        for example in &self.examples {
            for name in example.not_given.iter().chain(example.arguments.keys()) {
                if self.argument_named(name).is_none() {
                    return Err(refuse(format!(
                        "example «{}» names `{name}`, which is not an argument",
                        example.message
                    )));
                }
            }
            if example.not_given.is_empty()
                && let Some(validator) = &self.validator
            {
                validator(&serde_json::Value::Object(example.arguments.clone()))
                    .map_err(|error| refuse(format!("example «{}»: {error}", example.message)))?;
            }
        }
        Ok(())
    }
}

/// The operations offered to a turn, by key. A duplicate key is refused, never shadowed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OperationCatalog {
    operations: indexmap::IndexMap<OperationKey, OperationSpec>,
}

impl OperationCatalog {
    /// A catalog of `operations`.
    ///
    /// # Errors
    ///
    /// [`crate::error::ReductionError::DuplicateOperation`] for a key offered twice.
    pub fn new(
        operations: impl IntoIterator<Item = OperationSpec>,
    ) -> Result<Self, crate::error::ReductionError> {
        let mut catalog = Self::default();
        for spec in operations {
            catalog.insert(spec)?;
        }
        Ok(catalog)
    }

    /// Adds an operation.
    ///
    /// # Errors
    ///
    /// [`crate::error::ReductionError::DuplicateOperation`] when its key is taken.
    pub fn insert(&mut self, spec: OperationSpec) -> Result<(), crate::error::ReductionError> {
        if self.operations.contains_key(&spec.key) {
            return Err(crate::error::ReductionError::DuplicateOperation {
                operation: spec.key,
            });
        }
        self.operations.insert(spec.key.clone(), spec);
        Ok(())
    }

    /// The operation with this key.
    #[must_use]
    pub fn get(&self, key: &OperationKey) -> Option<&OperationSpec> {
        self.operations.get(key)
    }

    /// Every operation, in the order added.
    pub fn iter(&self) -> impl Iterator<Item = &OperationSpec> {
        self.operations.values()
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

/// One argument per top-level property, required as the schema says.
fn derive_arguments(schema: &Schema) -> Vec<ArgumentSpec> {
    let value = schema.as_value();
    let Some(properties) = value
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let required: Vec<&str> = value
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|names| names.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    let defs = value.get("$defs");
    properties
        .iter()
        .map(|(name, property)| {
            let mut spec = ArgumentSpec::new(name.clone(), shape_of(property, defs));
            spec.required = required.contains(&name.as_str());
            spec.description = property
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            spec
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct SetSubject {
        /// What the trip is called.
        value: String,
    }

    fn set_subject() -> OperationSpec {
        OperationSpec::new("trip.set_name")
            .summary("Name the trip.")
            .arguments::<SetSubject>()
            .argument("value", |a| a.label("subject").label_in("it-IT", "oggetto"))
    }

    #[test]
    fn arguments_come_from_the_type_with_their_documentation() {
        let spec = set_subject();
        let value = spec.argument_named("value").unwrap();
        assert!(value.required);
        assert_eq!(
            value.description.as_deref(),
            Some("What the trip is called.")
        );
        assert_eq!(value.shape, ValueShape::Text { written: false });
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn an_example_that_does_not_fit_the_arguments_type_is_refused() {
        let wrong = set_subject().example("the subject is March", json!({"value": 3}));
        assert!(wrong.validate().is_err());
        let unknown = set_subject().example_not_given("set the subject", ["title"]);
        assert!(unknown.validate().is_err());
        let fine = set_subject()
            .example("the subject is March", json!({"value": "March"}))
            .example_not_given("the subject needs changing", ["value"]);
        assert!(fine.validate().is_ok());
    }

    #[test]
    fn adjusting_an_argument_the_type_does_not_have_is_refused() {
        let spec = set_subject().argument("title", |a| a.label("title"));
        assert!(spec.validate().unwrap_err().reason.contains("`title`"));
    }

    #[test]
    fn a_summary_is_read_in_the_turns_language_when_it_has_one() {
        let spec = OperationSpec::new("trip.set_name")
            .summary("Name the trip.")
            .summary_in("it-IT", "Dà un nome al viaggio.");
        assert_eq!(
            spec.summary_for(&Locale::from("it-IT")),
            "Dà un nome al viaggio."
        );
        assert_eq!(
            spec.summary_for(&Locale::from("it-CH")),
            "Dà un nome al viaggio."
        );
        assert_eq!(spec.summary_for(&Locale::from("de-DE")), "Name the trip.");
    }
}
