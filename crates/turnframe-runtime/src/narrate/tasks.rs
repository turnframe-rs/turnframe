//! The three narration tasks: acknowledge the turn, answer a question, review a reply.

use std::fmt::Write as _;
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use turnframe_core::response::{NarratableFact, ToneProfile};
use turnframe_provider::request::{ContentPart, Message, Role};
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use super::outcome::TurnOutcome;
use crate::conversation::{RecentMessage, TranscriptRole};

const ACKNOWLEDGE: &str = include_str!("../../prompts/narrate/acknowledge.md");
const ANSWER: &str = include_str!("../../prompts/narrate/answer.md");
const REVIEW: &str = include_str!("../../prompts/narrate/review.md");
const STEP: &str = include_str!("../../prompts/narrate/step.md");

/// The longest progress line, in characters.
const STEP_CHARS: usize = 160;

/// The opening line of a writing task: the language, and the voice when one is set.
fn opening(locale: &str, tone: ToneProfile) -> String {
    let voice = match tone {
        ToneProfile::Warm => " Keep the voice warm and friendly.",
        ToneProfile::Formal => " Keep the voice formal.",
        ToneProfile::Concise => " Use as few words as possible.",
        _ => "",
    };
    format!("Write in the language of the locale {locale}.{voice}")
}

fn text_only() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["text"],
        "properties": {"text": {"type": "string"}}
    })
}

fn transcript_section(out: &mut String, transcript: &[RecentMessage]) {
    if transcript.is_empty() {
        return;
    }
    out.push_str("\n\nConversation so far:");
    for message in transcript {
        let who = match message.role {
            TranscriptRole::User => "user",
            TranscriptRole::Assistant => "assistant",
        };
        let _ = write!(out, "\n- {who}: {}", message.text);
    }
}

fn written(text: &str, max_chars: Option<usize>) -> Result<(), StructuralError> {
    if text.trim().is_empty() {
        return Err(StructuralError::new("empty_text", "the text is empty"));
    }
    match max_chars {
        Some(max) if text.chars().count() > max => Err(StructuralError::new(
            "too_long",
            format!("the text is longer than {max} characters: say it more briefly"),
        )),
        _ => Ok(()),
    }
}

/// Text a model wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Written {
    /// The text.
    pub text: String,
}

/// What the acknowledgement is written from.
pub(crate) struct AcknowledgeInput<'a> {
    pub outcome: &'a TurnOutcome,
    /// The turn's locale: the language every block of the reply is in.
    pub locale: &'a str,
    pub tone: ToneProfile,
    pub message: Option<&'a str>,
    /// What stands beside the reply: the card on screen.
    pub on_screen: &'a [String],
    /// The answers to the user's questions, already checked, which the reply gives.
    pub answers: &'a [String],
    /// The user's questions no fact answers.
    pub unanswered: &'a [String],
    /// What the server tells the user this turn, which the reply gives.
    pub notices: &'a [String],
    pub transcript: &'a [RecentMessage],
    pub guidance: &'a [String],
}

/// What each part of an outcome is for, said only for the parts an outcome holds: a
/// small model acts on a note about a part that is not there.
fn outcome_notes(outcome: &TurnOutcome, carries: bool) -> Vec<&'static str> {
    if outcome.only_asks() && !carries {
        return vec![
            "ask is all the outcome holds: the reply is that one question, in your own words, about that record, with nothing before it. When it has a because, say that reason first, in a few words.",
        ];
    }
    let mut notes = Vec::new();
    if !outcome.done.is_empty() {
        notes.push("done lists what this turn did. Acknowledge it in a few words of your own.");
    }
    if !outcome.not_done.is_empty() {
        notes.push("not_done lists what was not done, each with its reason. When a notice on screen already gives the reason, do not give it again: a few words linking it to the ask are enough.");
    }
    if !outcome.disputes.is_empty() {
        notes.push("disputes lists what the user said you got wrong. Own it in a few words; nothing was reverted. When the ask is a change you reported, ask what it should be instead.");
    }
    if !outcome.starting.is_empty() {
        notes.push("starting lists records the turn began without writing anything yet. Say you are starting one; with no ask, ask what the workflow guidance says to ask first.");
    }
    if outcome.ask.is_some() {
        notes.push("ask is the one thing to ask for next. End with exactly that question, in your own words, about that record. Ask for nothing else, and offer no list of options. When it has a because, say that reason first, in a few words.");
    }
    if outcome.card.is_some() {
        notes.push("card is a card on screen with its buttons. Point the user to it briefly.");
    }
    if !outcome.next.is_empty() {
        notes.push("next lists what the user may do now that the record needs nothing more. End by offering it in a few words of your own, as one question.");
    }
    notes
}

