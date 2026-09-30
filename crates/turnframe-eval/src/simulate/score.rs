//! A conversation scored by code, and a run reported as rates over its conversations.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::user::UserMove;

/// One turn of a conversation, as code reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchange {
    /// What the person did.
    pub said: Option<UserMove>,
    /// What they read back.
    pub reply: String,
    /// The error code of a turn that failed outright.
    pub failed: Option<String>,
    /// Whether the reply ended on a question, an ask, a card or an offer.
    pub way_forward: bool,
    /// What the reply asked, each as its record and what it waits on.
    pub asks: Vec<String>,
    /// The operations the reply offered.
    pub offers: Vec<String>,
    /// The operations the domain refused this turn.
    pub refused: Vec<String>,
    /// How many parts of the message were not understood.
    pub not_understood: usize,
}

/// The classes a conversation is scored on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationScore {
    /// Whether the goal's state was reached.
    pub reached: bool,
    /// Turns taken.
    pub turns: usize,
    /// Replies that ended on no question, no ask, no card and no offer.
    pub dead_ends: usize,
    /// Replies that asked what the reply before them asked.
    pub loops: usize,
    /// Parts of messages not understood.
    pub not_understood: usize,
    /// Acts the domain refused.
    pub refused: usize,
    /// Refused acts of an operation the reply before had offered.
    pub offers_refused: usize,
    /// Turns that failed outright.
    pub failed_turns: usize,
}

impl ConversationScore {
    /// Broken guarantees: a reply with no way forward, an offer the domain refused. Zero.
    #[must_use]
    pub const fn violations(&self) -> usize {
        self.dead_ends + self.offers_refused
    }
}

/// Scores `exchanges`, the goal `reached` or not.
#[must_use]
pub fn score(exchanges: &[Exchange], reached: bool) -> ConversationScore {
    let answered = |exchange: &&Exchange| exchange.failed.is_none();
    ConversationScore {
        reached,
        turns: exchanges.len(),
        dead_ends: exchanges
            .iter()
            .filter(answered)
            .filter(|exchange| !exchange.way_forward)
            .count(),
        loops: exchanges
            .windows(2)
            .filter(|pair| pair[1].failed.is_none() && !pair[1].asks.is_empty())
            .filter(|pair| pair[0].asks == pair[1].asks)
            .count(),
        not_understood: exchanges
            .iter()
            .map(|exchange| exchange.not_understood)
            .sum(),
        refused: exchanges
            .iter()
            .map(|exchange| exchange.refused.len())
            .sum(),
        offers_refused: exchanges
            .windows(2)
            .map(|pair| {
                pair[1]
                    .refused
                    .iter()
                    .filter(|operation| pair[0].offers.contains(operation))
                    .count()
            })
            .sum(),
        failed_turns: exchanges
            .iter()
            .filter(|exchange| !answered(exchange))
            .count(),
    }
}

/// One conversation: the goal, the manner, what was said and how it scored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    /// The goal's id.
    pub goal: String,
    /// The manner played.
    pub manner: String,
    /// Which sample of the pair it is.
    pub sample: u32,
    /// The turns, in order.
    pub exchanges: Vec<Exchange>,
    /// Why the conversation stopped.
    pub ended: Ending,
    /// Its score.
    pub score: ConversationScore,
}

impl Conversation {
    /// The conversation as a person would read it back.
    #[must_use]
    pub fn transcript(&self) -> String {
        let mut out = format!(
            "{} ({}, sample {}): {}",
            self.goal,
            self.manner,
            self.sample,
            if self.score.reached {
                "reached"
            } else {
                "not reached"
            }
        );
        for exchange in &self.exchanges {
            if let Some(said) = &exchange.said {
                let _ = write!(out, "\n  user: {said}");
            }
            match &exchange.failed {
                Some(code) => {
                    let _ = write!(out, "\n  (the turn failed: {code})");
                }
                None => {
                    let _ = write!(out, "\n  assistant: {}", exchange.reply.replace('\n', " "));
                }
            }
        }
        let _ = write!(out, "\n  ({})", self.ended);
        out
    }

    /// How badly it went, worst first when sorted descending.
    fn badness(&self) -> (usize, bool, usize) {
        (
            self.score.violations(),
            !self.score.reached,
            self.score.turns,
        )
    }
}

/// Why a conversation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ending {
    /// The person said they were done.
    Done,
    /// The turn limit came first.
    TurnLimit,
    /// The person could not decide: the simulator failed.
    UserFailed {
        /// Why.
        message: String,
    },
    /// The world could not be prepared.
    Unprepared {
        /// Why.
        message: String,
    },
}

impl std::fmt::Display for Ending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Done => f.write_str("the user was done"),
            Self::TurnLimit => f.write_str("the turn limit came first"),
            Self::UserFailed { message } => write!(f, "the simulated user failed: {message}"),
            Self::Unprepared { message } => write!(f, "the world was not prepared: {message}"),
        }
    }
}

/// How one class is read off a score.
type Class = fn(&ConversationScore) -> usize;

/// A run: every conversation, reported as rates.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimulationReport {
    /// The conversations, in goal, manner and sample order.
    pub conversations: Vec<Conversation>,
}

