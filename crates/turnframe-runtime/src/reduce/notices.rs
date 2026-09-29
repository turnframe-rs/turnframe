//! What the turn tells the user and the writer beside its acts: notices, facts and
//! the questions to answer.

use super::*;

impl Session<'_> {
    pub(super) fn record_constraint(&mut self, constraint: ConstraintKind) {
        if !self.applied.contains(&constraint) {
            self.applied.push(constraint);
        }
    }

    pub(super) fn add_constraint_notice(&mut self, constraint: ConstraintKind) {
        let copy = &self.reducer.copy;
        let (code, text) = match constraint {
            ConstraintKind::DoNotSubmit => {
                (notice::NOTHING_SUBMITTED, copy.nothing_submitted.clone())
            }
            ConstraintKind::DoNotDelete => (notice::NOTHING_DELETED, copy.nothing_deleted.clone()),
            ConstraintKind::DraftOnly => (notice::DRAFT_ONLY, copy.draft_only.clone()),
            ConstraintKind::NoExternalEffects => (
                notice::NO_EXTERNAL_EFFECTS,
                copy.no_external_effects.clone(),
            ),
            _ => return,
        };
        self.add_notice(code, NoticeSeverity::Info, text);
    }

    pub(super) fn add_partial_result_notice(&mut self) {
        let progressed = self.results.iter().flatten().any(|result| {
            matches!(
                result,
                PlannedActResult::ReadyToExecute { .. }
                    | PlannedActResult::AwaitingConfirmation { .. }
            )
        });
        let stalled = self.results.iter().flatten().any(|result| {
            matches!(
                result,
                PlannedActResult::Rejected { .. } | PlannedActResult::NeedsClarification { .. }
            )
        });
        if progressed && stalled {
            let text = self.reducer.copy.partial_result.clone();
            self.add_notice(notice::PARTIAL_RESULT, NoticeSeverity::Warning, text);
        }
    }

    /// The words that produced nothing, as facts and as one notice quoting them.
    pub(super) fn note_not_understood(&mut self, understanding: &Understanding) {
        if understanding.unreadable.is_some() {
            let text = self.reducer.copy.message_unreadable.clone();
            self.add_notice(notice::MESSAGE_UNREADABLE, NoticeSeverity::Warning, text);
        }
        let mut kept: Vec<String> = Vec::new();
        for item in &understanding.not_understood {
            if let NotUnderstoodReason::KeptUnchanged { constraint } = item.reason
                && let Some(asked) = understanding
                    .constraints
                    .iter()
                    .find(|c| c.unit == constraint)
            {
                let said = self.words(asked.words).to_owned();
                if !said.is_empty() && !kept.contains(&said) {
                    kept.push(said);
                }
            }
        }
        if !kept.is_empty() {
            let text = filled(&self.reducer.copy.kept_unchanged, &kept.join("», «"));
            self.add_notice(notice::KEPT_UNCHANGED, NoticeSeverity::Info, text);
        }
        let words: Vec<String> = understanding
            .not_understood
            .iter()
            .filter(|item| !matches!(item.reason, NotUnderstoodReason::KeptUnchanged { .. }))
            .map(|item| self.words(item.words).to_owned())
            .filter(|words| !words.is_empty())
            .collect();
        for said in &words {
            self.changed_nothing.push(NarratableFact::NotUnderstood {
                words: said.clone(),
            });
        }
        if words.is_empty() {
            return;
        }
        let text = filled(&self.reducer.copy.not_understood, &words.join("», «"));
        self.add_notice(notice::NOT_UNDERSTOOD, NoticeSeverity::Info, text);
    }

    /// Records a refusal so both the user (a notice) and the writer (a fact) learn of
    /// it. The domain's own sentence wins, then the runtime's for its own codes, then
    /// the catch-all.
    pub(super) fn note_refusal(&mut self, case_ref: Option<&CaseRef>, rejection: &DomainRejection) {
        let locale = &self.input.locale;
        let (code, text) = match &rejection.explanation {
            Some(text) => (notice::ACT_REFUSED, (**text).clone()),
            None => match self.reducer.copy.runtime_refusal(rejection.code.as_str()) {
                Some((code, text)) => (code, text.clone()),
                None => (notice::ACT_REFUSED, self.reducer.copy.act_refused.clone()),
            },
        };
        self.refusals.push(NarratableFact::ActRefused {
            case_ref: case_ref.cloned(),
            code: rejection.code.as_str().to_owned(),
            explanation: text.resolve(locale).to_owned(),
        });
        self.add_notice(code, NoticeSeverity::Warning, text);
    }

    pub(super) fn add_notice(
        &mut self,
        code: &'static str,
        severity: NoticeSeverity,
        text: LocalizedText,
    ) {
        self.notices.entry(code).or_insert_with(|| ServerNotice {
            block_id: BlockId::from(format!("notice:{code}")),
            code: code.to_owned(),
            severity,
            text,
        });
    }

    /// Every complete value set the workflows of the cases in view declared, by subject.
    pub(super) fn declared_enumerations(
        &self,
    ) -> BTreeMap<turnframe_core::flow::QuestionReference, DomainEnumeration> {
        let mut declared = BTreeMap::new();
        for view in self.context.views.values() {
            let Ok(definition) = self.reducer.workflows.require(&view.case_ref.workflow) else {
                continue;
            };
            let state = self.reducer.states.get(&view.case_ref.key());
            let Ok(enumerations) = definition.enumerations(view.case_ref.clone(), state) else {
                continue;
            };
            for enumeration in enumerations {
                declared.insert(enumeration.subject.clone(), enumeration);
            }
        }
        declared
    }

    /// One task per question (rule 5), with a basis the turn can support.
    pub(super) fn answer_tasks(&self, understanding: &Understanding) -> Vec<AnswerTask> {
        let proposed = self.results.iter().flatten().any(|result| {
            matches!(
                result,
                PlannedActResult::AwaitingConfirmation { .. }
                    | PlannedActResult::NeedsClarification { .. }
            ) || matches!(
                result,
                PlannedActResult::ReadyToExecute { command_refs } if !command_refs.is_empty()
            )
        });
        let executing = self.batches.values().any(|batch| !batch.is_empty());
        let turn_cases = self.turn_cases();
        let turn = self.turn_id().to_string();
        let declared = self.declared_enumerations();
        understanding
            .questions
            .iter()
            .map(|question| {
                let enumerations: Vec<DomainEnumeration> = match question.topic {
                    QuestionTopic::AcceptedValues => question
                        .subjects
                        .iter()
                        .filter_map(|subject| {
                            declared
                                .get(&turnframe_core::flow::QuestionReference::from(
                                    subject.as_str(),
                                ))
                                .cloned()
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let basis = match (question.topic, question.basis) {
                    // What the user can do is read off what is on offer now, and what a
                    // record holds is never general knowledge.
                    (QuestionTopic::Capabilities, _)
                    | (QuestionTopic::RecordState, AnswerBasis::GeneralDomainKnowledge) => {
                        AnswerBasis::CurrentCommittedState
                    }
                    // A declared set answers a question about the values it holds.
                    (QuestionTopic::AcceptedValues, _) if !enumerations.is_empty() => {
                        AnswerBasis::GeneralDomainKnowledge
                    }
                    // A proposal nobody made, or a state no command produces, is nothing.
                    (_, AnswerBasis::ProposedState) if !proposed => {
                        AnswerBasis::CurrentCommittedState
                    }
                    (_, AnswerBasis::CommittedStateAfterTurn) if !executing => {
                        AnswerBasis::CurrentCommittedState
                    }
                    (_, basis) => basis,
                };
                let resolver = &self.reducer.resolver;
                let framed = question.record.as_ref().and_then(|token| {
                    resolver
                        .token_map()
                        .resolve(resolver.account_id(), token)
                        .exact()
                        .cloned()
                });
                // A question about records that framed none is about the records of this
                // work in view, when the turn reached none: one about what is saved now
                // is about records whatever topic it was given.
                let about_records = question.topic == QuestionTopic::RecordState
                    || (question.topic == QuestionTopic::Knowledge
                        && basis == AnswerBasis::CurrentCommittedState);
                let case_refs = match (&framed, question.topic) {
                    (Some(case_ref), _) => vec![case_ref.clone()],
                    (None, _) if about_records && turn_cases.is_empty() => resolver
                        .in_view()
                        .filter(|case| !case.subject_only_when_named)
                        .map(|case| case.case_ref.clone())
                        .collect(),
                    (None, _) => turn_cases.clone(),
                };
                AnswerTask {
                    question_id: QuestionId::from(question.unit.to_string()),
                    question: self.words(question.words).to_owned(),
                    basis,
                    case_refs,
                    proposed_diff_ref: (basis == AnswerBasis::ProposedState).then(|| turn.clone()),
                    required_sources: if basis == AnswerBasis::GeneralDomainKnowledge {
                        self.reducer.narration.default_source_policy
                    } else {
                        SourcePolicy::AuthoritativeOnly
                    },
                    enumerations,
                    continues_previous: question.continues_previous,
                    capabilities: match question.topic {
                        QuestionTopic::Capabilities => self.capabilities(framed.as_ref()),
                        _ => Vec::new(),
                    },
                    asked_at: Some(TextSpan {
                        start_byte: question.words.start,
                        end_byte: question.words.end,
                    }),
                }
            })
            .collect()
    }

    /// What the user can do now: the operations on offer that a message may ask for,
    /// on the framed record's workflow when the question names one. One that only a
    /// card on screen can take is left out while no card is.
    pub(super) fn capabilities(&self, framed: Option<&CaseRef>) -> Vec<Capability> {
        let card_open = !self.context.active_interactions.is_empty();
        self.context
            .operations
            .iter()
            .filter(|spec| spec.availability.is_proposable())
            .filter(|spec| card_open || spec.target_policy != TargetPolicy::ActiveInteractionOnly)
            .filter(|spec| framed.is_none_or(|case_ref| case_ref.workflow == spec.workflow))
            .map(|spec| Capability {
                workflow: spec.workflow.clone(),
                operation: spec.key.clone(),
                summary: spec.summary_for(&self.input.locale).to_owned(),
            })
            .collect()
    }

    /// Every case the turn resolved exactly, deduplicated, in act order.
    pub(super) fn turn_cases(&self) -> Vec<CaseRef> {
        let mut seen: Vec<CaseRef> = Vec::new();
        for outcome in self.outcomes.iter().flatten() {
            if let Some(case_ref) = outcome.exact()
                && !seen.iter().any(|other| other.same_case(case_ref))
            {
                seen.push(case_ref.clone());
            }
        }
        seen
    }
}

/// `template` in every locale, with `{words}` replaced.
fn filled(template: &LocalizedText, words: &str) -> LocalizedText {
    let mut text = LocalizedText::new(template.default.replace("{words}", words));
    for (locale, translation) in &template.translations {
        text = text.with(locale.clone(), translation.replace("{words}", words));
    }
    text
}
