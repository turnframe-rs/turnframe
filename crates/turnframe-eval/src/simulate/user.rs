//! The person a conversation is held with: what they see, and what they do next.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::request::{Message, ModelRequest, OutputSpec};
use turnframe_provider::structured::{SchemaCache, parse_structured};

/// What the person does next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum UserMove {
    /// Types a message.
    Say {
        /// The message.
        text: String,
    },
    /// Presses an option of a card on screen, by the option's id.
    Press {
        /// The option.
        option: String,
    },
    /// Stops: the goal is reached as far as they can tell, or they give up.
    Done,
}

impl UserMove {
    /// A message.
    #[must_use]
    pub fn say(text: impl Into<String>) -> Self {
        Self::Say { text: text.into() }
    }

    /// A press of `option`.
    #[must_use]
    pub fn press(option: impl Into<String>) -> Self {
        Self::Press {
            option: option.into(),
        }
    }
}

impl std::fmt::Display for UserMove {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Say { text } => f.write_str(text),
            Self::Press { option } => write!(f, "[presses {option}]"),
            Self::Done => f.write_str("[done]"),
        }
    }
}

/// A card on screen, with the options the person may press.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardOnScreen {
    /// Its title.
    pub title: String,
    /// Its options, as `(id, label)`.
    pub options: Vec<(String, String)>,
}

/// What the person has in front of them when they decide.
#[derive(Debug, Clone, Copy)]
pub struct Screen<'a> {
    /// What they want.
    pub want: &'a str,
    /// How they talk.
    pub manner: &'a str,
    /// The conversation so far: what they did, and the reply they read.
    pub exchanges: &'a [(UserMove, String)],
    /// The cards open on screen.
    pub cards: &'a [CardOnScreen],
    /// The next steps the last reply offered, which a surface shows beside it.
    pub offers: &'a [String],
    /// How many turns they will still take.
    pub turns_left: u32,
}

/// Someone who holds a conversation.
#[async_trait]
pub trait SimulatedUser: Send + Sync {
    /// The next move, or why none could be had.
    ///
    /// # Errors
    ///
    /// A message when the person could not decide: a model that failed or answered badly.
    async fn next(&self, screen: &Screen<'_>) -> Result<UserMove, String>;
}

/// A person whose moves are written in advance, for tests: done once they run out.
#[derive(Debug, Default)]
pub struct ScriptedUser {
    moves: Mutex<VecDeque<UserMove>>,
}

impl ScriptedUser {
    /// A person who makes `moves`, in order.
    #[must_use]
    pub fn new(moves: impl IntoIterator<Item = UserMove>) -> Self {
        Self {
            moves: Mutex::new(moves.into_iter().collect()),
        }
    }
}

#[async_trait]
impl SimulatedUser for ScriptedUser {
    async fn next(&self, _screen: &Screen<'_>) -> Result<UserMove, String> {
        let mut moves = self
            .moves
            .lock()
            .map_err(|_| "the script was poisoned".to_owned())?;
        Ok(moves.pop_front().unwrap_or(UserMove::Done))
    }
}

const SYSTEM: &str = "You play a person using a text assistant, to test it. You have a goal \
and a manner of talking. Each time, read the conversation so far and what is on screen, and \
give your next move:\n\
- say: the next message you type, in your manner, as that person would type it. Never say \
you are testing or playing.\n\
- press: an option of a card on screen, by its id, when a card asks you to choose. Only a \
card's options can be pressed.\n\
- done: when the assistant has told you your goal is reached, or when you would give up.\n\
Keep to your goal: ask for nothing it does not name. When the assistant asks something your \
goal answers, answer it; when it asks something your goal does not say, answer briefly as a \
reasonable person would. When it misunderstands you, say so as that person would.";

/// What the simulator answers, before it is a move.
#[derive(Debug, Deserialize)]
struct Decided {
    #[serde(rename = "move")]
    kind: String,
    text: String,
    option: String,
}

/// A person played by a model.
pub struct ModelUser {
    provider: Arc<dyn ModelProvider>,
    temperature: Option<f32>,
    schemas: SchemaCache,
}

impl std::fmt::Debug for ModelUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelUser")
            .field("provider", &self.provider.provider_key())
            .field("model", &self.provider.model_key())
            .finish_non_exhaustive()
    }
}

impl ModelUser {
    /// A person played by `provider`, which should not be the one under test.
    #[must_use]
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        Self {
            provider,
            temperature: None,
            schemas: SchemaCache::new(),
        }
    }

    /// Plays at `temperature`, for a model that takes one.
    #[must_use]
    pub const fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "thinking": {"type": "string"},
                "move": {"type": "string", "enum": ["say", "press", "done"]},
                "text": {"type": "string"},
                "option": {"type": "string"}
            },
            "required": ["thinking", "move", "text", "option"],
            "additionalProperties": false
        })
    }

    fn prompt(screen: &Screen<'_>) -> String {
        let mut out = format!(
            "Your goal: {}\nYour manner: {}\n\nConversation so far:",
            screen.want, screen.manner
        );
        if screen.exchanges.is_empty() {
            out.push_str("\n(nothing yet: you speak first)");
        }
        for (said, reply) in screen.exchanges {
            let _ = write!(out, "\n- you: {said}\n- assistant: {reply}");
        }
        if !screen.cards.is_empty() {
            out.push_str("\n\nCards on screen:");
            for card in screen.cards {
                let options: Vec<String> = card
                    .options
                    .iter()
                    .map(|(id, label)| format!("{id} «{label}»"))
                    .collect();
                let _ = write!(out, "\n- «{}»: {}", card.title, options.join(", "));
            }
        }
        if !screen.offers.is_empty() {
            let _ = write!(
                out,
                "\n\nSuggested next steps on screen (type what you want; they are not buttons):\n- {}",
                screen.offers.join("\n- ")
            );
        }
        let _ = write!(
            out,
            "\n\nTurns you will still take: {}.\n\nAnswer with your thinking in a sentence, \
             the move, the text for say (empty otherwise) and the option id for press (empty \
             otherwise).",
            screen.turns_left
        );
        out
    }
}

#[async_trait]
impl SimulatedUser for ModelUser {
    async fn next(&self, screen: &Screen<'_>) -> Result<UserMove, String> {
        let schema = Self::schema();
        let compiled = self
            .schemas
            .compile(&schema)
            .map_err(|error| error.to_string())?;
        let mut request = ModelRequest::new(ModelPurpose::OfflineEvaluate)
            .with_system(SYSTEM.to_owned())
            .with_message(Message::user(Self::prompt(screen)));
        request.output = OutputSpec::json("turnframe_simulated_user", schema);
        request.temperature = self.temperature;
        let response = self
            .provider
            .generate(request)
            .await
            .map_err(|error| error.to_string())?;
        let decided: Decided =
            parse_structured(&response, &compiled).map_err(|error| error.to_string())?;
        match decided.kind.as_str() {
            "say" if !decided.text.trim().is_empty() => Ok(UserMove::say(decided.text.trim())),
            "press" if !decided.option.trim().is_empty() => {
                Ok(UserMove::press(decided.option.trim()))
            }
            "done" => Ok(UserMove::Done),
            other => Err(format!("the simulator answered {other} with nothing to do")),
        }
    }
}
