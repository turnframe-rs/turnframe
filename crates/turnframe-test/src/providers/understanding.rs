//! What a turn was understood to say, written by a test: a builder that turns quotes into
//! word ranges, and a [`TurnUnderstander`] that returns the understandings it was given.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use turnframe_core::ids::{OperationKey, OptionId, TargetToken, WorkflowKey};
use turnframe_core::plan::AnswerBasis;
use turnframe_core::understanding::{
    ActAction, ActId, ActStatus, ActTarget, ArgumentValue, CardAnswer, ConstraintKind, Dispute,
    Excerpt, FoundBy, MessageRef, NotUnderstood, NotUnderstoodReason, QuestionTopic, RecordValue,
    Superseded, TurnConstraint, Understanding, UnderstoodAct, UnderstoodArgument,
    UnderstoodQuestion, Unit, UnitId, UnitKind, Unreadable, WordRange,
};
use turnframe_tasks::TaskScope;
use turnframe_understand::{ActChecker, StepSink, TurnUnderstander, UnderstandingInput, Words};

/// A quote that does not occur in the text the understanding is about.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("«{quote}» does not occur in «{text}»")]
pub struct QuoteNotFound {
    /// The quote.
    pub quote: String,
    /// The text.
    pub text: String,
}

/// Builds an [`Understanding`] of one message from quotes of it.
///
/// ```
/// use turnframe_test::providers::UnderstandingBuilder;
///
/// let understanding = UnderstandingBuilder::of("set the name to Lisbon")
///     .apply("trip.set_name", "tok_1", serde_json::json!({"value": "Lisbon"}), "set the name to Lisbon")
///     .build()
///     .unwrap();
/// assert_eq!(understanding.acts[0].arguments["value"].excerpt.unwrap().words.first, 4);
/// ```
#[derive(Debug, Clone)]
pub struct UnderstandingBuilder {
    words: Words,
    understanding: Understanding,
    error: Option<QuoteNotFound>,
}

impl UnderstandingBuilder {
    /// Starts an understanding of `text`.
    #[must_use]
    pub fn of(text: impl AsRef<str>) -> Self {
        Self {
            words: Words::split(text.as_ref()),
            understanding: Understanding::default(),
            error: None,
        }
    }

    /// The message, as given.
    #[must_use]
    pub fn text(&self) -> &str {
        self.words.text()
    }

    fn range(&mut self, quote: &str) -> Option<WordRange> {
        let found = self.locate(quote);
        if found.is_none() && self.error.is_none() {
            self.error = Some(QuoteNotFound {
                quote: quote.to_owned(),
                text: self.words.text().to_owned(),
            });
        }
        found
    }

    fn locate(&self, quote: &str) -> Option<WordRange> {
        let start = self.words.text().find(quote)?;
        let end = start + quote.len();
        let words = self.words.words();
        let first = words.iter().position(|w| w.end > start)?;
        let last = words.iter().rposition(|w| w.start < end)?;
        Some(WordRange {
            first,
            last,
            start: words[first].start,
            end: words[last].end,
        })
    }

    fn unit(&mut self, kind: UnitKind, words: WordRange, workflow: Option<WorkflowKey>) -> UnitId {
        let id = UnitId(u16::try_from(self.understanding.units.len() + 1).unwrap_or(u16::MAX));
        self.understanding.units.push(Unit {
            id,
            kind,
            words,
            workflow,
            found_by: FoundBy::Segment,
        });
        id
    }

    fn arguments(
        &self,
        arguments: &serde_json::Value,
        words: WordRange,
    ) -> BTreeMap<String, UnderstoodArgument> {
        let Some(object) = arguments.as_object() else {
            return BTreeMap::new();
        };
        object
            .iter()
            .map(|(name, value)| {
                let located = value.as_str().and_then(|text| self.locate(text));
                let argument = UnderstoodArgument {
                    value: ArgumentValue::Json(value.clone()),
                    excerpt: Some(Excerpt {
                        message: MessageRef::Current,
                        words: located.unwrap_or(words),
                    }),
                };
                (name.clone(), argument)
            })
            .collect()
    }

    fn act(
        mut self,
        action: ActAction,
        target: ActTarget,
        arguments: &serde_json::Value,
        quote: &str,
    ) -> Self {
        let Some(words) = self.range(quote) else {
            return self;
        };
        let workflow = match &action {
            ActAction::Start { workflow } => Some(workflow.clone()),
            ActAction::Apply { operation } => operation
                .as_str()
                .split_once('.')
                .map(|(workflow, _)| WorkflowKey::from(workflow)),
        };
        let unit = self.unit(UnitKind::Request, words, workflow);
        let mut depends_on = Vec::new();
        if let ActTarget::SameTurn { act } = &target {
            depends_on.push(*act);
        }
        let arguments = self.arguments(arguments, words);
        self.understanding.acts.push(UnderstoodAct {
            id: ActId::new(unit, 1),
            action,
            target,
            arguments,
            words,
            depends_on,
            status: ActStatus::Ready,
        });
        self
    }

