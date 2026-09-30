//! What the reply says in words (spec §10): an acknowledgement written from the turn's
//! outcome and asking the one thing code chose, and one answer per question. Each is a
//! small task; a reviewed block that fails twice is dropped, and the question code wrote
//! stands in for a dropped acknowledgement.
//!
//! | Module | What it holds |
//! | --- | --- |
//! | [`outcome`] | the outcome and its ask, gathered by code |
//! | [`tasks`] | the acknowledge, answer and review tasks |

pub(crate) mod outcome;
pub(crate) mod tasks;

use std::marker::PhantomData;

use turnframe_tasks::{TaskCall, TaskEngine, TaskId, TaskKind, TaskOutcome, TaskScope};

use self::outcome::TurnOutcome;
use self::tasks::{
    Acknowledge, AcknowledgeInput, Answer, AnswerInput, Answered, Review, ReviewInput, StepInput,
    StepProse, Verdict, Written,
};

/// Runs the narration tasks of one turn under its scope.
pub(crate) struct Narrator<'a> {
    pub engine: &'a TaskEngine,
    pub scope: &'a TaskScope,
    pub max_chars: Option<usize>,
}

impl Narrator<'_> {
    /// One understanding step as a progress line; `None` when no model wrote one.
    pub async fn step(&self, locale: &str, step: &str) -> Option<String> {
        let id = TaskId::new("reply/step");
        let call = TaskCall {
            id: &id,
            parent: None,
            depth: 1,
        };
        let input = StepInput { locale, step };
        match self
            .engine
            .run(self.scope, call, &StepProse(PhantomData), &input)
            .await
        {
            TaskOutcome::Accepted { output, .. } => Some(output.text.trim().to_owned()),
            _ => None,
        }
    }
}

impl Narrator<'_> {
    /// The acknowledgement, reviewed when its profile asks. `None` when no model wrote
    /// one that passed.
    pub async fn acknowledge(&self, input: &AcknowledgeInput<'_>) -> Option<String> {
        let task = Acknowledge {
            max_chars: self.max_chars,
            input: PhantomData,
        };
        let id = TaskId::new("reply/acknowledge");
        let call = TaskCall {
            id: &id,
            parent: None,
            depth: 1,
        };
        let TaskOutcome::Accepted { output, .. } =
            self.engine.run(self.scope, call, &task, input).await
        else {
            return None;
        };
        if !self
            .engine
            .profile(self.scope, TaskKind::Acknowledge)
            .review
        {
            return Some(output.text.trim().to_owned());
        }
        let verdict = self.review(&output, input, "reply/review").await?;
        let issues = verdict.issues();
        if issues.is_empty() {
            return Some(output.text.trim().to_owned());
        }
        let id = TaskId::new("reply/acknowledge.after_review");
        let call = TaskCall {
            id: &id,
            parent: None,
            depth: 3,
        };
        let feedback = format!("{} ({})", verdict.reasoning, issues.join(", "));
        let TaskOutcome::Accepted { output, .. } = self
            .engine
            .run_with_feedback(self.scope, call, &task, input, &output, &feedback)
            .await
        else {
            return None;
        };
        let verdict = self
            .review(&output, input, "reply/review.after_rewrite")
            .await?;
        verdict
            .issues()
            .is_empty()
            .then(|| output.text.trim().to_owned())
    }

    async fn review(
        &self,
        written: &Written,
        input: &AcknowledgeInput<'_>,
        id: &str,
    ) -> Option<Verdict> {
        let id = TaskId::new(id);
        let call = TaskCall {
            id: &id,
            parent: None,
            depth: 2,
        };
        let mut material = serde_json::to_value(input.outcome).unwrap_or_default();
        if let serde_json::Value::Object(map) = &mut material {
            if !input.answers.is_empty() {
                map.insert("answers".to_owned(), input.answers.into());
            }
            if !input.notices.is_empty() {
                map.insert("notices".to_owned(), input.notices.into());
            }
        }
        let review = ReviewInput {
            reply: &written.text,
            material,
            has_ask: input.outcome.ask.is_some(),
            elsewhere: input.outcome.ask.as_ref().is_some_and(|ask| ask.elsewhere),
            has_next: !input.outcome.next.is_empty(),
            closing: input.outcome.closing.is_some(),
            on_screen: input.on_screen,
            carries: !input.answers.is_empty() || !input.notices.is_empty(),
        };
        match self
            .engine
            .run(self.scope, call, &Review(PhantomData), &review)
            .await
        {
            TaskOutcome::Accepted { output, .. } => Some(output),
            _ => None,
        }
    }

    /// One answer, or why there is none; the failure's code when no model answered.
    pub async fn answer(
        &self,
        question: &str,
        input: &AnswerInput<'_>,
    ) -> Result<Answered, String> {
        let task = Answer {
            max_chars: self.max_chars,
            input: PhantomData,
        };
        let id = TaskId::new(format!("{question}/answer"));
        let call = TaskCall {
            id: &id,
            parent: None,
            depth: 1,
        };
        match self.engine.run(self.scope, call, &task, input).await {
            TaskOutcome::Accepted { output, .. } => Ok(Answered {
                text: output.text.trim().to_owned(),
                ..output
            }),
            TaskOutcome::Failed { failure, .. } => Err(failure.code()),
            _ => Err("disagreement".to_owned()),
        }
    }
}

/// Whether an outcome gives the acknowledgement anything to say.
pub(crate) fn speaks(outcome: &TurnOutcome) -> bool {
    !outcome.is_silent()
}
