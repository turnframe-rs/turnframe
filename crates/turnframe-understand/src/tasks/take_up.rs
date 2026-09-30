//! `take_up`: which offer of the last reply a part of the message takes up, if any.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turnframe_provider::request::Message;
use turnframe_tasks::{ModelTask, StructuralError, TaskKind};

use crate::input::UnderstandingInput;
use crate::render;
use crate::schema::{object, one_of};
use crate::tasks::check_one_of;
use crate::words::Span;

/// The answer for a part that takes up no offer.
pub const NONE: &str = "none";

/// The answer for a part that says no to the offers and asks for nothing else.
pub const DECLINES: &str = "declines";

const BUILT_IN: &str = include_str!("../../prompts/understand/take_up.md");

/// The take-up task, over one turn.
#[derive(Debug, Clone, Copy)]
pub struct TakeUp<'a> {
    turn: &'a UnderstandingInput,
}

impl<'a> TakeUp<'a> {
    /// The task for `turn`.
    #[must_use]
    pub const fn new(turn: &'a UnderstandingInput) -> Self {
        Self { turn }
    }
}

/// One part of the message, read against the offers.
#[derive(Debug, Clone, Copy)]
pub struct TakeUpInput {
    /// How the part is shown.
    pub label: &'static str,
    /// Its words.
    pub words: Span,
}

/// The offer taken up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Taking {
    /// An offer's handle, `o1`, or `none`.
    pub offer: String,
}

/// The handles of the turn's offers, `o1` first, then `declines` and `none`.
fn choices(turn: &UnderstandingInput) -> Vec<String> {
    (1..=turn.offers.len())
        .map(|number| format!("o{number}"))
        .chain([DECLINES.to_owned(), NONE.to_owned()])
        .collect()
}

/// The position of the offer a handle names, when it names one.
#[must_use]
pub fn offer_at(turn: &UnderstandingInput, handle: &str) -> Option<usize> {
    let number: usize = handle.strip_prefix('o')?.parse().ok()?;
    (1..=turn.offers.len())
        .contains(&number)
        .then(|| number - 1)
}

impl ModelTask for TakeUp<'_> {
    type Input = TakeUpInput;
    type Output = Taking;

    fn kind(&self) -> TaskKind {
        TaskKind::TakeUp
    }

    fn prompt_name(&self) -> &str {
        "understand.take_up"
    }

    fn instructions(&self) -> &str {
        BUILT_IN
    }

    fn schema(&self, _input: &TakeUpInput) -> Value {
        object(vec![("offer", one_of(choices(self.turn)))])
    }

    fn render(&self, input: &TakeUpInput) -> Vec<Message> {
        let turn = self.turn;
        let mut offers = String::from("Offers the assistant made:");
        for (number, offer) in (1..).zip(&turn.offers) {
            let record = offer
                .act
                .record
                .as_ref()
                .and_then(|token| turn.record(token))
                .map_or_else(String::new, |(_, record)| format!(" on {}", record.label));
            let _ = write!(
                offers,
                "\n- o{number}: «{}» ({}{record})",
                offer.words, offer.act.operation
            );
        }
        let _ = write!(
            offers,
            "\n- {DECLINES}: the part says no to them, or that nothing more is wanted\n- {NONE}: \
             the part takes up none of them"
        );
        vec![Message::user(render::sections([
            Some(offers),
            render::last_assistant(turn),
            Some(render::message(&turn.message)),
            Some(render::unit(input.label, &turn.message, input.words)),
        ]))]
    }

    fn check(&self, _input: &TakeUpInput, output: &Taking) -> Result<(), StructuralError> {
        check_one_of("offer", &output.offer, &choices(self.turn))
    }
}
