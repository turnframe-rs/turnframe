//! Steps G–K: what the message asks, the acts a card answer stands for, and the
//! reduction of both.

use turnframe_core::command::ResolutionChannel;
use turnframe_core::error::{InteractionError, OrchestratorError};
use turnframe_core::ids::BlockId;
use turnframe_core::interaction::{InteractionRejection, StoredInteractionAction};
use turnframe_core::observe::{Signal, SignalLabels};
use turnframe_core::operation::OperationCatalog;
use turnframe_core::plan::TargetPolicy;
use turnframe_core::reduce::{PlannedActResult, ReductionPlan};
use turnframe_core::response::{NarratableFact, NoticeSeverity, ServerNotice};
use turnframe_core::target::TargetResolution;
use turnframe_core::understanding::Understanding;

use super::Session;
use crate::execute::ExecutionReport;
use crate::interactions::{AcceptedInteraction, ResponseAdmission, ResponseContext};
use crate::orchestrator::CaseCandidate;
use crate::reduce::{ReducedTurn, rejection};
use crate::resolve::TargetResolver;
use crate::resume::DependentAct;
use crate::turn::{
    aim_at_found, card_act_of, fill_found_arguments, reduction_context, target_resolution_records,
    turn_reducer, typed_response, unlisted, unlisted_arguments,
};