/// What the facts of an answer are for, said only for the kinds it was given.
fn fact_notes(facts: &[NarratableFact]) -> Vec<&'static str> {
    let has = |kind: &str| {
        facts.iter().any(|fact| {
            serde_json::to_value(fact)
                .ok()
                .and_then(|value| value.get("kind").cloned())
                .is_some_and(|found| found == kind)
        })
    };
    let mut notes = Vec::new();
    if has("operation_available") {
        notes.push("operation_available facts say what the user can do. Their summaries are written for the software, not for the user: say what each lets the user do in plain words of your own, never by an operation's key, and leave out how its values are given. Asked whether they can do one thing on offer, say yes, then ask for what it needs, as a question.");
    }
    if has("record") || has("state_value") || has("obligation_open") {
        notes.push("record, state_value and obligation_open facts answer a question about a record: which record it is and where it stands, what it holds, and what it still needs.");
    }
    if has("knowledge") {
        notes.push("knowledge facts are what a source says: answer from them and nothing else.");
    }
    notes
}

/// Acknowledges the turn from its outcome, and asks its one question.
pub(crate) struct Acknowledge<'a> {
    pub max_chars: Option<usize>,
    pub input: PhantomData<&'a ()>,
}

impl<'a> ModelTask for Acknowledge<'a> {
    type Input = AcknowledgeInput<'a>;
    type Output = Written;

    fn kind(&self) -> TaskKind {
        TaskKind::Acknowledge
    }

    fn prompt_name(&self) -> &str {
        "narrate.acknowledge"
    }

    fn instructions(&self) -> &str {
        ACKNOWLEDGE
    }

    fn schema(&self, _input: &AcknowledgeInput<'a>) -> Value {
        text_only()
    }

    fn render(&self, input: &AcknowledgeInput<'a>) -> Vec<Message> {
        let mut out = opening(input.locale, input.tone);
        if let Some(message) = input.message {
            let _ = write!(out, "\n\nThe user wrote: «{message}»");
        }
        transcript_section(&mut out, input.transcript);
        if !input.guidance.is_empty() {
            let _ = write!(out, "\n\nWorkflow guidance:\n{}", input.guidance.join("\n"));
        }
        if !input.on_screen.is_empty() {
            let _ = write!(
                out,
                "\n\nOn screen beside your reply:\n- {}",
                input.on_screen.join("\n- ")
            );
        }
        if !input.answers.is_empty() {
            let _ = write!(
                out,
                "\n\nAnswers to give in your reply:\n- {}",
                input.answers.join("\n- ")
            );
        }
        if !input.unanswered.is_empty() {
            let _ = write!(
                out,
                "\n\nQuestions no fact answers:\n- {}",
                input.unanswered.join("\n- ")
            );
        }
        if !input.notices.is_empty() {
            let _ = write!(
                out,
                "\n\nNotices to give in your reply:\n- {}",
                input.notices.join("\n- ")
            );
        }
        let carries =
            !input.answers.is_empty() || !input.notices.is_empty() || !input.unanswered.is_empty();
        let mut notes = outcome_notes(input.outcome, carries);
        if !input.answers.is_empty() {
            notes.push("the answers are already checked: give each as written or in fewer words, before the ask, and add nothing to them.");
        }
        if !input.notices.is_empty() {
            notes.push("the notices are what the server tells the user: give each in a few words of your own, once.");
        }
        if !input.unanswered.is_empty() {
            notes.push("a question no fact answers is not yours to answer: when it says you got something wrong, own it in a few words; otherwise say in a few words that you cannot tell from what you know.");
        }
        let _ = write!(out, "\n\nNotes on the outcome:\n- {}", notes.join("\n- "));
        let outcome = serde_json::to_string_pretty(input.outcome).unwrap_or_default();
        let _ = write!(out, "\n\nOutcome:\n{outcome}");
        vec![Message::user(out)]
    }

