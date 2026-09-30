//! Steps O–V: what the turn did as facts and notices, the answer composed from them,
//! and the turn persisted exactly as returned.

use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::error::OrchestratorError;
use turnframe_core::flow::NextStep;
use turnframe_core::ids::BlockId;
use turnframe_core::observe::{Signal, SignalLabels};
use turnframe_core::reduce::{PlannedActResult, ReductionPlan};
use turnframe_core::replay::{CommandOutcome, TurnPhase};
use turnframe_core::response::{
    AnswerProgress, AnswerStatus, AssistantTurn, CaseLabel, DoneAct, Expectation, NarratableFact,
    NoticeSeverity, ResponseBlock, ServerNotice,
};
use turnframe_core::target::TargetResolution;
use turnframe_store::events::LedgerReceiptGroup;

use super::Session;
use crate::compose::{CompositionInput, OutcomeFlags};
use crate::conversation::UnavailableWorkflow;
use crate::execute::ExecutionReport;
use crate::interactions::PersistedInteractions;
use crate::reduce::{ReducedTurn, notice};
use crate::turn::unmet_preconditions;

/// A notice under its own code.
fn notice_of(
    code: &str,
    severity: NoticeSeverity,
    text: turnframe_core::locale::LocalizedText,
) -> ServerNotice {
    ServerNotice {
        block_id: BlockId::from(format!("notice:{code}")),
        code: code.to_owned(),
        severity,
        text,
    }
}

