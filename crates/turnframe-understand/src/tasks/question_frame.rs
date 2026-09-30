//! `question_frame`: which record a question is about, and which declared subjects.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use turnframe_core::understanding::QuestionTopic;

use crate::input::{RecordBrief, UnderstandingInput};
use crate::render;
use crate::schema::{array, object, one_of};
use crate::tasks::check_one_of;
use crate::words::Span;

/// The answer for a question about no record.
pub const NONE: &str = "none";

/// The topics a question may have, as a model names them.
pub const TOPICS: [&str; 5] = [
    "record_state",
    "accepted_values",
    "capabilities",
    "ability",
    "knowledge",
];

const BUILT_IN: &str = include_str!("../../prompts/understand/question_frame.md");

/// The question-framing task, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct QuestionFrame<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> QuestionFrame<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }

    /// The topics on offer: knowledge only where a source can answer it.
    fn topics(&self) -> Vec<String> {
        TOPICS
            .iter()
            .filter(|topic| self.turn.knowledge || **topic != "knowledge")
            .map(|topic| (*topic).to_owned())
            .collect()
    }
}

/// One question and what it may be about.
#[derive(Debug, Clone)]
pub struct QuestionInput<'a> {
    /// Its words.
    pub words: Span,
    /// The records it may be about, listed as `r1`, `r2`...
    pub records: Vec<&'a RecordBrief>,
    /// The subjects it may be about.
    pub subjects: Vec<&'a str>,
}

impl QuestionInput<'_> {
    /// Every record answer allowed.
    #[must_use]
    pub fn choices(&self) -> Vec<String> {
        let mut choices: Vec<String> = (1..=self.records.len()).map(|n| format!("r{n}")).collect();
        choices.push(NONE.to_owned());
        choices
    }

    /// The record listed under `handle`.
    #[must_use]
    pub fn record(&self, handle: &str) -> Option<&RecordBrief> {
        let position: usize = handle.strip_prefix('r')?.parse().ok()?;
        self.records.get(position.checked_sub(1)?).copied()
    }
}

/// What a question is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Framing {
    /// What kind of thing it asks, one of [`TOPICS`].
    pub topic: String,
    /// A record handle, or `none`.
    pub record: String,
    /// The subjects, among those listed.
    #[serde(default)]
    pub subjects: Vec<String>,
}

impl Framing {
    /// Whether the question asks if one particular thing can be done, which asks for it.
    #[must_use]
    pub fn asks_for_it(&self) -> bool {
        self.topic == "ability"
    }

    /// The topic, as the understanding records it.
    #[must_use]
    pub fn topic(&self) -> QuestionTopic {
        match self.topic.as_str() {
            "accepted_values" => QuestionTopic::AcceptedValues,
            "capabilities" | "ability" => QuestionTopic::Capabilities,
            "knowledge" => QuestionTopic::Knowledge,
            _ => QuestionTopic::RecordState,
        }
    }
}

impl<'a> ModelTask for QuestionFrame<'a> {
    type Input = QuestionInput<'a>;
    type Output = Framing;

    fn kind(&self) -> TaskKind {
        TaskKind::QuestionFrame
    }

    fn prompt_name(&self) -> &str {
        "understand.question_frame"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, input: &QuestionInput<'a>) -> Value {
        let mut properties = vec![
            ("topic", one_of(self.topics())),
            ("record", one_of(input.choices())),
        ];
        if !input.subjects.is_empty() {
            properties.push(("subjects", array(one_of(input.subjects.iter().copied()))));
        }
        object(properties)
    }

    fn render(&self, input: &QuestionInput<'a>) -> Vec<Message> {
        let mut records = String::from("Records:");
        let mut last_about = Vec::new();
        for (position, record) in input.records.iter().enumerate() {
            let handle = format!("r{}", position + 1);
            let _ = write!(
                records,
                "\n- {handle}: {}",
                render::record_line(record, true)
            );
            if !record.obligations.is_empty() {
                let _ = write!(records, " · still needs: {}", record.obligations.join("; "));
            }
            if self.turn.last_subjects.contains(&record.token) {
                last_about.push(handle);
            }
        }
        let _ = write!(records, "\n- {NONE}: no record");
        let conversation = (!last_about.is_empty()).then(|| {
            format!(
                "The last assistant message was about {}.",
                last_about.join(" and ")
            )
        });
        let subjects = (!input.subjects.is_empty())
            .then(|| format!("Subjects: {}", input.subjects.join(", ")));
        let words = &self.turn.message;
        vec![Message::user(render::sections([
            Some(records),
            subjects,
            conversation,
            render::last_assistant(self.turn),
            Some(render::message(words)),
            Some(render::unit("Question", words, input.words)),
        ]))]
    }

    fn check(&self, input: &QuestionInput<'a>, output: &Framing) -> Result<(), StructuralError> {
        check_one_of("topic", &output.topic, &self.topics())?;
        check_one_of("record", &output.record, &input.choices())?;
        let subjects: Vec<String> = input.subjects.iter().map(|s| (*s).to_owned()).collect();
        for subject in &output.subjects {
            check_one_of("subjects", subject, &subjects)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topics(turn: &UnderstandingInput) -> Vec<String> {
        let input = QuestionInput {
            words: Span::new(0, 1),
            records: Vec::new(),
            subjects: Vec::new(),
        };
        let schema = QuestionFrame::new(turn).schema(&input);
        schema["properties"]["topic"]["enum"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn knowledge_is_offered_only_where_a_source_holds_some() {
        let turn = UnderstandingInput::new("what is it?", "en-GB", chrono::NaiveDate::MIN);
        assert!(topics(&turn).contains(&"knowledge".to_owned()));
        let without = turn.with_knowledge(false);
        assert!(!topics(&without).contains(&"knowledge".to_owned()));
        assert!(topics(&without).contains(&"record_state".to_owned()));
    }
}