impl Session<'_> {
    /// Step G. The steps stream as [`TurnEvent::Step`](crate::stream::TurnEvent::Step)
    /// while they happen; a turn with no text costs no model call.
    pub(super) async fn understand(
        &mut self,
        resolver: &TargetResolver,
        operations: &OperationCatalog,
        answered: bool,
    ) -> Result<Understanding, OrchestratorError> {
        let definitions = self.runtime.workflows.definitions();
        let card = self
            .open_interactions
            .iter()
            .find(|interaction| interaction.blocking)
            .filter(|_| !answered);
        let summary = card.map(crate::interactions::summarize);
        let text = self.input.text.as_deref().unwrap_or_default();
        let sources = crate::understand::Sources {
            turn: self.input.turn_id,
            definitions: &definitions,
            cases: &self.cases,
            resolver,
            text,
            locale: &self.input.locale,
            today: self.now.date_naive(),
            recent: &self.recent,
            previous: self.previous.as_ref(),
            card: card.zip(summary.as_ref()),
            typed_answers_allowed: self.runtime.policy.allow_text_resolution_for_low_risk,
            config: &self.runtime.config.understanding,
            effort: &self.effort,
        };
        // Each step is also said in the user's language, beside the understanding, when
        // the deployment asked for it.
        let narration = &self.runtime.config.narration;
        let (said, to_say) = futures::channel::mpsc::unbounded();
        let steps = Steps {
            publisher: self.publisher,
            trace: self.runtime.trace.as_deref(),
            turn: self.input.turn_id,
            said: (narration.enabled && narration.steps && self.effort.steps).then_some(said),
        };
        let understander = self.runtime.understander.as_ref();
        let understanding = async move {
            let understood =
                crate::understand::run(understander, &sources, operations, &steps).await;
            drop(steps);
            understood
        };
        let saying = self.runtime.composer.say_steps(
            to_say,
            &self.input.locale,
            self.input.turn_id,
            self.publisher,
        );
        let (understood, said_tasks) = futures::join!(understanding, saying);
        let understood = understood?;
        self.record.tasks = understood.tasks;
        self.record.tasks.extend(said_tasks);
        self.record.budget = Some(understood.budget);
        Ok(understood.understanding)
    }

    /// Looks up the records the message named and the turn did not have (spec §12.3),
    /// and loads what the directory finds. Returns whether it loaded any.
    pub(super) async fn find_unlisted(
        &mut self,
        understanding: &mut Understanding,
        resolver: &mut TargetResolver,
        answered: Option<&AcceptedInteraction>,
        operations: &OperationCatalog,
    ) -> Result<bool, OrchestratorError> {
        let text = self.input.text.clone().unwrap_or_default();
        let limit = self.runtime.config.interaction.max_selection_candidates;
        let mut found = std::collections::BTreeMap::new();
        for (act, workflow, named) in unlisted(understanding, &text) {
            let candidates = self
                .runtime
                .directory
                .find(
                    &self.input.actor,
                    &self.input.conversation_id,
                    &workflow,
                    named.as_deref(),
                )
                .await
                .map_err(OrchestratorError::Store)?;
            let mut keys = Vec::new();
            for candidate in candidates.into_iter().take(limit) {
                keys.push(candidate.key.clone());
                self.load_candidate(candidate, false).await?;
            }
            found.insert(act, keys);
        }
        let arguments = self.find_named_arguments(understanding, operations).await?;
        if found.values().all(Vec::is_empty) && arguments.values().all(|(keys, _)| keys.len() != 1)
        {
            fill_found_arguments(understanding, &arguments, resolver);
            return Ok(false);
        }
        *resolver = self.resolver(answered);
        aim_at_found(understanding, &found, resolver);
        fill_found_arguments(understanding, &arguments, resolver);
        Ok(true)
    }

    /// Looks up each record argument that names a record not in view, loading the one
    /// found; with none or several, the reason the value is asked for again.
    async fn find_named_arguments(
        &mut self,
        understanding: &Understanding,
        operations: &OperationCatalog,
    ) -> Result<
        std::collections::BTreeMap<
            (turnframe_core::understanding::ActId, String),
            (Vec<turnframe_core::case::CaseKey>, String),
        >,
        OrchestratorError,
    > {
        let mut found = std::collections::BTreeMap::new();
        for (act, name, workflow, named) in unlisted_arguments(understanding) {
            let candidates = self
                .runtime
                .directory
                .find(
                    &self.input.actor,
                    &self.input.conversation_id,
                    &workflow,
                    Some(&named),
                )
                .await
                .map_err(OrchestratorError::Store)?;
            let copy = &self.runtime.notice_copy;
            // One that can be registered now is offered, not only asked for again.
            let can_open = operations.iter().any(|spec| {
                spec.workflow == workflow
                    && matches!(
                        spec.target_policy,
                        TargetPolicy::NewCaseOnly | TargetPolicy::AllowsNewCase
                    )
            });
            let template = match (candidates.is_empty(), can_open) {
                (true, true) => &copy.record_not_found_yet,
                (true, false) => &copy.record_not_found,
                (false, _) => &copy.record_not_unique,
            };
            // The workflow's own word for one of its records, in the user's language.
            let noun = self
                .runtime
                .workflows
                .definitions()
                .get(&workflow)
                .and_then(|definition| definition.noun())
                .map_or_else(
                    || workflow.to_string(),
                    |noun| noun.resolve(&self.input.locale).to_owned(),
                );
            let reason = template
                .resolve(&self.input.locale)
                .replace("{workflow}", &noun)
                .replace("{named}", &named);
            let keys: Vec<_> = candidates
                .iter()
                .map(|candidate| candidate.key.clone())
                .collect();
            if let [only] = candidates.as_slice() {
                self.load_candidate(only.clone(), false).await?;
            }
            found.insert((act, name), (keys, reason));
        }
        Ok(found)
    }

    /// Step C for a typed answer to the card on screen: admitted like a click, on the
    /// model-interpreted channel, which authorizes only what needs no confirmation. A
    /// card that refuses it gets a notice, and the rest of the message goes on.
    pub(super) async fn admit_typed_answer(
        &mut self,
        understanding: &Understanding,
    ) -> Result<Option<AcceptedInteraction>, OrchestratorError> {
        let Some(typed) = understanding.card_answer.as_ref() else {
            return Ok(None);
        };
        let Some(card) = self
            .open_interactions
            .iter()
            .find(|interaction| interaction.blocking)
            .cloned()
        else {
            return Ok(None);
        };
        let current = self
            .cases
            .get(&card.case_ref.key())
            .map_or(card.case_ref.expected_revision, |case| {
                case.case_ref.expected_revision
            });
        let context = ResponseContext::click(
            &self.input.actor,
            &self.input.conversation_id,
            self.input.turn_id,
            current,
            self.now,
        )
        .through(ResolutionChannel::ModelInterpreted);
        let response = typed_response(&card, &typed.option);
        match self.runtime.interactions.accept(context, &response).await {
            Ok(ResponseAdmission::Accepted(accepted)) => {
                self.resolving_card = Some(accepted.interaction_id());
                Ok(Some(*accepted))
            }
            Ok(ResponseAdmission::AlreadyAnswered(record)) => {
                self.replayed = Some(record);
                Ok(None)
            }
            Err(InteractionError::Rejected(refused)) => {
                self.refuse_typed_answer(&refused);
                Ok(None)
            }
            Err(error) => Err(self.observed_rejection(error)),
        }
    }

    fn refuse_typed_answer(&mut self, refused: &InteractionRejection) {
        let code = match refused {
            InteractionRejection::ChannelNotAllowed { .. } => {
                rejection::TEXT_RESOLUTION_NOT_ALLOWED
            }
            InteractionRejection::UnknownOption => rejection::UNKNOWN_OPTION,
            _ => rejection::NO_ACTIVE_INTERACTION,
        };
        if let Some((code, text)) = self.runtime.notice_copy.runtime_refusal(code) {
            self.early_notices.push(ServerNotice {
                block_id: BlockId::from(format!("notice:{code}")),
                code: code.to_owned(),
                severity: NoticeSeverity::Info,
                text: text.clone(),
            });
        }
    }

    /// Commits a confirmation whose card carries dependent acts before they are
    /// reduced, then reads the cases again: the dependents compile against what the
    /// confirmed commands made (spec §6.7).
    pub(super) async fn settle_prerequisite(
        &mut self,
        answered: Option<&AcceptedInteraction>,
    ) -> Result<Option<ExecutionReport>, OrchestratorError> {
        let Some(accepted) = answered else {
            return Ok(None);
        };
        if DependentAct::of(&accepted.record.interaction.payload).is_empty() {
            return Ok(None);
        }
        let batches = self.confirmed_batches(Some(accepted)).await?;
        if batches.is_empty() {
            return Ok(None);
        }
        let execution = self
            .runtime
            .executor
            .execute(self.account(), &batches, self.now)
            .await?;
        self.observe_execution(&execution);
        self.commit(&execution, Some(accepted)).await?;
        self.committed = true;
        let key = accepted.record.interaction.case_ref.key();
        self.confirmed_case = self.cases.get(&key).map(|case| {
            let mut candidate = CaseCandidate::new(key.clone(), case.label.clone());
            candidate.confirm_every_write = case.terms.confirm_every_write;
            candidate.subject_only_when_named = case.terms.subject_only_when_named;
            candidate
        });
        self.reload_cases().await?;
        Ok(Some(execution))
    }

    /// The acts a card answer stands for, placed before the message's own: the card's
    /// own act, then the acts its confirmation lets run (spec §13.3, §15.3, I7).
    pub(super) fn with_card_acts(
        &mut self,
        understanding: Understanding,
        answered: Option<&AcceptedInteraction>,
        resolver: &TargetResolver,
    ) -> Understanding {
        let with_card = card_act_of(understanding, answered, resolver);
        self.card_act = with_card.card_act;
        if with_card.declined {
            self.declined_instruction = true;
            self.declined_fact = answered.map(|accepted| self.declined(accepted));
        }
        let mut understanding = with_card.understanding;
        if let Some(accepted) = answered
            && matches!(
                accepted.response.action,
                StoredInteractionAction::ConfirmCommands { .. }
            )
        {
            let dependents = crate::resume::dependents(
                &accepted.record.interaction.payload,
                resolver.token_map(),
            );
            let at = usize::from(with_card.card_act.is_some());
            understanding.acts.splice(at..at, dependents);
        }
        self.take_values_the_obligation_fixes(&mut understanding, resolver);
        // The record keeps what was reduced: the message's reading and the card's acts.
        if self
            .input
            .text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
            || !understanding.acts.is_empty()
        {
            self.record.plan_hash = understanding.hash().ok();
            self.record.understanding = Some(understanding.clone());
        }
        understanding
    }

    /// An act waiting for values that the one open obligation of its record answered by
    /// the same operation fixes takes them from it: the record says which, so nobody is
    /// asked. With two such obligations the record does not say, and the value is asked.
    fn take_values_the_obligation_fixes(
        &self,
        understanding: &mut Understanding,
        resolver: &TargetResolver,
    ) {
        use turnframe_core::understanding::{
            ActStatus, ActTarget, ArgumentValue, UnderstoodArgument,
        };
        for act in &mut understanding.acts {
            let ActStatus::NeedsValue { arguments, reason } = &act.status else {
                continue;
            };
            let (Some(operation), ActTarget::Record { token }) = (act.operation(), &act.target)
            else {
                continue;
            };
            let Some(case) = resolver
                .token_map()
                .get(token)
                .and_then(|entry| self.cases.get(&entry.case_ref.key()))
            else {
                continue;
            };
            let answering: Vec<_> = case
                .view
                .obligations
                .iter()
                .filter_map(|obligation| obligation.act.as_ref())
                .filter(|answer| &answer.operation == operation)
                .collect();
            let [only] = answering.as_slice() else {
                continue;
            };
            let still: Vec<String> = arguments
                .iter()
                .filter(|name| !only.given.contains_key(*name))
                .cloned()
                .collect();
            if still.len() == arguments.len() {
                continue;
            }
            let reason = reason.clone();
            for (name, value) in &only.given {
                if arguments.contains(name) {
                    act.arguments
                        .entry(name.clone())
                        .or_insert_with(|| UnderstoodArgument {
                            value: ArgumentValue::Json(value.clone()),
                            excerpt: None,
                        });
                }
            }
            act.status = if still.is_empty() {
                ActStatus::Ready
            } else {
                ActStatus::NeedsValue {
                    arguments: still,
                    reason,
                }
            };
        }
    }

    /// The decline, as a fact carrying the card's own question and the option chosen.
    fn declined(&self, accepted: &AcceptedInteraction) -> NarratableFact {
        let payload = &accepted.record.interaction.payload;
        let locale = &self.input.locale;
        NarratableFact::InstructionDeclined {
            case_ref: accepted.response.case_ref.clone(),
            interaction_id: accepted.response.interaction_id,
            option_id: accepted.response.option_id.clone(),
            question: payload.title.resolve(locale).to_owned(),
            option_label: payload
                .options
                .iter()
                .find(|option| option.id == accepted.response.option_id)
                .map(|option| option.label.resolve(locale).to_owned())
                .unwrap_or_default(),
        }
    }

    /// Steps J and K.
    pub(super) fn reduce(
        &mut self,
        understanding: &Understanding,
        resolver: &TargetResolver,
        operations: &OperationCatalog,
        answered: Option<&AcceptedInteraction>,
    ) -> Result<ReducedTurn, OrchestratorError> {
        let definitions = self.runtime.workflows.definitions();
        let context = reduction_context(
            &self.cases,
            &self.open_interactions,
            resolver,
            operations,
            &self.runtime.policy,
            self.runtime.config.understanding.plan_limits,
            self.now,
        );
        let reducer = turn_reducer(
            &definitions,
            &self.cases,
            resolver,
            &self.runtime.policy_engine,
            &self.runtime.config,
            answered,
            self.card_act,
        )
        .with_copy(self.runtime.notice_copy.clone());
        let stage = crate::signals::Stage::enter();
        let reduced = reducer.reduce_turn(&self.input, understanding, &context)?;
        stage.observe(
            self.runtime.observer.as_ref(),
            Signal::ReductionDuration,
            &SignalLabels::none(),
        );
        self.observe_reduction(&reduced.plan);
        Ok(reduced)
    }

    fn observe_reduction(&mut self, plan: &ReductionPlan) {
        let observer = self.runtime.observer.as_ref();
        self.record.act_outcomes = plan
            .acts
            .iter()
            .map(|act| act.result.name().to_owned())
            .collect();
        self.record.reduction_plan_hash = Some(plan.plan_hash.clone());
        self.record.policy_decisions = plan.policy_decisions.clone();
        self.record.target_resolutions = target_resolution_records(plan);
        for _ in &plan.refusals {
            observer.observe_labeled(&Signal::ActRefused, &SignalLabels::none());
        }
        for operation in &plan.superseded_operations {
            observer.observe_labeled(
                &Signal::ActSuperseded,
                &SignalLabels::none().with_operation(operation.clone()),
            );
        }
        for planned in &plan.acts {
            match &planned.target {
                Some(TargetResolution::Ambiguous { .. }) => {
                    observer.observe_labeled(&Signal::TargetAmbiguous, &SignalLabels::none());
                }
                Some(TargetResolution::Missing) => {
                    observer.observe_labeled(&Signal::TargetMissing, &SignalLabels::none());
                }
                Some(_) => {}
                // An act whose target resolved nowhere leaves no other trace.
                None => {
                    if let Some(code) = unresolved_target_code(&planned.result) {
                        observer.observe_labeled(
                            &Signal::TargetUnresolved,
                            &SignalLabels::none().with_error_code(code),
                        );
                    }
                }
            }
            if matches!(
                planned.result,
                PlannedActResult::AwaitingConfirmation { .. }
            ) {
                observer
                    .observe_labeled(&Signal::CommandConfirmationRequired, &SignalLabels::none());
            }
        }
    }
}

