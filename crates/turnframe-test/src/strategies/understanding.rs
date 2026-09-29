//! Strategies for understandings grounded in a text: every word range is words of it.

use proptest::prelude::*;
use turnframe_core::ids::{OperationKey, TargetToken};
use turnframe_core::plan::AnswerBasis;
use turnframe_core::understanding::{
    ActAction, ActId, ActStatus, ActTarget, ConstraintKind, QuestionTopic, TurnConstraint,
    Understanding, UnderstoodAct, UnderstoodQuestion, UnitId, WordRange,
};
use turnframe_understand::{Span, Words};

use crate::strategies::ids::label;

/// A range of the words of `text`, or `None` for a text with no words.
#[must_use]
pub fn word_range(text: &str) -> Option<BoxedStrategy<WordRange>> {
    let words = Words::split(text);
    if words.is_empty() {
        return None;
    }
    let count = words.len();
    Some(
        (0..count, 0..count)
            .prop_map(move |(a, b)| {
                let span = Span::new(a.min(b), a.max(b));
                words
                    .range(span)
                    .unwrap_or_else(|_| unreachable!("indices are in range"))
            })
            .boxed(),
    )
}

/// An understanding of `text` with acts, questions and constraints on its words.
pub fn grounded_understanding(text: &str) -> BoxedStrategy<Understanding> {
    let Some(range) = word_range(text) else {
        return Just(Understanding::default()).boxed();
    };
    let act = (range.clone(), label(), label()).prop_map(|(words, operation, token)| {
        (
            words,
            OperationKey::from(operation),
            TargetToken::from(token),
        )
    });
    (
        proptest::collection::vec(act, 0..3),
        proptest::collection::vec(range.clone(), 0..2),
        proptest::collection::vec(range, 0..2),
    )
        .prop_map(|(acts, questions, constraints)| {
            let mut unit = 0u16;
            let mut next = || {
                unit += 1;
                UnitId(unit)
            };
            Understanding {
                acts: acts
                    .into_iter()
                    .map(|(words, operation, token)| UnderstoodAct {
                        id: ActId::new(next(), 1),
                        action: ActAction::Apply { operation },
                        target: ActTarget::Record { token },
                        arguments: std::collections::BTreeMap::new(),
                        words,
                        depends_on: Vec::new(),
                        status: ActStatus::Ready,
                    })
                    .collect(),
                questions: questions
                    .into_iter()
                    .map(|words| UnderstoodQuestion {
                        unit: next(),
                        words,
                        workflow: None,
                        record: None,
                        subjects: Vec::new(),
                        basis: AnswerBasis::CurrentCommittedState,
                        topic: QuestionTopic::default(),
                        continues_previous: false,
                    })
                    .collect(),
                constraints: constraints
                    .into_iter()
                    .map(|words| TurnConstraint {
                        unit: next(),
                        kind: ConstraintKind::DoNotSubmit,
                        words,
                    })
                    .collect(),
                ..Understanding::default()
            }
        })
        .boxed()
}
