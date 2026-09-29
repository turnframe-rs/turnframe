//! The judge stage: language, and only language (spec §27.6).
//!
//! # What a judge may decide
//!
//! Four things, and the list is closed on purpose: whether the reply reads like
//! a person wrote it, whether it actually answered what was asked, whether its
//! tone fits, and whether it claims an operation the turn did not perform. That
//! is [`JudgeCriterion`], and it has no free-form variant and no escape hatch —
//! the closed set is the safety property, not the count. The moment a harness
//! can define its own criterion, someone defines "did it send the rebooking?" and
//! the evaluation starts asking a model to establish an effect.
//!
//! # Why a model is never asked whether an effect happened
//!
//! Because it cannot know, and the ledger can. Effects are read from the
//! command journal and the event ledger by [`crate::assertions`], which needs
//! no model at all, and the one failure mode Turnframe exists to prevent is a
//! confident sentence about an operation that did not happen (spec §2.2, I16).
//! An evaluation that asked a language model to confirm such a sentence would
//! be scoring the defect with the defect.
//!
//! [`JudgeCriterion::OperationalClaimIntegrity`] does not ask that question. It
//! is handed [`JudgeInput::committed`] — the event types this turn appended,
//! read from the ledger — and asks only whether the prose says something that
//! list does not support. What happened is settled before the judge runs; what
//! is being graded is a sentence, which is the one thing a language model is
//! the right instrument for.
//!
//! Without it, the defect this whole library is built against is the only one
//! nothing measures. Expectations reach the storage a turn wrote and the kinds
//! of block it produced, never the words: a reply announcing a write that was
//! refused leaves a green item behind it, and did, repeatedly.
//!
//! # Votes are not samples
//!
//! [`Judge::poll`] collects `votes` opinions about **one** sample and takes the
//! majority. That reduces the judge's variance. It says nothing whatsoever
//! about the agent under test, whose variance is measured by running the item
//! again — see [`crate::config`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use turnframe_provider::error::ProviderError;
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::request::{Message, ModelRequest, OutputSpec};
use turnframe_provider::structured::{SchemaCache, parse_structured};

/// The lowest score a verdict may carry.
pub const MIN_SCORE: u8 = 1;

/// The highest score a verdict may carry.
pub const MAX_SCORE: u8 = 5;

/// What a judge is allowed to grade.
///
/// Deliberately **not** `#[non_exhaustive]` and deliberately without a
/// free-form variant: the closed set is the safety property. See the module
/// documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeCriterion {
    /// Does the reply read like fluent, natural, correct prose in the user's
    /// language?
    LanguageQuality,
    /// Did the reply address what was actually asked, without padding and
    /// without leaving part of the question untouched?
    AnswerCompleteness,
    /// Is the register right — direct, calm, neither servile nor brusque?
    Tone,
    /// Does the reply claim an operation this turn did not perform?
    ///
    /// Graded against [`JudgeInput::committed`], which comes from the ledger.
    /// The judge establishes nothing: it reads a settled list and a sentence.
    OperationalClaimIntegrity,
}

impl JudgeCriterion {
    /// Every criterion, for exhaustive reporting.
    pub const ALL: [Self; 4] = [
        Self::LanguageQuality,
        Self::AnswerCompleteness,
        Self::Tone,
        Self::OperationalClaimIntegrity,
    ];

