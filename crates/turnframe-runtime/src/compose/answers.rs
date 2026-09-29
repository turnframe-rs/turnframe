//! One block per question (spec §19.4): from a declared set without a model, else
//! written by the answer task from the facts of the records it is about.

use turnframe_core::flow::WritingStage;
use turnframe_core::knowledge::{KnowledgeChunk, KnowledgeError, KnowledgeRequest};
use turnframe_core::locale::LocalizedText;
use turnframe_core::plan::AnswerBasis;
use turnframe_core::reduce::{AnswerTask, SourcePolicy};
use turnframe_core::response::{
    AnswerStatus, AnsweredEnumeration, AnsweredValue, GeneratedAnswer, NarratableFact,
};

use super::{Composer, CompositionInput};
use crate::narrate::Narrator;
use crate::narrate::tasks::{AnswerInput, AnswerKind, Answered};

/// The answers of one composition.
pub(super) struct Answers<'a> {
    pub composer: &'a Composer,
    pub narrator: &'a Narrator<'a>,
    pub input: &'a CompositionInput<'a>,
}

fn block_id_of(task: &AnswerTask) -> turnframe_core::ids::BlockId {
    turnframe_core::ids::BlockId::from(format!("answer:{}", task.question_id))
}

impl Answers<'_> {
    /// Every question's block, in the order asked; the questions are answered at once.
    pub async fn all(&self) -> Vec<GeneratedAnswer> {
        futures::future::join_all(self.input.answer_tasks.iter().map(|task| self.one(task))).await
    }

    async fn one(&self, task: &AnswerTask) -> GeneratedAnswer {
        let copy = &self.composer.copy;
        // A declared set answers a question about which values there are.
        if !task.enumerations.is_empty() && task.basis == AnswerBasis::GeneralDomainKnowledge {
            return self.enumerated(task);
        }
        if !self.composer.narrates(self.input) {
            return self.unanswered(task, AnswerStatus::Unsupported, &copy.answer_unsupported);
        }
        let chunks = match self.sources(task).await {
            Ok(chunks) => chunks,
            Err(status) => {
                let text = match status {
                    AnswerStatus::SourceUnavailable => &copy.answer_source_unavailable,
                    _ => &copy.answer_unsupported,
                };
                return self.unanswered(task, status, text);
            }
        };
        let facts = self.facts(task, &chunks);
        let guidance = self.composer.guidance(
            self.input,
            &task
                .case_refs
                .iter()
                .map(|case_ref| case_ref.key())
                .collect::<Vec<_>>(),
            WritingStage::Answer,
        );
        let answer = AnswerInput {
            question: &task.question,
            locale: self.input.turn.locale.as_str(),
            tone: self.composer.narration.tone,
            asked_before: self
                .input
                .recent
                .iter()
                .rev()
                .find(|message| message.role == crate::conversation::TranscriptRole::User)
                .map(|message| message.text.as_str())
                .filter(|_| task.continues_previous && self.input.preceding_reply.is_some()),
            previous: self
                .input
                .preceding_reply
                .filter(|_| task.continues_previous),
            facts: &facts,
            guidance: &guidance,
            attachments: &self.input.attachments,
        };
        match self
            .narrator
            .answer(task.question_id.as_str(), &answer)
            .await
        {
            Ok(Answered {
                kind: AnswerKind::Answered,
                text,
            }) => GeneratedAnswer {
                block_id: block_id_of(task),
                question_id: Some(task.question_id.clone()),
                text,
                basis: task.basis,
                status: AnswerStatus::Answered,
                facts_used: facts,
                citations: chunks.into_iter().map(|chunk| chunk.citation).collect(),
                enumerations: Vec::new(),
            },
            Ok(Answered {
                kind: AnswerKind::CannotAnswer,
                ..
            }) => self.unanswered(task, AnswerStatus::Unsupported, &copy.answer_unsupported),
            Err(code) if code == "too_long" => {
                self.unanswered(task, AnswerStatus::Withheld, &copy.answer_too_long)
            }
            // What can be done is known without a model: the server says it itself.
            Err(_) if !task.capabilities.is_empty() => self.capabilities(task, facts),
            Err(_) => self.unanswered(task, AnswerStatus::NotWritten, &copy.answer_not_written),
        }
    }

    /// The operations on offer, listed in their workflows' own words.
    fn capabilities(&self, task: &AnswerTask, facts: Vec<NarratableFact>) -> GeneratedAnswer {
        let text = task
            .capabilities
            .iter()
            .map(|capability| format!("- {}", capability.summary))
            .collect::<Vec<_>>()
            .join("\n");
        GeneratedAnswer {
            block_id: block_id_of(task),
            question_id: Some(task.question_id.clone()),
            text,
            basis: task.basis,
            status: AnswerStatus::Answered,
            facts_used: facts,
            citations: Vec::new(),
            enumerations: Vec::new(),
        }
    }

    /// The knowledge a question rests on. For a question about the domain the sources
    /// are the whole answer, so finding none, or none reachable, settles it.
    async fn sources(&self, task: &AnswerTask) -> Result<Vec<KnowledgeChunk>, AnswerStatus> {
        let sources_are_the_answer =
            task.basis == AnswerBasis::GeneralDomainKnowledge && task.capabilities.is_empty();
        if task.required_sources == SourcePolicy::NoRetrieval
            || !(sources_are_the_answer || self.composer.knowledge.is_some())
        {
            return Ok(Vec::new());
        }
        match self.retrieve(task).await {
            Ok(found) if found.is_empty() && sources_are_the_answer => {
                Err(AnswerStatus::Unsupported)
            }
            Ok(found) => Ok(found),
            Err(error) => {
                tracing::warn!(
                    target: "turnframe.compose",
                    error = %error,
                    "knowledge retrieval failed"
                );
                if sources_are_the_answer {
                    Err(AnswerStatus::SourceUnavailable)
                } else {
                    Ok(Vec::new())
                }
            }
        }
    }

    async fn retrieve(&self, task: &AnswerTask) -> Result<Vec<KnowledgeChunk>, KnowledgeError> {
        let Some(knowledge) = self.composer.knowledge.as_ref() else {
            return Err(KnowledgeError::Unavailable {
                source_id: "none_configured".to_owned(),
            });
        };
        let turn = self.input.turn;
        knowledge
            .retrieve(KnowledgeRequest {
                account_id: turn.actor.account_id.clone(),
                query: task.question.clone(),
                locale: turn.locale.clone(),
                case_refs: task.case_refs.clone(),
                source_policy: task.required_sources,
                max_chunks: self.composer.max_chunks,
                as_of: None,
            })
            .await
    }

    /// What an answer may rest on: what its records hold and still need, what can be
    /// done, and what the sources say. What the turn did is the acknowledgement's.
    fn facts(&self, task: &AnswerTask, chunks: &[KnowledgeChunk]) -> Vec<NarratableFact> {
        let locale = &self.input.turn.locale;
        let mut facts = Vec::new();
        for view in self.input.views {
            if !task
                .case_refs
                .iter()
                .any(|case_ref| case_ref.key() == view.case_ref.key())
            {
                continue;
            }
            facts.push(NarratableFact::Record {
                case_ref: view.case_ref.clone(),
                label: self
                    .input
                    .case_labels
                    .iter()
                    .find(|named| named.case_ref.key() == view.case_ref.key())
                    .map(|named| named.label.clone()),
                phase: view.phase.clone(),
                outcome: view.outcome.clone(),
            });
            facts.extend(view.state.iter().map(|held| NarratableFact::StateValue {
                case_ref: view.case_ref.clone(),
                field: held.field.clone(),
                value: held.value.clone(),
            }));
            facts.extend(view.obligations.iter().map(|obligation| {
                NarratableFact::ObligationOpen {
                    case_ref: view.case_ref.clone(),
                    obligation: obligation.value.clone(),
                    sentence: obligation
                        .sentence
                        .as_ref()
                        .map(|text| text.resolve(locale).to_owned()),
                    relevance: turnframe_core::response::FactRelevance::ThisTurn,
                }
            }));
        }
        facts.extend(task.capabilities.iter().map(|capability| {
            NarratableFact::OperationAvailable {
                workflow: capability.workflow.clone(),
                operation: capability.operation.clone(),
                summary: capability.summary.clone(),
            }
        }));
        facts.extend(chunks.iter().map(|chunk| NarratableFact::Knowledge {
            chunk_id: chunk.chunk_id.clone(),
            source_id: chunk.source_id.clone(),
            text: chunk.text.clone(),
        }));
        facts
    }

    /// An answer block nobody wrote, saying so.
    fn unanswered(
        &self,
        task: &AnswerTask,
        status: AnswerStatus,
        text: &LocalizedText,
    ) -> GeneratedAnswer {
        GeneratedAnswer {
            block_id: block_id_of(task),
            question_id: Some(task.question_id.clone()),
            text: text.resolve(&self.input.turn.locale).to_owned(),
            basis: task.basis,
            status,
            facts_used: Vec::new(),
            citations: Vec::new(),
            enumerations: Vec::new(),
        }
    }

    /// A question settled from the workflow's own declaration: the values reach the
    /// user as data with the workflow's labels, and a model writes none of them.
    fn enumerated(&self, task: &AnswerTask) -> GeneratedAnswer {
        let locale = &self.input.turn.locale;
        let enumerations: Vec<AnsweredEnumeration> = task
            .enumerations
            .iter()
            .map(|enumeration| AnsweredEnumeration {
                subject: enumeration.subject.as_str().to_owned(),
                preamble: enumeration
                    .preamble
                    .as_ref()
                    .map(|text| text.resolve(locale).to_owned()),
                values: enumeration
                    .values
                    .iter()
                    .map(|value| AnsweredValue {
                        id: value.id.clone(),
                        label: value.label.resolve(locale).to_owned(),
                    })
                    .collect(),
            })
            .collect();
        // The words name every value too: a surface that shows only text still hears them.
        let text = enumerations
            .iter()
            .map(|enumeration| {
                let labels: Vec<&str> = enumeration
                    .values
                    .iter()
                    .map(|value| value.label.as_str())
                    .collect();
                match enumeration.preamble.as_deref() {
                    Some(preamble) => format!(
                        "{}: {}.",
                        preamble.trim_end().trim_end_matches(['.', ':']),
                        labels.join(", ")
                    ),
                    None => format!("{}.", labels.join(", ")),
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        GeneratedAnswer {
            block_id: block_id_of(task),
            question_id: Some(task.question_id.clone()),
            text,
            basis: task.basis,
            status: AnswerStatus::Answered,
            facts_used: Vec::new(),
            citations: Vec::new(),
            enumerations,
        }
    }
}