    fn check(
        &self,
        _input: &AcknowledgeInput<'a>,
        output: &Written,
    ) -> Result<(), StructuralError> {
        written(&output.text, self.max_chars)
    }
}

/// What one answer is written from.
pub(crate) struct AnswerInput<'a> {
    pub question: &'a str,
    /// The turn's locale: the language every block of the reply is in.
    pub locale: &'a str,
    pub tone: ToneProfile,
    /// The user's last message and the assistant's reply to it, when the question
    /// follows them up.
    pub asked_before: Option<&'a str>,
    pub previous: Option<&'a str>,
    pub facts: &'a [NarratableFact],
    pub guidance: &'a [String],
    pub attachments: &'a [ContentPart],
}

/// Whether the facts answer the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AnswerKind {
    /// They do; the text is the answer.
    Answered,
    /// They do not; the text says why, in a few words.
    CannotAnswer,
}

/// An answer, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Answered {
    /// Whether it is an answer.
    pub kind: AnswerKind,
    /// The answer, or the reason there is none.
    pub text: String,
}

/// Answers one question from the facts it is given.
pub(crate) struct Answer<'a> {
    pub max_chars: Option<usize>,
    pub input: PhantomData<&'a ()>,
}

impl<'a> ModelTask for Answer<'a> {
    type Input = AnswerInput<'a>;
    type Output = Answered;

    fn kind(&self) -> TaskKind {
        TaskKind::Answer
    }

    fn prompt_name(&self) -> &str {
        "narrate.answer"
    }

    fn instructions(&self) -> &str {
        ANSWER
    }

    fn schema(&self, _input: &AnswerInput<'a>) -> Value {
        json!({
            "type": "object", "additionalProperties": false, "required": ["kind", "text"],
            "properties": {
                "kind": {"type": "string", "enum": ["answered", "cannot_answer"]},
                "text": {"type": "string", "description": "The answer, or why there is none."}
            }
        })
    }

    fn render(&self, input: &AnswerInput<'a>) -> Vec<Message> {
        let mut out = format!(
            "{}\n\nQuestion: «{}»",
            opening(input.locale, input.tone),
            input.question
        );
        match (input.asked_before, input.previous) {
            (Some(asked), Some(previous)) => {
                let _ = write!(
                    out,
                    "\n\nIt follows up the user's last message, «{asked}», and the assistant's reply: \
                     «{previous}». They show what the question is about; what is so comes from the \
                     facts alone."
                );
            }
            (None, Some(previous)) => {
                let _ = write!(
                    out,
                    "\n\nIt follows up the assistant's last message: «{previous}»"
                );
            }
            _ => {}
        }
        if !input.guidance.is_empty() {
            let _ = write!(out, "\n\nWorkflow guidance:\n{}", input.guidance.join("\n"));
        }
        let notes = fact_notes(input.facts);
        if !notes.is_empty() {
            let _ = write!(out, "\n\nNotes on the facts:\n- {}", notes.join("\n- "));
        }
        let facts = serde_json::to_string_pretty(input.facts).unwrap_or_default();
        let _ = write!(out, "\n\nFacts:\n{facts}");
        let mut parts = vec![ContentPart::text(out)];
        parts.extend(input.attachments.iter().cloned());
        vec![Message::new(Role::User, parts)]
    }

    fn check(&self, _input: &AnswerInput<'a>, output: &Answered) -> Result<(), StructuralError> {
        match output.kind {
            AnswerKind::Answered => written(&output.text, self.max_chars),
            AnswerKind::CannotAnswer => Ok(()),
        }
    }
}

/// One understanding step, said while the message is read.
pub(crate) struct StepInput<'a> {
    pub locale: &'a str,
    /// The step, as code describes it.
    pub step: &'a str,
}

/// Says one step of the understanding as a progress line, in the turn's language.
pub(crate) struct StepProse<'a>(pub PhantomData<&'a ()>);

impl<'a> ModelTask for StepProse<'a> {
    type Input = StepInput<'a>;
    type Output = Written;

    fn kind(&self) -> TaskKind {
        TaskKind::Progress
    }

    fn prompt_name(&self) -> &str {
        "narrate.step"
    }

    fn instructions(&self) -> &str {
        STEP
    }