impl Session<'_> {
    pub(super) async fn answer(
        &mut self,
        reduced: &ReducedTurn,
        execution: &ExecutionReport,
        persisted: &PersistedInteractions,
    ) -> Result<AssistantTurn, OrchestratorError> {
        let plan = &reduced.plan;
        let subjects = self.subjects(plan);
        let reachable_only: Vec<CaseKey> = self
            .cases
            .iter()
            .filter(|(_, case)| case.terms.subject_only_when_named)
            .map(|(key, _)| key.clone())
            .collect();
        let (views, states, follow_up, case_refresh_unavailable) =
            self.views_and_follow_up(execution, plan).await;
        let named = self.named_workflows(plan);
        let case_labels: Vec<CaseLabel> = self
            .cases
            .values()
            .map(|case| CaseLabel {
                case_ref: case.case_ref.clone(),
                label: case.label.clone(),
            })
            .collect();
        let mut interactions = persisted.created.clone();
        interactions.extend(follow_up.created.clone());
        // A budget spent after commit stops the model calls, not the commit (§11.1).
        let budget_exhausted = self.spent_limit().is_some();

        let mut facts = plan.refusals.clone();
        facts.extend(plan.awaiting_confirmation.clone());
        facts.extend(plan.changed_nothing.clone());
        facts.extend(self.declined_fact.clone());
        let mut notices = self.early_notices.clone();
        notices.extend(plan.notices.iter().cloned());
        self.execution_refusals(execution, &mut facts, &mut notices);
        self.omitted_attachments(&mut facts, &mut notices);
        let replayed = self.replay_facts(&mut facts, &mut notices).await;
        let unavailable = self.unavailable();
        self.blocked_requests(&unavailable, &mut facts, &mut notices);
        let preceding = self
            .previous
            .as_ref()
            .map(crate::orchestrator::assistant_text);

        // A turn that reached no record is on the one its question names, else the one
        // the conversation is on, else the records its questions were answered from.
        let mut touched = touched(plan);
        let acted = !touched.is_empty();
        if touched.is_empty() {
            touched = asked_about(plan, true);
        }
        if touched.is_empty()
            && let Some(previous) = &self.previous
        {
            touched = previous
                .subjects
                .iter()
                .map(CaseRef::key)
                .filter(|key| self.cases.contains_key(key))
                .collect();
        }
        if touched.is_empty() {
            touched = asked_about(plan, false);
        }
        // A turn that did something, or read nothing it could act on, goes on to the
        // next record in view with work open once its own have none.
        let unread = self
            .record
            .understanding
            .as_ref()
            .is_some_and(|understanding| !understanding.not_understood.is_empty());
        let beside: Vec<CaseKey> = if acted || unread {
            self.cases
                .keys()
                .filter(|key| !touched.contains(key))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let (disputes, contested) = self.disputes();
        let started = started(plan);
        let workflows = self.runtime.workflows.definitions();
        let next_steps: Vec<(CaseRef, Vec<NextStep>)> = views
            .iter()
            .filter(|view| touched.contains(&view.case_ref.key()))
            .filter_map(|view| {
                let definition = workflows.get(&view.case_ref.workflow)?;
                let state = states.get(&view.case_ref.key()).cloned().flatten();
                let steps: Vec<NextStep> = definition
                    .next_steps(view.case_ref.clone(), state.as_ref())
                    .ok()?
                    .into_iter()
                    .filter(|step| {
                        offerable(definition.as_ref(), &view.case_ref, state.as_ref(), step)
                    })
                    .collect();
                (!steps.is_empty()).then(|| (view.case_ref.clone(), steps))
            })
            .collect();
        // A record a request needed and none of exists is offered, by the operation opening one.
        let openings: Vec<(CaseRef, turnframe_core::ids::OperationKey, String)> = self
            .none_yet
            .iter()
            .filter_map(|workflow| {
                let definition = workflows.get(workflow)?;
                let fresh = CaseRef::new(
                    workflow.clone(),
                    "",
                    turnframe_core::ids::CaseRevision::ZERO,
                );
                let opening = definition
                    .operations(fresh.clone(), None)
                    .ok()?
                    .into_iter()
                    .find(|spec| {
                        spec.target_policy == turnframe_core::plan::TargetPolicy::NewCaseOnly
                    })?;
                let noun = definition.noun().map_or_else(
                    || workflow.to_string(),
                    |noun| noun.resolve(&self.input.locale).to_owned(),
                );
                Some((fresh, opening.key, noun))
            })
            .collect();
        // Cards earlier turns left open are still on screen: the reply may point to one.
        let open_cards: Vec<turnframe_core::interaction::Interaction> = self
            .runtime
            .interactions
            .open_for_conversation(self.account(), &self.input.conversation_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|card| !interactions.iter().any(|created| created.id == card.id))
            .collect();
        let input = CompositionInput::new(&self.input)
            .with_attachments(self.attachments.parts.clone())
            // The first click's events, so «already done» rests on what did it.
            .with_ledger(&replayed)
            .with_answer_tasks(&plan.answer_tasks)
            .with_events(&execution.events)
            .with_interactions(&interactions)
            .with_open_cards(&open_cards)
            .with_views(&views)
            .with_subjects(&subjects)
            .with_touched(&touched)
            .with_next_steps(&next_steps)
            .with_openings(&openings)
            .with_asked_before(
                self.previous
                    .as_ref()
                    .map_or(&[][..], |previous| previous.expectations.as_slice()),
            )
            .with_beside(&beside)
            .with_effort(Some(&self.effort))
            .with_disputes(&disputes)
            .with_contested(&contested)
            .with_started(&started)
            .with_reachable_only(&reachable_only)
            .with_recent(&self.recent)
            .with_named_workflows(&named)
            .with_case_labels(&case_labels)
            .with_artifacts_shown(&self.artifacts_shown)
            .with_preceding_reply(preceding.as_deref())
            .with_unavailable(&unavailable)
            .with_notices(&notices)
            .with_refusals(&facts)
            .with_outcomes(OutcomeFlags {
                had_failure: execution.any_uncommitted(),
                outcome_unknown: execution.has_unknown_outcome(),
                revision_conflict: execution.outcomes.iter().any(|record| {
                    matches!(record.outcome, CommandOutcome::RevisionConflict { .. })
                }),
                // A card that could not be written must not be mentioned (§15.5).
                interaction_unavailable: !persisted.is_complete() || !follow_up.is_complete(),
                case_refresh_unavailable,
                budget_exhausted,
                instruction_declined: self.declined_instruction,
            });
        let mut composition = self.runtime.composer.compose(input).await?;
        composition.turn.expectations = expectations(plan);
        composition.turn.done = done(plan, execution);
        composition
            .turn
            .expectations
            .extend(composition.expectation.take());
        let listed: Vec<CaseLabel> = self
            .cases
            .values()
            .map(|case| CaseLabel {
                case_ref: case.case_ref.clone(),
                label: case.label.clone(),
            })
            .collect();
        let carried = still_waiting(
            self.previous.as_ref(),
            plan,
            &composition.turn.expectations,
            &listed,
        );
        composition.turn.expectations.extend(carried);
        self.record.tasks.extend(composition.tasks.iter().cloned());
        self.record.budget = Some(spent_together(
            self.record.budget.take().unwrap_or_default(),
            &composition.budget,
        ));
        self.publisher.phase_reached(TurnPhase::Composed);
        self.publisher.blocks(&composition.turn);
        self.observe_composition(&composition.turn);

        self.runtime
            .stores
            .conversations()
            .append_assistant_turn(self.account(), composition.turn.clone())
            .await
            .map_err(OrchestratorError::Store)?;
        self.finish(&composition.turn).await?;
        self.publisher.completed(&composition.turn);
        Ok(composition.turn)
    }

    /// Refusals only execution could decide, told on both channels: a notice with the
    /// domain's own sentence, and a fact so the prose does not claim the write.
    fn execution_refusals(
        &self,
        execution: &ExecutionReport,
        facts: &mut Vec<NarratableFact>,
        notices: &mut Vec<ServerNotice>,
    ) {
        for (case_ref, rejection) in &execution.rejections {
            facts.push(NarratableFact::ActRefused {
                case_ref: Some(case_ref.clone()),
                code: rejection.code.as_str().to_owned(),
                explanation: rejection
                    .explanation
                    .as_ref()
                    .map(|text| text.resolve(&self.input.locale).to_owned())
                    .unwrap_or_default(),
            });
            if let Some(text) = rejection.explanation.as_ref() {
                notices.push(notice_of(
                    notice::ACT_REFUSED,
                    NoticeSeverity::Warning,
                    (**text).clone(),
                ));
            }
        }
    }

    /// A request nothing on offer does, about a workflow that cannot start, was understood:
    /// it is refused with the workflow's own reason, not reported as not understood.
    fn blocked_requests(
        &self,
        unavailable: &[UnavailableWorkflow],
        facts: &mut Vec<NarratableFact>,
        notices: &mut Vec<ServerNotice>,
    ) {
        let Some(understanding) = &self.record.understanding else {
            return;
        };
        let text = self.input.text.as_deref().unwrap_or_default();
        for missed in &understanding.not_understood {
            if !matches!(
                missed.reason,
                turnframe_core::understanding::NotUnderstoodReason::NoOperation
            ) {
                continue;
            }
            let workflow = understanding
                .units
                .iter()
                .find(|unit| unit.id == missed.unit)
                .and_then(|unit| unit.workflow.as_ref());
            let Some(blocked) = unavailable
                .iter()
                .find(|blocked| Some(&blocked.workflow) == workflow)
            else {
                continue;
            };
            let words = text
                .get(missed.words.start..missed.words.end)
                .unwrap_or_default();
            facts.retain(|fact| {
                !matches!(fact, NarratableFact::NotUnderstood { words: said } if said == words)
            });
            facts.push(NarratableFact::ActRefused {
                case_ref: None,
                code: crate::reduce::rejection::PRECONDITION_UNMET.to_owned(),
                explanation: blocked.reason.clone(),
            });
            notices.push(notice_of(
                notice::ACT_REFUSED,
                NoticeSeverity::Warning,
                turnframe_core::locale::LocalizedText::new(blocked.reason.clone()),
            ));
        }
        if !facts
            .iter()
            .any(|fact| matches!(fact, NarratableFact::NotUnderstood { .. }))
        {
            notices.retain(|shown| shown.code != notice::NOT_UNDERSTOOD);
        }
    }

    /// A file the turn carried and no model was shown, on both channels.
    fn omitted_attachments(
        &self,
        facts: &mut Vec<NarratableFact>,
        notices: &mut Vec<ServerNotice>,
    ) {
        for omitted in &self.attachments.omitted {
            let text = self
                .runtime
                .attachment_copy
                .for_reason(omitted.reason)
                .clone();
            facts.push(NarratableFact::AttachmentNotShown {
                attachment_id: omitted.attachment_id.clone(),
                filename: omitted.filename.clone(),
                reason: text.resolve(&self.input.locale).to_owned(),
            });
            notices.push(notice_of(
                notice::ATTACHMENT_NOT_SHOWN,
                NoticeSeverity::Warning,
                text,
            ));
        }
    }

    /// A second click: a notice and a fact saying so, and the first click's receipts
    /// read back from the ledger. A ledger that cannot be read costs the receipts.
    async fn replay_facts(
        &self,
        facts: &mut Vec<NarratableFact>,
        notices: &mut Vec<ServerNotice>,
    ) -> Vec<LedgerReceiptGroup> {
        let Some(record) = self.replayed.as_ref() else {
            return Vec::new();
        };
        let progress = AnswerProgress::of(record.interaction.status);
        facts.push(NarratableFact::InteractionAlreadyAnswered {
            interaction_id: record.interaction.id,
            option_id: record.interaction.resolved_option_id.clone(),
            progress,
        });
        notices.push(notice_of(
            notice::ALREADY_ANSWERED,
            NoticeSeverity::Info,
            self.runtime.notice_copy.already_answered(progress).clone(),
        ));
        if record.resolution_event_ids.is_empty() {
            return Vec::new();
        }
        self.runtime
            .stores
            .events()
            .get_by_ids(self.account(), &record.resolution_event_ids)
            .await
            .map(|events| turnframe_store::events::group_for_receipts(&events))
            .unwrap_or_default()
    }

    /// What the user said the assistant got wrong, in their words, and each receipt of
    /// the last reply they contested, as it was shown.
    fn disputes(&self) -> (Vec<String>, Vec<String>) {
        let text = self.input.text.as_deref().unwrap_or_default();
        let locale = &self.input.locale;
        let shown: Vec<String> = self
            .previous
            .iter()
            .flat_map(|previous| previous.receipts())
            .map(|receipt| {
                format!(
                    "{}: {}",
                    receipt.title.resolve(locale),
                    receipt.body.resolve(locale)
                )
            })
            .collect();
        let disputes = self
            .record
            .understanding
            .iter()
            .flat_map(|understanding| &understanding.disputes);
        let words = disputes
            .clone()
            .filter_map(|dispute| text.get(dispute.words.start..dispute.words.end))
            .map(str::to_owned)
            .collect();
        // A receipt is keyed `r1`, `r2`… in the order the last reply showed it.
        let contested = disputes
            .filter_map(|dispute| {
                dispute
                    .receipt
                    .as_deref()?
                    .strip_prefix('r')?
                    .parse::<usize>()
                    .ok()
            })
            .filter_map(|position| shown.get(position.checked_sub(1)?).cloned())
            .collect();
        (words, contested)
    }

    /// The workflows this turn cannot start, with the reason each declared.
    fn unavailable(&self) -> Vec<UnavailableWorkflow> {
        unmet_preconditions(&self.runtime.workflows.definitions(), &self.cases)
            .into_iter()
            .map(|(workflow, reason)| UnavailableWorkflow {
                workflow,
                reason: reason.resolve(&self.input.locale).to_owned(),
            })
            .collect()
    }

    fn observe_composition(&self, turn: &AssistantTurn) {
        let observer = self.runtime.observer.as_ref();
        for block in &turn.blocks {
            match block {
                ResponseBlock::Receipt(_) => {
                    observer.observe_labeled(&Signal::ClaimReceiptEmitted, &SignalLabels::none());
                }
                ResponseBlock::Answer(answer) => {
                    let signal = if answer.status == AnswerStatus::Answered {
                        Signal::QuestionAnswered
                    } else {
                        Signal::QuestionUnanswered
                    };
                    observer.observe_labeled(&signal, &SignalLabels::none());
                }
                _ => {}
            }
        }
    }
}