    /// An act applying `operation` to the record `target` names, asked for in `quote`.
    #[must_use]
    pub fn apply(
        self,
        operation: impl Into<OperationKey>,
        target: impl Into<TargetToken>,
        arguments: serde_json::Value,
        quote: &str,
    ) -> Self {
        let target = ActTarget::Record {
            token: target.into(),
        };
        self.apply_to(operation, target, arguments, quote)
    }

    /// An act applying `operation` to any target.
    #[must_use]
    pub fn apply_to(
        self,
        operation: impl Into<OperationKey>,
        target: ActTarget,
        arguments: serde_json::Value,
        quote: &str,
    ) -> Self {
        let action = ActAction::Apply {
            operation: operation.into(),
        };
        self.act(action, target, &arguments, quote)
    }

    /// An act applying `operation` to a record of `workflow` the user named in
    /// `named` that is not in view, for the directory to look up.
    #[must_use]
    pub fn apply_to_unlisted(
        mut self,
        operation: impl Into<OperationKey>,
        workflow: impl Into<WorkflowKey>,
        named: &str,
        arguments: serde_json::Value,
        quote: &str,
    ) -> Self {
        let words = self.range(named);
        let target = ActTarget::NotListed {
            workflow: workflow.into(),
            words,
        };
        self.apply_to(operation, target, arguments, quote)
    }

    /// An act applying `operation` to a record of `workflow` it creates.
    #[must_use]
    pub fn open(
        self,
        operation: impl Into<OperationKey>,
        workflow: impl Into<WorkflowKey>,
        arguments: serde_json::Value,
        quote: &str,
    ) -> Self {
        let target = ActTarget::New {
            workflow: workflow.into(),
        };
        self.apply_to(operation, target, arguments, quote)
    }

    /// An act starting a new case of `workflow`.
    #[must_use]
    pub fn start(self, workflow: impl Into<WorkflowKey>, quote: &str) -> Self {
        let workflow = workflow.into();
        let target = ActTarget::New {
            workflow: workflow.clone(),
        };
        self.act(
            ActAction::Start { workflow },
            target,
            &serde_json::Value::Null,
            quote,
        )
    }

    /// Gives the last act an argument naming a record. A record an earlier act of the
    /// turn creates makes that act a prerequisite.
    #[must_use]
    pub fn with_record(mut self, name: &str, record: RecordValue) -> Self {
        if let Some(act) = self.understanding.acts.last_mut() {
            if let RecordValue::SameTurn { act: earlier } = &record
                && !act.depends_on.contains(earlier)
            {
                act.depends_on.push(*earlier);
            }
            act.arguments.insert(
                name.to_owned(),
                UnderstoodArgument {
                    value: ArgumentValue::Record(record),
                    excerpt: None,
                },
            );
        }
        self
    }

    /// The id of the act added last.
    #[must_use]
    pub fn last_act(&self) -> Option<ActId> {
        self.understanding.acts.last().map(|act| act.id)
    }

    /// Replaces the last act's status.
    #[must_use]
    pub fn with_status(mut self, status: ActStatus) -> Self {
        if let Some(act) = self.understanding.acts.last_mut() {
            act.status = status;
        }
        self
    }

    /// Marks the last act as asking for `arguments`.
    #[must_use]
    pub fn needing(self, arguments: &[&str]) -> Self {
        self.with_status(ActStatus::NeedsValue {
            arguments: arguments.iter().map(|name| (*name).to_owned()).collect(),
            reason: None,
        })
    }

    /// Marks the last act as superseded by a later unit, removing it from the acts.
    #[must_use]
    pub fn superseded_by_next(mut self) -> Self {
        if let Some(act) = self.understanding.acts.pop() {
            let by = UnitId(act.id.unit.0 + 1);
            self.understanding.superseded.push(Superseded {
                act: act.id,
                action: act.action,
                by,
            });
        }
        self
    }

    /// A question answered against the current committed state.
    #[must_use]
    pub fn ask(self, quote: &str) -> Self {
        self.ask_about(AnswerBasis::CurrentCommittedState, None, &[], quote)
    }

    /// A question with its basis, record and subjects; one on general domain knowledge is
    /// about the domain, the rest about a record, until [`Self::about`] says otherwise.
    #[must_use]
    pub fn ask_about(
        mut self,
        basis: AnswerBasis,
        record: Option<TargetToken>,
        subjects: &[&str],
        quote: &str,
    ) -> Self {
        let Some(words) = self.range(quote) else {
            return self;
        };
        let unit = self.unit(UnitKind::Question, words, None);
        self.understanding.questions.push(UnderstoodQuestion {
            unit,
            words,
            workflow: None,
            record,
            subjects: subjects.iter().map(|s| (*s).to_owned()).collect(),
            basis,
            topic: match basis {
                AnswerBasis::GeneralDomainKnowledge => QuestionTopic::Knowledge,
                _ => QuestionTopic::default(),
            },
            continues_previous: false,
        });
        self
    }