    fn schema(&self, _input: &StepInput<'a>) -> Value {
        text_only()
    }

    fn render(&self, input: &StepInput<'a>) -> Vec<Message> {
        vec![Message::user(format!(
            "{}\n\nStep: {}",
            opening(input.locale, ToneProfile::Neutral),
            input.step
        ))]
    }

    fn check(&self, _input: &StepInput<'a>, output: &Written) -> Result<(), StructuralError> {
        written(&output.text, Some(STEP_CHARS))
    }
}

/// A reply and what it may rest on, for review.
pub(crate) struct ReviewInput<'a> {
    pub reply: &'a str,
    pub material: Value,
    /// Whether the material has an ask the reply must end on.
    pub has_ask: bool,
    pub on_screen: &'a [String],
    /// Whether the material holds answers or notices the reply must give.
    pub carries: bool,
}

/// The review's checks, each a yes or no question, with the one it asks.
const CHECKS: [(&str, &str); 5] = [
    ("asks_the_ask", "Does the reply ask the ask, in any words?"),
    (
        "asks_anything_else",
        "Does it ask for anything that is neither the ask nor something an answer in the material says?",
    ),
    (
        "claims_beyond_material",
        "Does it state an action (one it says it is taking included), a value or a promise that neither the material nor the screen holds?",
    ),
    (
        "contradicts_screen",
        "Does it say the opposite of something on screen?",
    ),
    (
        "leaves_something_out",
        "Does it leave out anything one of the answers or notices in the material says?",
    ),
];

impl ReviewInput<'_> {
    /// The checks that apply: none about an ask when there is none, none about the
    /// screen when nothing is on it.
    fn checks(&self) -> Vec<(&'static str, &'static str)> {
        CHECKS
            .into_iter()
            .filter(|(name, _)| match *name {
                "asks_the_ask" | "asks_anything_else" => self.has_ask,
                "contradicts_screen" => !self.on_screen.is_empty(),
                "leaves_something_out" => self.carries,
                _ => true,
            })
            .collect()
    }
}

/// What a review found: its reasoning, then one answer per check that applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Verdict {
    /// The reasoning the answers follow from.
    pub reasoning: String,
    #[serde(default)]
    pub asks_the_ask: Option<bool>,
    #[serde(default)]
    pub asks_anything_else: Option<bool>,
    #[serde(default)]
    pub claims_beyond_material: Option<bool>,
    #[serde(default)]
    pub contradicts_screen: Option<bool>,
    #[serde(default)]
    pub leaves_something_out: Option<bool>,
}

impl Verdict {
    /// Every check the reply failed, by name; empty when it may be shown.
    pub fn issues(&self) -> Vec<&'static str> {
        [
            ("does_not_ask", self.asks_the_ask == Some(false)),
            ("asks_something_else", self.asks_anything_else == Some(true)),
            (
                "claims_not_in_material",
                self.claims_beyond_material == Some(true),
            ),
            (
                "contradicts_a_notice",
                self.contradicts_screen == Some(true),
            ),
            (
                "leaves_out_an_answer_or_notice",
                self.leaves_something_out == Some(true),
            ),
        ]
        .into_iter()
        .filter_map(|(issue, failed)| failed.then_some(issue))
        .collect()
    }

    fn answer(&self, check: &str) -> Option<bool> {
        match check {
            "asks_the_ask" => self.asks_the_ask,
            "asks_anything_else" => self.asks_anything_else,
            "claims_beyond_material" => self.claims_beyond_material,
            "contradicts_screen" => self.contradicts_screen,
            "leaves_something_out" => self.leaves_something_out,
            _ => None,
        }
    }
}

/// Checks a reply against its material, the screen around it, and its ask.
pub(crate) struct Review<'a>(pub PhantomData<&'a ()>);

impl<'a> ModelTask for Review<'a> {
    type Input = ReviewInput<'a>;
    type Output = Verdict;

    fn kind(&self) -> TaskKind {
        TaskKind::Review
    }

    fn prompt_name(&self) -> &str {
        "narrate.review"
    }

    fn instructions(&self) -> &str {
        REVIEW
    }