/// What the reply asks for, recorded so the next turn reads its answer (§6.8): each act
/// that is waiting for a value.
fn expectations(plan: &ReductionPlan) -> Vec<Expectation> {
    plan.acts
        .iter()
        .filter_map(|planned| {
            let PlannedActResult::NeedsValue { arguments, .. } = &planned.result else {
                return None;
            };
            Some(Expectation::AwaitingValue {
                act: Box::new(planned.act.clone()),
                case_ref: planned
                    .target
                    .as_ref()
                    .and_then(TargetResolution::exact)
                    .cloned(),
                missing: arguments.clone(),
            })
        })
        .collect()
}

/// The acts the plan made ready on a record whose commands committed: what the next message
/// may correct.
fn done(plan: &ReductionPlan, execution: &ExecutionReport) -> Vec<DoneAct> {
    plan.acts
        .iter()
        .filter(|planned| matches!(planned.result, PlannedActResult::ReadyToExecute { .. }))
        .filter_map(|planned| {
            let case_ref = planned.target.as_ref().and_then(TargetResolution::exact)?;
            let committed = execution.outcomes.iter().any(|record| {
                record.case_ref.key() == case_ref.key()
                    && matches!(record.outcome, CommandOutcome::Committed { .. })
            });
            committed.then(|| DoneAct {
                act: Box::new(planned.act.clone()),
                case_ref: case_ref.clone(),
            })
        })
        .collect()
}