impl SimulationReport {
    /// The conversations that ran: an unprepared world measured nothing.
    fn measured(&self) -> impl Iterator<Item = &Conversation> {
        self.conversations
            .iter()
            .filter(|conversation| !matches!(conversation.ended, Ending::Unprepared { .. }))
    }

    /// How many conversations ran.
    #[must_use]
    pub fn measured_count(&self) -> usize {
        self.measured().count()
    }

    /// Broken guarantees over the whole run: zero, or a defect.
    #[must_use]
    pub fn violations(&self) -> usize {
        self.measured()
            .map(|conversation| conversation.score.violations())
            .sum()
    }

    /// The `count` worst conversations: guarantees broken, then goals missed, then length.
    #[must_use]
    pub fn worst(&self, count: usize) -> Vec<&Conversation> {
        let mut all: Vec<&Conversation> = self.measured().collect();
        all.sort_by_key(|conversation| std::cmp::Reverse(conversation.badness()));
        all.truncate(count);
        all
    }

    /// Each class as a rate over the conversations, then the worst transcripts.
    #[must_use]
    pub fn summary(&self) -> String {
        let measured: Vec<&Conversation> = self.measured().collect();
        let total = measured.len();
        let sum = |class: Class| -> usize {
            measured
                .iter()
                .map(|conversation| class(&conversation.score))
                .sum()
        };
        let per = |count: usize| {
            if total == 0 {
                0.0
            } else {
                count as f64 / total as f64
            }
        };
        let reached = measured.iter().filter(|c| c.score.reached).count();
        let mut out = format!(
            "{total} conversation(s), {} unprepared\nreached: {reached}/{total} ({:.0}%)",
            self.conversations.len() - total,
            per(reached) * 100.0
        );
        let classes: [(&str, Class); 7] = [
            ("turns", |s| s.turns),
            ("dead ends", |s| s.dead_ends),
            ("loops", |s| s.loops),
            ("not understood", |s| s.not_understood),
            ("refused", |s| s.refused),
            ("offers refused", |s| s.offers_refused),
            ("failed turns", |s| s.failed_turns),
        ];
        for (name, class) in classes {
            let count = sum(class);
            let _ = write!(
                out,
                "\n{name}: {count} ({:.2} per conversation)",
                per(count)
            );
        }
        let _ = write!(out, "\nguarantee violations: {}", self.violations());
        for conversation in self.worst(3) {
            let _ = write!(out, "\n\n{}", conversation.transcript());
        }
        out
    }

    /// The report as JSON.
    ///
    /// # Errors
    ///
    /// A serialization error, which a report of plain data does not produce.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(way_forward: bool) -> Exchange {
        Exchange {
            said: Some(UserMove::say("hello")),
            reply: "Hi.".to_owned(),
            way_forward,
            ..Exchange::default()
        }
    }

    #[test]
    fn a_reply_with_no_way_forward_is_a_dead_end_and_a_violation() {
        let score = score(&[exchange(true), exchange(false)], true);
        assert_eq!(score.dead_ends, 1);
        assert_eq!(score.violations(), 1);
    }

    #[test]
    fn the_same_ask_twice_in_a_row_is_a_loop() {
        let asking = |what: &str| Exchange {
            asks: vec![what.to_owned()],
            ..exchange(true)
        };
        let score = score(
            &[
                asking("trip/1: name"),
                asking("trip/1: name"),
                asking("trip/1: date"),
            ],
            false,
        );
        assert_eq!(score.loops, 1);
        assert_eq!(
            score.violations(),
            0,
            "a loop is measured, not a broken guarantee"
        );
    }

    #[test]
    fn an_offer_refused_on_the_next_turn_is_a_violation() {
        let offering = Exchange {
            offers: vec!["sample.send".to_owned()],
            ..exchange(true)
        };
        let refusing = Exchange {
            refused: vec!["sample.send".to_owned(), "sample.other".to_owned()],
            ..exchange(true)
        };
        let score = score(&[offering, refusing], false);
        assert_eq!((score.refused, score.offers_refused), (2, 1));
        assert_eq!(score.violations(), 1);
    }

    #[test]
    fn a_failed_turn_is_counted_apart_from_dead_ends() {
        let failed = Exchange {
            failed: Some("internal".to_owned()),
            ..exchange(false)
        };
        let score = score(&[failed], false);
        assert_eq!((score.failed_turns, score.dead_ends), (1, 0));
    }

    #[test]
    fn the_worst_conversations_come_first() {
        let conversation = |goal: &str, reached: bool, dead_ends: usize| Conversation {
            goal: goal.to_owned(),
            manner: "plain".to_owned(),
            sample: 1,
            exchanges: Vec::new(),
            ended: Ending::Done,
            score: ConversationScore {
                reached,
                dead_ends,
                ..ConversationScore::default()
            },
        };
        let report = SimulationReport {
            conversations: vec![
                conversation("fine", true, 0),
                conversation("missed", false, 0),
                conversation("broken", true, 2),
            ],
        };
        let worst: Vec<&str> = report.worst(3).iter().map(|c| c.goal.as_str()).collect();
        assert_eq!(worst, ["broken", "missed", "fine"]);
        assert_eq!(report.violations(), 2);
    }
}
