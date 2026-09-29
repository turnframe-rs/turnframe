//! Steps L–N: cards that must exist first, execution, and the one commit; and the
//! record of the turn at commit and at the end.

use turnframe_core::command::CommandBatch;
use turnframe_core::error::OrchestratorError;
use turnframe_core::interaction::StoredInteractionAction;
use turnframe_core::observe::{Signal, SignalLabels};
use turnframe_core::replay::{CommandOutcome, TurnPhase};
use turnframe_core::response::AssistantTurn;
use turnframe_store::interaction::{InvalidationReason, ResolutionOutcome};

use super::Session;
use crate::execute::ExecutionReport;
use crate::interactions::{AcceptedInteraction, PersistedInteractions};
use crate::reduce::ReducedTurn;

impl Session<'_> {
    /// Steps L, M and N. `settled` is the confirmation already committed for its
    /// dependents: its commands do not run again and its card is not settled again.
    pub(super) async fn execute(
        &mut self,
        reduced: &ReducedTurn,
        answered: Option<&AcceptedInteraction>,
        settled: Option<ExecutionReport>,
    ) -> Result<(ExecutionReport, PersistedInteractions), OrchestratorError> {
        let persisted = self
            .runtime
            .interactions
            .persist(
                &reduced.plan.pre_execution_interactions,
                self.account(),
                self.input.conversation_id,
                self.input.turn_id,
                self.now,
            )
            .await;
        for interaction in &persisted.created {
            self.record.interactions_created.push(interaction.id);
            self.runtime.observer.observe_labeled(
                &Signal::InteractionCreated,
                &SignalLabels::none().with_interaction(interaction.kind),
            );
        }
        self.runtime
            .executor
            .journal_pending(&reduced.pending, self.now)
            .await?;

        let mut batches = self.confirmed_batches(answered).await?;
        if settled.is_some() {
            batches.clear();
        }
        batches.extend(reduced.plan.batches.iter().cloned());
        // What those writes imply elsewhere, asked once.
        if let Some(consequences) = self.runtime.consequences.as_ref() {
            let following = consequences
                .following(&self.input.actor, &batches)
                .await
                .map_err(OrchestratorError::Store)?;
            batches.extend(following);
        }
        self.publisher.phase_reached(TurnPhase::Executing);
        let mut execution = self
            .runtime
            .executor
            .execute(self.account(), &batches, self.now)
            .await?;
        self.observe_execution(&execution);

        let settling = if settled.is_some() { None } else { answered };
        self.commit(&execution, settling).await?;
        self.committed = true;
        self.publisher.phase_reached(TurnPhase::Committed);
        if let Some(settled) = settled {
            execution.absorb(settled);
        }
        Ok((execution, persisted))
    }

    /// The commands a confirmation click authorized, resumed from the journal
    /// (spec §15.3). A confirmed command runs before the acts that needed it.
    pub(super) async fn confirmed_batches(
        &self,
        answered: Option<&AcceptedInteraction>,
    ) -> Result<Vec<CommandBatch<serde_json::Value>>, OrchestratorError> {
        let Some(accepted) = answered else {
            return Ok(Vec::new());
        };
        let StoredInteractionAction::ConfirmCommands { command_refs } = &accepted.response.action
        else {
            return Ok(Vec::new());
        };
        let Some(origin) = accepted.origin() else {
            return Ok(Vec::new());
        };
        self.runtime
            .executor
            .resume_confirmed(
                self.account(),
                command_refs,
                &origin,
                &self.input.actor,
                self.input.turn_id,
            )
            .await
    }

    /// Step N: journal completions, events, card resolutions, invalidations, outbox
    /// rows, replay record and phase marker, in one write.
    pub(super) async fn commit(
        &mut self,
        execution: &ExecutionReport,
        answered: Option<&AcceptedInteraction>,
    ) -> Result<(), OrchestratorError> {
        let mut bundle = execution.bundle();
        let mut settled = None;
        if let Some(accepted) = answered {
            let outcome = self.resolution_outcome(execution);
            bundle = bundle.with_interaction_finish(accepted.interaction_id(), outcome.clone());
            settled = Some((accepted.record.interaction.clone(), outcome));
        }
        for (case_key, revision) in &execution.changed {
            bundle = bundle.with_invalidation(
                case_key.clone(),
                *revision,
                InvalidationReason::RevisionChanged,
            );
        }
        // Extended: a turn that settled a confirmation first commits twice.
        self.record
            .command_outcomes
            .extend(execution.outcomes.iter().cloned());
        self.record.event_ids.extend(execution.event_ids());
        self.record
            .outbox_ids
            .extend(execution.outbox.iter().map(|entry| entry.outbox_id));
        self.record
            .reconciliation_attempt_ids
            .extend(execution.pending_attempts());
        self.stamp_calls();
        self.record.phase = TurnPhase::Committed;
        self.record.recorded_at = self.now;
        bundle = bundle
            .with_replay_record(self.record.clone())
            .with_turn_phase(self.input.turn_id, TurnPhase::Committed);
        let stage = crate::signals::Stage::enter();
        let written = self.runtime.executor.commit(self.account(), bundle).await;
        stage.observe(
            self.runtime.observer.as_ref(),
            Signal::PersistenceDuration,
            &SignalLabels::none(),
        );
        written?;
        if let Some((card, outcome)) = settled {
            crate::interactions::observe_resolution(
                self.runtime.observer.as_ref(),
                &card,
                &outcome,
            );
        }
        Ok(())
    }

    /// How the answered card is settled (spec §15.5): `Resolved` once what it
    /// authorized committed, otherwise failed or restored as configured.
    fn resolution_outcome(&self, execution: &ExecutionReport) -> ResolutionOutcome {
        if execution.outcomes.is_empty() || execution.any_committed() {
            return ResolutionOutcome::Resolved {
                event_ids: execution.event_ids(),
            };
        }
        if self
            .runtime
            .config
            .interaction
            .restore_card_after_failed_command
        {
            return ResolutionOutcome::RestoreActive;
        }
        ResolutionOutcome::Failed {
            code: first_failure_code(execution),
        }
    }

    /// The prompts a source supplied, stamped at commit and again at the end.
    fn stamp_calls(&mut self) {
        // A task running on its built-in text had no prompt supplied.
        let sourced: Vec<_> = self
            .record
            .tasks
            .iter()
            .filter_map(|task| task.prompt_ref.clone())
            .filter(|reference| {
                reference.version.as_str() != turnframe_tasks::instructions::BUILT_IN_VERSION
            })
            .collect();
        for reference in sourced {
            if !self.record.prompt_refs.contains(&reference) {
                self.record.prompt_refs.push(reference);
            }
        }
    }

    /// Step U: the phase marker, and the replay record as the turn ended.
    pub(super) async fn finish(&mut self, turn: &AssistantTurn) -> Result<(), OrchestratorError> {
        self.record.response_block_ids = turn.block_ids();
        self.record.phase = TurnPhase::Delivered;
        self.record.recorded_at = self.runtime.clock.now();
        self.stamp_calls();
        self.trace(&crate::trace::TraceEvent::Completed {
            reply: turn,
            record: &self.record,
        });
        if self.runtime.config.observability.record_replay {
            self.runtime
                .stores
                .replay()
                .put(self.record.clone())
                .await
                .map_err(OrchestratorError::Store)?;
        }
        self.runtime
            .stores
            .conversations()
            .set_turn_phase(self.account(), &self.input.turn_id, TurnPhase::Delivered)
            .await
            .map_err(OrchestratorError::Store)?;
        Ok(())
    }

    pub(super) fn observe_execution(&self, execution: &ExecutionReport) {
        let observer = self.runtime.observer.as_ref();
        for record in &execution.outcomes {
            let labels = SignalLabels::none();
            match &record.outcome {
                CommandOutcome::Committed { .. } => {
                    observer.observe_labeled(&Signal::CommandExecuted, &labels);
                }
                CommandOutcome::Rejected { code } => observer.observe_labeled(
                    &Signal::CommandRejected,
                    &labels.with_error_code(code.as_str()),
                ),
                CommandOutcome::RevisionConflict { .. } => {
                    observer.observe_labeled(&Signal::CommandRevisionConflict, &labels);
                }
                CommandOutcome::IdempotentReplay => {
                    observer.observe_labeled(&Signal::CommandIdempotencyReplay, &labels);
                }
                CommandOutcome::OutcomeUnknown { .. } => {
                    observer.observe_labeled(&Signal::ExternalOutcomeUnknown, &labels);
                }
                _ => {}
            }
        }
    }
}

/// The stable code of the first command that did not commit.
fn first_failure_code(execution: &ExecutionReport) -> String {
    execution
        .outcomes
        .iter()
        .find_map(|record| match &record.outcome {
            CommandOutcome::Rejected { code } => Some(code.as_str().to_owned()),
            CommandOutcome::Failed { code } => Some(code.clone()),
            CommandOutcome::RevisionConflict { .. } => Some("revision_conflict".to_owned()),
            CommandOutcome::OutcomeUnknown { .. } => Some("outcome_unknown".to_owned()),
            _ => None,
        })
        .unwrap_or_else(|| "not_committed".to_owned())
}