/// The rejection code of an act whose target nothing could resolve.
fn unresolved_target_code(result: &PlannedActResult) -> Option<&'static str> {
    let PlannedActResult::Rejected { rejection } = result else {
        return None;
    };
    [
        rejection::TARGET_POLICY_MISMATCH,
        rejection::TARGET_UNAUTHORIZED,
        rejection::TARGET_MISSING,
        rejection::TARGET_STALE,
        rejection::TARGET_UNRESOLVED,
        rejection::NO_ACTIVE_INTERACTION,
    ]
    .into_iter()
    .find(|code| *code == rejection.code.as_str())
}

/// Understanding's steps, streamed on the turn and reported to the trace.
struct Steps<'a> {
    publisher: &'a crate::stream::TurnPublisher,
    trace: Option<&'a dyn crate::trace::TurnTrace>,
    turn: turnframe_core::ids::TurnId,
    /// Where a step goes to be said, when steps are said.
    said: Option<futures::channel::mpsc::UnboundedSender<turnframe_understand::Step>>,
}

impl turnframe_understand::StepSink for Steps<'_> {
    fn step(&self, step: turnframe_understand::Step) {
        if let Some(trace) = self.trace {
            trace.event(&crate::trace::TraceEvent::Step {
                turn: self.turn,
                step: &step,
            });
        }
        if let Some(said) = &self.said {
            let _ = said.unbounded_send(step.clone());
        }
        self.publisher.publish_step(step);
    }
}