/// How many acts waiting for a record the user named a reply carries, the newest first.
const STILL_WAITING: usize = 3;

/// Acts of earlier turns still waiting for a record the user named that did not exist:
/// those this turn neither did nor left waiting again, carried to the next reply. One
/// whose record is now `listed` under that name was left undone by the turn that brought
/// the record, and is let go.
fn still_waiting(
    previous: Option<&AssistantTurn>,
    plan: &ReductionPlan,
    now: &[Expectation],
    listed: &[CaseLabel],
) -> Vec<Expectation> {
    use turnframe_core::understanding::{ArgumentValue, RecordValue, UnderstoodAct};
    let words = |text: &str| {
        text.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let names_a_record = |act: &UnderstoodAct, missing: &[String]| {
        missing.iter().any(
            |name| match act.arguments.get(name).map(|argument| &argument.value) {
                Some(ArgumentValue::Record(RecordValue::Named { workflow, named })) => {
                    !listed.iter().any(|case| {
                        case.case_ref.workflow == *workflow && words(&case.label) == words(named)
                    })
                }
                _ => false,
            },
        )
    };
    let same = |act: &UnderstoodAct,
                case_ref: Option<&CaseRef>,
                other: &UnderstoodAct,
                other_ref: Option<&CaseRef>| {
        act.operation() == other.operation()
            && case_ref.map(CaseRef::key) == other_ref.map(CaseRef::key)
    };
    let done = |act: &UnderstoodAct, case_ref: Option<&CaseRef>| {
        plan.acts.iter().any(|planned| {
            matches!(planned.result, PlannedActResult::ReadyToExecute { .. })
                && planned.act.operation() == act.operation()
                && case_ref.is_none_or(|case_ref| {
                    planned
                        .target
                        .as_ref()
                        .and_then(TargetResolution::exact)
                        .is_some_and(|aimed| aimed.key() == case_ref.key())
                })
        })
    };
    let waiting_now: Vec<(&UnderstoodAct, Option<&CaseRef>)> = now
        .iter()
        .filter_map(|expectation| match expectation {
            Expectation::AwaitingValue { act, case_ref, .. } => Some((&**act, case_ref.as_ref())),
            _ => None,
        })
        .collect();
    let mut carried: Vec<Expectation> = Vec::new();
    let earlier = previous.map_or(&[][..], |previous| previous.expectations.as_slice());
    for expectation in earlier {
        let (Expectation::AwaitingValue {
            act,
            case_ref,
            missing,
        }
        | Expectation::StillWaiting {
            act,
            case_ref,
            missing,
        }) = expectation
        else {
            continue;
        };
        let case_ref = case_ref.as_ref();
        let already = |list: &[(&UnderstoodAct, Option<&CaseRef>)]| {
            list.iter()
                .any(|(other, other_ref)| same(act, case_ref, other, *other_ref))
        };
        let kept: Vec<(&UnderstoodAct, Option<&CaseRef>)> = carried
            .iter()
            .filter_map(|expectation| match expectation {
                Expectation::StillWaiting { act, case_ref, .. } => {
                    Some((&**act, case_ref.as_ref()))
                }
                _ => None,
            })
            .collect();
        if !names_a_record(act, missing)
            || done(act, case_ref)
            || already(&waiting_now)
            || already(&kept)
        {
            continue;
        }
        carried.push(Expectation::StillWaiting {
            act: act.clone(),
            case_ref: case_ref.cloned(),
            missing: missing.clone(),
        });
    }
    carried.truncate(STILL_WAITING);
    carried
}

/// The cases an act of the turn reached, in act order. A refused act counts: what its
/// record still needs is the way forward.
fn touched(plan: &ReductionPlan) -> Vec<CaseKey> {
    let mut touched: Vec<CaseKey> = Vec::new();
    for planned in &plan.acts {
        if matches!(planned.result, PlannedActResult::SupersededByCorrection) {
            continue;
        }
        if let Some(key) = planned
            .target
            .as_ref()
            .and_then(TargetResolution::exact)
            .map(CaseRef::key)
            && !touched.contains(&key)
        {
            touched.push(key);
        }
    }
    touched
}

/// The records the turn's questions about records were answered from; with `named`, only
/// a question answered from one record. A question about what can be done, or about the
/// values a field takes, is answered by proposing and leaves the ask alone.
fn asked_about(plan: &ReductionPlan, named: bool) -> Vec<CaseKey> {
    let mut keys: Vec<CaseKey> = Vec::new();
    let about_records = plan.answer_tasks.iter().filter(|task| {
        // The values one record's field may take are about that record, however read.
        let values_of_one = !task.enumerations.is_empty() && task.case_refs.len() == 1;
        task.capabilities.is_empty()
            && (values_of_one
                || (task.enumerations.is_empty()
                    && task.basis != turnframe_core::plan::AnswerBasis::GeneralDomainKnowledge))
            && (!named || task.case_refs.len() == 1)
    });
    for key in about_records.flat_map(|task| task.case_refs.iter().map(CaseRef::key)) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// The workflows a start of the turn opened without writing: the record is the
/// conversation's subject, and nothing reports it.
fn started(plan: &ReductionPlan) -> Vec<turnframe_core::ids::WorkflowKey> {
    plan.acts
        .iter()
        .filter(|planned| matches!(planned.result, PlannedActResult::NoChange))
        .filter_map(|planned| match &planned.act.action {
            turnframe_core::understanding::ActAction::Start { workflow } => Some(workflow.clone()),
            turnframe_core::understanding::ActAction::Apply { .. } => None,
        })
        .collect()
}

/// Two spends of one turn, as one report.
fn spent_together(
    first: turnframe_core::replay::BudgetReport,
    second: &turnframe_core::replay::BudgetReport,
) -> turnframe_core::replay::BudgetReport {
    let mut spent = first;
    spent.model_calls += second.model_calls;
    spent.prompt_tokens += second.prompt_tokens;
    spent.max_depth = spent.max_depth.max(second.max_depth);
    if spent.exhausted.is_none() {
        spent.exhausted.clone_from(&second.exhausted);
    }
    spent
}

/// Whether the domain would take `step` on the case now (I22): the view offers its operation
/// and, when its arguments are complete, the act compiles to commands that all validate. A step
/// still missing values is offered on the operation alone; they are asked when it is taken up.
fn offerable(
    definition: &dyn turnframe_core::flow::ErasedWorkflow,
    case_ref: &CaseRef,
    state: Option<&serde_json::Value>,
    step: &NextStep,
) -> bool {
    let Ok(offered) = definition.operations(case_ref.clone(), state) else {
        return false;
    };
    let Some(spec) = offered
        .iter()
        .find(|spec| spec.key == step.operation && spec.availability.is_proposable())
    else {
        return false;
    };
    // Values asked when the step is taken up are tried from the operation's own examples:
    // with none that completes them, the step is offered on the operation alone.
    let known = serde_json::Value::Object(step.arguments.clone());
    let arguments = if spec.check_arguments(&known).is_ok() {
        known
    } else {
        let completed = spec.examples.iter().find_map(|example| {
            let mut arguments = example.arguments.clone();
            arguments.extend(step.arguments.clone());
            let arguments = serde_json::Value::Object(arguments);
            spec.check_arguments(&arguments)
                .is_ok()
                .then_some(arguments)
        });
        match completed {
            Some(arguments) => arguments,
            None => return true,
        }
    };
    let act = turnframe_core::target::ResolvedAct {
        act: turnframe_core::understanding::ActId::new(turnframe_core::understanding::UnitId(0), 0),
        kind: turnframe_core::target::ResolvedActKind::ApplyOperation {
            operation: step.operation.clone(),
        },
        case_ref: case_ref.clone(),
        arguments,
        evidence_digest: turnframe_core::hash::Digest::of_bytes(b"turnframe.offer"),
    };
    definition
        .compile_act(case_ref.clone(), state, &act)
        .is_ok_and(|commands| {
            !commands.is_empty()
                && commands
                    .iter()
                    .all(|command| definition.validate_command(state, command).is_ok())
        })
}