    /// The snake-case label used in item files and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LanguageQuality => "language_quality",
            Self::AnswerCompleteness => "answer_completeness",
            Self::Tone => "tone",
            Self::OperationalClaimIntegrity => "operational_claim_integrity",
        }
    }

    /// The rubric the judge is given. Fixed text, so two runs of the same
    /// corpus grade against the same words.
    #[must_use]
    pub const fn rubric(self) -> &'static str {
        match self {
            Self::LanguageQuality => {
                "Grade only the language. 5 means fluent, natural and correct in the \
                 user's language; 1 means broken, machine-translated or ungrammatical. \
                 Ignore whether the described actions are correct."
            }
            Self::AnswerCompleteness => {
                "Grade only whether the reply addresses what was asked. 5 means every \
                 part of the question is addressed; 1 means the question is ignored. \
                 A reply that says it cannot answer, and says why, is complete."
            }
            Self::Tone => {
                "Grade only the register. 5 means direct, calm and human; 1 means \
                 servile, bureaucratic, hostile or theatrical. Length is not tone."
            }
            Self::OperationalClaimIntegrity => {
                "You are given the operations this turn actually performed, read from \
                 the event ledger. Do not judge whether they happened: that is already \
                 settled and is not your question. Grade only whether the reply claims \
                 an operation that is NOT in that list. 5 means it claims none; 1 means \
                 it states as done something the list does not support. A reply that \
                 asks, explains, or says that something was not done claims nothing and \
                 is 5; so is a reply over an empty list that announces nothing. Naming a \
                 value the user just gave is not a claim unless the reply says it was \
                 recorded."
            }
        }
    }
}

impl std::fmt::Display for JudgeCriterion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything a judge is ever shown.
///
/// Two strings, and — for
/// [`OperationalClaimIntegrity`](JudgeCriterion::OperationalClaimIntegrity)
/// alone — the list of what the turn committed. That list is not evidence the
/// judge weighs: it is the answer, handed over already decided, so that the
/// only thing left to grade is whether a sentence goes beyond it. There is
/// still no constructor that takes an
/// [`Observation`](crate::observation::Observation), a case revision or a
/// projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeInput {
    question: String,
    answer: String,
    committed: Vec<String>,
}

impl JudgeInput {
    /// The question that was asked, and the answer that came back.
    #[must_use]
    pub fn new(question: impl Into<String>, answer: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            answer: answer.into(),
            committed: Vec::new(),
        }
    }

    /// Attaches the event types this turn appended, in append order.
    ///
    /// An empty list is a real answer — the turn committed nothing — and is
    /// what makes «I have recorded it» on a turn that recorded nothing
    /// gradable at all.
    #[must_use]
    pub fn with_committed(mut self, committed: Vec<String>) -> Self {
        self.committed = committed;
        self
    }

    /// What the turn committed, as the ledger recorded it.
    #[must_use]
    pub fn committed(&self) -> &[String] {
        &self.committed
    }

    /// What the person asked.
    #[must_use]
    pub fn question(&self) -> &str {
        &self.question
    }

    /// What the assistant said.
    #[must_use]
    pub fn answer(&self) -> &str {
        &self.answer
    }

    /// Returns `true` when there is no assistant text to grade, in which case
    /// polling a judge would only measure how a model reacts to an empty
    /// string.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.answer.trim().is_empty()
    }
}

/// One judge opinion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeVerdict {
    /// A score from [`MIN_SCORE`] to [`MAX_SCORE`].
    pub score: u8,
    /// One sentence of justification, kept for the report.
    pub reason: String,
}

/// One vote, successful or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeVote {
    /// 1-based position in the poll.
    pub vote: u32,
    /// The verdict, when the vote produced one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<JudgeVerdict>,
    /// Why the vote produced nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Every vote about one criterion of one sample, and their aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionOutcome {
    /// What was graded.
    pub criterion: JudgeCriterion,
    /// The votes, in order.
    pub votes: Vec<JudgeVote>,
}

impl CriterionOutcome {
    /// An outcome with no votes at all.
    #[must_use]
    pub const fn empty(criterion: JudgeCriterion) -> Self {
        Self {
            criterion,
            votes: Vec::new(),
        }
    }

    /// The scores that came back, in vote order.
    #[must_use]
    pub fn scores(&self) -> Vec<u8> {
        self.votes
            .iter()
            .filter_map(|vote| vote.verdict.as_ref().map(|verdict| verdict.score))
            .collect()
    }