    /// Marks the last question as following up the assistant's last message.
    #[must_use]
    pub fn following_up(mut self) -> Self {
        if let Some(question) = self.understanding.questions.last_mut() {
            question.continues_previous = true;
        }
        self
    }

    /// Sets what kind of thing the last question asks.
    #[must_use]
    pub fn about(mut self, topic: QuestionTopic) -> Self {
        if let Some(question) = self.understanding.questions.last_mut() {
            question.topic = topic;
        }
        self
    }

    /// A turn-wide constraint stated in `quote`.
    #[must_use]
    pub fn constrain(mut self, kind: ConstraintKind, quote: &str) -> Self {
        let Some(words) = self.range(quote) else {
            return self;
        };
        let unit = self.unit(UnitKind::Constraint, words, None);
        self.understanding
            .constraints
            .push(TurnConstraint { unit, kind, words });
        self
    }

    /// A typed answer to the card on screen.
    #[must_use]
    pub fn answer_card(mut self, option: impl Into<OptionId>, quote: &str) -> Self {
        let Some(words) = self.range(quote) else {
            return self;
        };
        let unit = self.unit(UnitKind::CardAnswer, words, None);
        self.understanding.card_answer = Some(CardAnswer {
            unit,
            option: option.into(),
            words,
        });
        self
    }

    /// A dispute of the receipt shown under `receipt`, when it names one.
    #[must_use]
    pub fn dispute(mut self, receipt: Option<&str>, quote: &str) -> Self {
        let Some(words) = self.range(quote) else {
            return self;
        };
        let unit = self.unit(UnitKind::Dispute, words, None);
        self.understanding.disputes.push(Dispute {
            unit,
            words,
            receipt: receipt.map(str::to_owned),
        });
        self
    }

    /// Words that produced nothing to act on.
    #[must_use]
    pub fn not_understood(mut self, reason: NotUnderstoodReason, quote: &str) -> Self {
        let Some(words) = self.range(quote) else {
            return self;
        };
        let unit = self.unit(UnitKind::Request, words, None);
        self.understanding.not_understood.push(NotUnderstood {
            unit,
            words,
            reason,
        });
        self
    }

    /// Greetings, thanks, words that ask for nothing.
    #[must_use]
    pub fn chitchat(mut self, quote: &str) -> Self {
        if let Some(words) = self.range(quote) {
            self.unit(UnitKind::Chitchat, words, None);
        }
        self
    }

    /// Finishes the understanding.
    ///
    /// # Errors
    ///
    /// [`QuoteNotFound`] for the first quote that is not in the text.
    pub fn build(self) -> Result<Understanding, QuoteNotFound> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.understanding),
        }
    }
}

/// A [`TurnUnderstander`] that returns the understandings queued, in order, and keeps
/// what each turn showed it.
///
/// A turn with nothing queued is unreadable, with the code `unscripted`, so an extra
/// turn shows up as a turn that did nothing instead of an improvised reading.
#[derive(Debug, Default)]
pub struct ScriptedUnderstanding {
    queue: Mutex<VecDeque<Understanding>>,
    seen: Mutex<Vec<UnderstandingInput>>,
}

impl ScriptedUnderstanding {
    /// Nothing queued.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues `understanding` for the next turn.
    #[must_use]
    pub fn then(self, understanding: Understanding) -> Self {
        self.push(understanding);
        self
    }

    /// Queues `understanding` for the next turn, after construction.
    pub fn push(&self, understanding: Understanding) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(understanding);
    }

    /// What each turn showed understanding, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<UnderstandingInput> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Understandings queued and not yet used.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[async_trait]
impl TurnUnderstander for ScriptedUnderstanding {
    async fn understand(
        &self,
        _scope: &TaskScope,
        turn: &UnderstandingInput,
        _steps: &dyn StepSink,
        _checker: &dyn ActChecker,
    ) -> Understanding {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(turn.clone());
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Understanding::unreadable(Unreadable::Segmentation {
                    code: "unscripted".to_owned(),
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_become_word_ranges_and_values_point_at_their_words() {
        let understanding = UnderstandingBuilder::of("please set the name to Lisbon")
            .apply(
                "trip.set_name",
                "t_1",
                serde_json::json!({"value": "Lisbon"}),
                "set the name to Lisbon",
            )
            .build()
            .unwrap();
        let act = &understanding.acts[0];
        assert_eq!((act.words.first, act.words.last), (1, 5));
        let excerpt = act.arguments["value"].excerpt.unwrap();
        assert_eq!((excerpt.words.first, excerpt.words.last), (5, 5));
        assert_eq!(understanding.units[0].workflow, Some("trip".into()));
    }

    #[test]
    fn a_quote_that_is_not_in_the_text_fails_the_build() {
        let error = UnderstandingBuilder::of("hello")
            .ask("what is the total?")
            .build()
            .unwrap_err();
        assert_eq!(error.quote, "what is the total?");
    }
}