    fn schema(&self, input: &ReviewInput<'a>) -> Value {
        let checks = input.checks();
        let mut properties = serde_json::Map::new();
        properties.insert("reasoning".to_owned(), json!({"type": "string"}));
        let mut required = vec![json!("reasoning")];
        for (name, question) in checks {
            properties.insert(
                name.to_owned(),
                json!({"type": "boolean", "description": question}),
            );
            required.push(json!(name));
        }
        json!({
            "type": "object", "additionalProperties": false,
            "required": required, "properties": properties
        })
    }

    fn render(&self, input: &ReviewInput<'a>) -> Vec<Message> {
        let mut out = format!("Reply: «{}»", input.reply);
        let material = serde_json::to_string_pretty(&input.material).unwrap_or_default();
        let _ = write!(out, "\n\nMaterial:\n{material}");
        if !input.on_screen.is_empty() {
            let _ = write!(
                out,
                "\n\nOn screen beside it:\n- {}",
                input.on_screen.join("\n- ")
            );
        }
        let checks: Vec<String> = input
            .checks()
            .into_iter()
            .map(|(name, question)| format!("- {name}: {question}"))
            .collect();
        let _ = write!(out, "\n\nChecks:\n{}", checks.join("\n"));
        vec![Message::user(out)]
    }

    fn check(&self, input: &ReviewInput<'a>, output: &Verdict) -> Result<(), StructuralError> {
        match input
            .checks()
            .into_iter()
            .find(|(name, _)| output.answer(name).is_none())
        {
            Some((name, _)) => Err(StructuralError::new(
                "missing_check",
                format!("answer {name} with true or false"),
            )),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpenAI's strict structured output takes an object at the root, every property
    /// required and no other property allowed.
    fn assert_strict(name: &str, schema: &Value) {
        assert_eq!(schema["type"], "object", "{name}: the root is an object");
        assert_eq!(schema["additionalProperties"], false, "{name}");
        let properties: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        for property in properties {
            assert!(
                required.contains(&property.as_str()),
                "{name}: {property} is required"
            );
        }
    }

    #[test]
    fn a_follow_up_is_shown_as_what_was_said_and_not_as_what_is_so() {
        let answer = AnswerInput {
            question: "which one was it?",
            locale: "en-GB",
            tone: ToneProfile::Neutral,
            asked_before: Some("it was reduced rate"),
            previous: Some("Noted."),
            facts: &[],
            guidance: &[],
            attachments: &[],
        };
        let task = Answer {
            max_chars: None,
            input: PhantomData,
        };
        let rendered = format!("{:?}", task.render(&answer));
        assert!(
            rendered.contains("what is so comes from the facts alone"),
            "{rendered}"
        );
    }

    #[test]
    fn every_narration_schema_is_a_strict_object() {
        let outcome = TurnOutcome::default();
        let acknowledge = AcknowledgeInput {
            outcome: &outcome,
            locale: "en-GB",
            tone: ToneProfile::Neutral,
            message: None,
            on_screen: &[],
            answers: &[],
            unanswered: &[],
            notices: &[],
            transcript: &[],
            guidance: &[],
        };
        let task = Acknowledge {
            max_chars: None,
            input: PhantomData,
        };
        assert_strict("acknowledge", &task.schema(&acknowledge));
        let answer = AnswerInput {
            question: "?",
            locale: "en-GB",
            tone: ToneProfile::Neutral,
            asked_before: None,
            previous: None,
            facts: &[],
            guidance: &[],
            attachments: &[],
        };
        let task = Answer {
            max_chars: None,
            input: PhantomData,
        };
        assert_strict("answer", &task.schema(&answer));
        let on_screen = ["A card.".to_owned()];
        let review = ReviewInput {
            reply: "",
            material: Value::Null,
            has_ask: true,
            on_screen: &on_screen,
            carries: true,
        };
        assert_strict("review", &Review(PhantomData).schema(&review));
        let step = StepInput {
            locale: "en-GB",
            step: "Reading the message (4 words).",
        };
        assert_strict("step", &StepProse(PhantomData).schema(&step));
    }

    #[test]
    fn a_review_asks_only_the_checks_that_apply() {
        let review = ReviewInput {
            reply: "Done.",
            material: Value::Null,
            has_ask: false,
            on_screen: &[],
            carries: false,
        };
        let schema = Review(PhantomData).schema(&review);
        let asked: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
        assert_eq!(asked, ["reasoning", "claims_beyond_material"]);
    }
}