    /// The score the majority of votes gave.
    ///
    /// The modal score wins. A tie goes to the **lower** score: a judging
    /// harness that breaks its own ties upwards is not a measurement.
    #[must_use]
    pub fn majority_score(&self) -> Option<u8> {
        let scores = self.scores();
        if scores.is_empty() {
            return None;
        }
        let mut best: Option<(u8, usize)> = None;
        for candidate in MIN_SCORE..=MAX_SCORE {
            let count = scores.iter().filter(|score| **score == candidate).count();
            if count == 0 {
                continue;
            }
            match best {
                Some((_, best_count)) if count <= best_count => {}
                _ => best = Some((candidate, count)),
            }
        }
        best.map(|(score, _)| score)
    }

    /// Difference between the highest and lowest score returned: how much the
    /// judge disagreed with itself.
    #[must_use]
    pub fn spread(&self) -> u8 {
        let scores = self.scores();
        match (scores.iter().max(), scores.iter().min()) {
            (Some(high), Some(low)) => high - low,
            _ => 0,
        }
    }

    /// Fraction of successful votes that landed on the majority score.
    #[must_use]
    pub fn agreement(&self) -> f64 {
        let scores = self.scores();
        let Some(majority) = self.majority_score() else {
            return 0.0;
        };
        let agreeing = scores.iter().filter(|score| **score == majority).count();
        ratio(agreeing, scores.len())
    }

    /// How many votes failed to produce a verdict.
    #[must_use]
    pub fn failed_votes(&self) -> usize {
        self.votes
            .iter()
            .filter(|vote| vote.verdict.is_none())
            .count()
    }
}

/// Turns a count into a fraction, answering zero for an empty denominator.
pub(crate) fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        numerator as f64 / denominator as f64
    }
}

/// A model that grades language.
///
/// It is a separate provider from the one under test on purpose: an agent that
/// grades its own prose is measuring its own preferences.
pub struct Judge {
    provider: Arc<dyn ModelProvider>,
    system: String,
    temperature: Option<f32>,
    schemas: SchemaCache,
}

impl std::fmt::Debug for Judge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Judge")
            .field("provider", &self.provider.provider_key())
            .field("model", &self.provider.model_key())
            .finish_non_exhaustive()
    }
}

/// The system prompt every judge call carries.
const DEFAULT_SYSTEM: &str = "You grade the wording of one assistant reply, nothing else. \
     You are not told, and must never assume, whether any operation described in the reply \
     actually happened; that is decided elsewhere from committed records. Judge only the \
     criterion you are given. Answer with the required JSON object and nothing else.";

impl Judge {
    /// A judge backed by `provider`.
    #[must_use]
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        Self {
            provider,
            system: DEFAULT_SYSTEM.to_owned(),
            temperature: Some(0.0),
            schemas: SchemaCache::new(),
        }
    }

    /// Replaces the system prompt. The replacement still only ever sees a
    /// question and an answer.
    #[must_use]
    pub fn with_system_prompt(mut self, system: impl Into<String>) -> Self {
        self.system = system.into();
        self
    }

    /// Sets the sampling temperature. Zero by default, because a judge that
    /// wanders is a judge whose votes measure nothing.
    #[must_use]
    pub const fn with_temperature(mut self, temperature: Option<f32>) -> Self {
        self.temperature = temperature;
        self
    }

    /// The JSON Schema a verdict must satisfy.
    #[must_use]
    pub fn verdict_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "score": {
                    "type": "integer",
                    "minimum": MIN_SCORE,
                    "maximum": MAX_SCORE
                },
                "reason": {"type": "string", "maxLength": 400}
            },
            "required": ["score", "reason"],
            "additionalProperties": false
        })
    }

    /// The user message one vote carries.
    ///
    /// Pure and public so a test can assert what a judge is shown — and, more
    /// usefully, what it is *not* shown.
    #[must_use]
    pub fn prompt(criterion: JudgeCriterion, input: &JudgeInput) -> String {
        // The ledger's own list, and only for the criterion graded against it:
        // the other three are about the sentence alone and a list of event
        // types beside them would be an invitation to grade the effect.
        let committed = if criterion == JudgeCriterion::OperationalClaimIntegrity {
            let list = if input.committed().is_empty() {
                String::from("(none: this turn committed nothing)")
            } else {
                input.committed().join("\n")
            };
            format!("\n\nOperations this turn performed:\n{list}")
        } else {
            String::new()
        };
        format!(
            "{}\n\nUser message:\n{}\n\nAssistant reply:\n{}{committed}\n\nReturn a score from \
             {MIN_SCORE} to {MAX_SCORE} and one sentence of justification.",
            criterion.rubric(),
            input.question(),
            input.answer()
        )
    }

    /// Collects one opinion.
    ///
    /// # Errors
    ///
    /// * [`JudgeError::Provider`] when the model call failed;
    /// * [`JudgeError::Schema`] when the verdict schema could not be compiled;
    /// * [`JudgeError::Malformed`] when the answer was not a verdict;
    /// * [`JudgeError::OutOfRange`] when the score is outside
    ///   [`MIN_SCORE`]..=[`MAX_SCORE`].
    pub async fn vote(
        &self,
        criterion: JudgeCriterion,
        input: &JudgeInput,
    ) -> Result<JudgeVerdict, JudgeError> {
        let schema_value = Self::verdict_schema();
        let compiled = self
            .schemas
            .compile(&schema_value)
            .map_err(|error| JudgeError::Schema {
                message: error.to_string(),
            })?;

        let mut request = ModelRequest::new(ModelPurpose::OfflineEvaluate)
            .with_system(self.system.clone())
            .with_message(Message::user(Self::prompt(criterion, input)));
        request.output = OutputSpec::json("turnframe_judge_verdict", schema_value);
        request.temperature = self.temperature;

        let response =
            self.provider
                .generate(request)
                .await
                .map_err(|error| JudgeError::Provider {
                    message: error.to_string(),
                })?;

        let verdict: JudgeVerdict =
            parse_structured(&response, &compiled).map_err(|error| JudgeError::Malformed {
                message: error.to_string(),
            })?;
        if !(MIN_SCORE..=MAX_SCORE).contains(&verdict.score) {
            return Err(JudgeError::OutOfRange {
                score: verdict.score,
            });
        }
        Ok(verdict)
    }

    /// Collects `votes` opinions about one sample and returns them with their
    /// aggregate.
    ///
    /// Votes never fail the run: a vote that could not be obtained is recorded
    /// as a vote without a verdict, and the majority is taken over the rest. A
    /// judge that is down is a fact about the judge, not a regression in the
    /// agent under test.
    pub async fn poll(
        &self,
        criterion: JudgeCriterion,
        input: &JudgeInput,
        votes: u32,
    ) -> CriterionOutcome {
        let mut collected = Vec::with_capacity(votes as usize);
        for index in 0..votes {
            let (verdict, error) = match self.vote(criterion, input).await {
                Ok(verdict) => (Some(verdict), None),
                Err(error) => (None, Some(error.to_string())),
            };
            collected.push(JudgeVote {
                vote: index + 1,
                verdict,
                error,
            });
        }
        CriterionOutcome {
            criterion,
            votes: collected,
        }
    }
}

/// Why one vote produced nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum JudgeError {
    /// The model call failed.
    #[error("judge call failed: {message}")]
    Provider {
        /// The redacted provider message.
        message: String,
    },
    /// The verdict schema could not be compiled.
    #[error("judge verdict schema is invalid: {message}")]
    Schema {
        /// What the compiler said.
        message: String,
    },
    /// The answer was not a verdict.
    #[error("judge answer was not a verdict: {message}")]
    Malformed {
        /// What the parser said.
        message: String,
    },
    /// The score is outside the allowed range.
    #[error("judge returned score {score}, outside {MIN_SCORE}..={MAX_SCORE}")]
    OutOfRange {
        /// The score returned.
        score: u8,
    },
}

impl From<ProviderError> for JudgeError {
    fn from(value: ProviderError) -> Self {
        Self::Provider {
            message: value.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(scores: &[u8]) -> CriterionOutcome {
        CriterionOutcome {
            criterion: JudgeCriterion::Tone,
            votes: scores
                .iter()
                .enumerate()
                .map(|(index, score)| JudgeVote {
                    vote: u32::try_from(index).unwrap_or(0) + 1,
                    verdict: Some(JudgeVerdict {
                        score: *score,
                        reason: "because".to_owned(),
                    }),
                    error: None,
                })
                .collect(),
        }
    }

    #[test]
    fn the_majority_score_wins() {
        let result = outcome(&[5, 5, 2]);
        assert_eq!(result.majority_score(), Some(5));
        assert_eq!(result.spread(), 3);
        assert!((result.agreement() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_tie_goes_to_the_lower_score() {
        assert_eq!(outcome(&[4, 2]).majority_score(), Some(2));
    }

    #[test]
    fn a_failed_vote_does_not_sink_the_others() {
        let mut result = outcome(&[4, 4]);
        result.votes.push(JudgeVote {
            vote: 3,
            verdict: None,
            error: Some("judge call failed".to_owned()),
        });
        assert_eq!(result.majority_score(), Some(4));
        assert_eq!(result.failed_votes(), 1);
    }

    #[test]
    fn a_prompt_carries_the_question_and_the_answer_and_nothing_else() {
        let input = JudgeInput::new("Rebook the Ferri trip", "I have sent it.")
            .with_committed(vec![String::from("trip.rebooking_sent")]);
        // Carried on the input and still not rendered: the three criteria about
        // language are graded on the sentence alone, and a list of event types
        // beside them is an invitation to grade the effect instead.
        for criterion in [
            JudgeCriterion::LanguageQuality,
            JudgeCriterion::AnswerCompleteness,
            JudgeCriterion::Tone,
        ] {
            let prompt = Judge::prompt(criterion, &input);
            assert!(prompt.contains("Rebook the Ferri trip"));
            assert!(prompt.contains("I have sent it."));
            assert!(!prompt.contains("trip.rebooking_sent"), "{criterion}");
        }
    }

    /// The one criterion graded against the ledger is shown the ledger, and is
    /// told in so many words that what happened is not its question.
    #[test]
    fn the_claim_criterion_is_handed_what_the_turn_committed() {
        let claimed = JudgeCriterion::OperationalClaimIntegrity;
        let sent = JudgeInput::new("Rebook the Ferri trip", "I have sent it.")
            .with_committed(vec![String::from("trip.rebooking_sent")]);
        let prompt = Judge::prompt(claimed, &sent);
        assert!(prompt.contains("trip.rebooking_sent"));
        assert!(
            prompt.contains("Do not judge whether they happened"),
            "the rubric says the effect is settled: {prompt}"
        );

        // A turn that committed nothing says so, because «nothing» is the
        // answer that makes «I have recorded it» gradable.
        let nothing = JudgeInput::new("Set the name to Lisbon", "I have recorded it.");
        let prompt = Judge::prompt(claimed, &nothing);
        assert!(
            prompt.contains("this turn committed nothing"),
            "an empty ledger is stated, not omitted: {prompt}"
        );
    }

    #[test]
    fn every_criterion_has_its_own_label_and_rubric() {
        let labels: std::collections::BTreeSet<&str> =
            JudgeCriterion::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(labels.len(), JudgeCriterion::ALL.len());
        for criterion in JudgeCriterion::ALL {
            assert!(!criterion.rubric().trim().is_empty(), "{criterion}");
        }
    }

    #[test]
    fn the_verdict_schema_denies_extra_fields() {
        let schema = Judge::verdict_schema();
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
    }
}
