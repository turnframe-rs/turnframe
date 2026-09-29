//! Steps A–F: the turn is accepted, its cases loaded and projected, a click judged
//! against the revision just read, and the conversation read back.

use std::sync::Arc;

use turnframe_core::error::{AuthorizationError, InteractionError, OrchestratorError, StoreError};
use turnframe_core::interaction::InteractionRejection;
use turnframe_core::observe::{Signal, SignalLabels};
use turnframe_core::replay::WorkflowVersionRecord;
use turnframe_core::response::ResponseBlock;
use turnframe_store::conversation::StoredUserTurn;

use super::Session;
use crate::attachments::TurnAttachments;
use crate::interactions::{AcceptedInteraction, ResponseAdmission, ResponseContext};
use crate::orchestrator::CaseCandidate;
use crate::resolve::TargetResolver;
use crate::turn::{
    DirectoryTerms, addressable_cards, admit_card_case, blocking_summary, build_resolver,
    candidates_of_open_cards, project_case, recent_messages, unaddressable_card,
};

impl Session<'_> {
    /// Step B: the conversation belongs to the actor, the user turn is stored, and
    /// the open cards are read.
    pub(super) async fn accept(&mut self) -> Result<(), OrchestratorError> {
        let conversations = self.runtime.stores.conversations();
        conversations
            .load_conversation(self.account(), &self.input.conversation_id)
            .await
            .map_err(|error| match error {
                // Another tenant's conversation and a missing one get the same answer.
                StoreError::NotFound => {
                    OrchestratorError::Unauthorized(AuthorizationError::ConversationNotAccessible {
                        conversation_id: self.input.conversation_id,
                    })
                }
                other => OrchestratorError::Store(other),
            })?;
        match conversations
            .append_user_turn(StoredUserTurn::new(self.input.clone(), self.now))
            .await
        {
            // The same turn arriving twice is a retry, not a second turn.
            Ok(()) | Err(StoreError::Conflict) => {}
            Err(error) => return Err(OrchestratorError::Store(error)),
        }
        self.runtime
            .stores
            .replay()
            .put(self.record.clone())
            .await
            .map_err(OrchestratorError::Store)?;
        self.open_interactions = self
            .runtime
            .interactions
            .open_for_conversation(self.account(), &self.input.conversation_id)
            .await
            .map_err(OrchestratorError::Interaction)?;
        Ok(())
    }

    /// Steps D and E: each candidate at its current revision, projected. A view that
    /// breaks a §8.4 invariant stops the turn (I2).
    pub(super) async fn load_cases(&mut self) -> Result<(), OrchestratorError> {
        let directory = Arc::clone(&self.runtime.directory);
        let mut candidates = directory
            .candidates(&self.input.actor, &self.input.conversation_id)
            .await
            .map_err(OrchestratorError::Store)?;
        if let Some(origin) = self.input.origin.as_ref()
            && let Some(candidate) = directory
                .resolve_origin(&self.input.actor, &origin.origin_token)
                .await
                .map_err(OrchestratorError::Store)?
        {
            self.origin_case = Some(candidate.key.clone());
            candidates.push(candidate);
        }
        // The user confirmed its creation on this very turn.
        if let Some(confirmed) = self.confirmed_case.clone()
            && !candidates
                .iter()
                .any(|candidate| candidate.key == confirmed.key)
        {
            candidates.push(confirmed);
        }
        // Open cards propose their cases; the directory still decides (§12.3, §25.4).
        let from_cards = candidates_of_open_cards(&candidates, &self.open_interactions);
        let proposed = candidates
            .into_iter()
            .map(|candidate| (candidate, false))
            .chain(from_cards.into_iter().map(|candidate| (candidate, true)));
        for (candidate, from_card) in proposed {
            self.load_candidate(candidate, from_card).await?;
        }
        // A card whose case the directory refused is not part of this turn.
        self.open_interactions =
            addressable_cards(&self.cases, std::mem::take(&mut self.open_interactions));
        Ok(())
    }

    /// Loads one candidate at its current revision and projects it. A candidate an
    /// open card proposed is admitted by the directory first; one already loaded, or
    /// of a workflow this runtime does not host, is skipped.
    pub(super) async fn load_candidate(
        &mut self,
        candidate: CaseCandidate,
        from_card: bool,
    ) -> Result<(), OrchestratorError> {
        if self.cases.contains_key(&candidate.key) {
            return Ok(());
        }
        let Ok(registered) = self.runtime.workflows.require(&candidate.key.workflow) else {
            return Ok(());
        };
        let loaded = registered
            .executor
            .load(self.account(), &candidate.key.case_id)
            .await
            .map_err(OrchestratorError::Store)?;
        let candidate = if from_card {
            let admitted = admit_card_case(
                self.runtime.observer.as_ref(),
                self.runtime.directory.as_ref(),
                &self.input.actor,
                &self.input.conversation_id,
                candidate,
                loaded.value.is_some(),
            )
            .await
            .map_err(OrchestratorError::Store)?;
            let Some(admitted) = admitted else {
                return Ok(());
            };
            admitted
        } else {
            candidate
        };
        let case_ref = candidate.key.clone().at(loaded.revision);
        let terms = DirectoryTerms {
            confirm_every_write: candidate.confirm_every_write,
            subject_only_when_named: candidate.subject_only_when_named,
        };
        let case = project_case(
            self.runtime.observer.as_ref(),
            &registered.definition,
            case_ref.clone(),
            candidate.label,
            loaded.value,
            terms,
        )
        .inspect_err(|error| {
            if matches!(error, OrchestratorError::InvariantViolation(_)) {
                self.runtime.observer.observe_labeled(
                    &Signal::WorkflowInvariantViolation,
                    &SignalLabels::workflow(candidate.key.workflow.clone()),
                );
            }
        })?;
        self.record.loaded_cases.push(case_ref);
        self.record.workflow_versions.push(WorkflowVersionRecord {
            key: registered.key.clone(),
            version: registered.version.clone(),
        });
        self.cases.insert(candidate.key, case);
        Ok(())
    }

    /// Reads and projects every case again, after the turn committed mid-flight.
    pub(super) async fn reload_cases(&mut self) -> Result<(), OrchestratorError> {
        self.cases.clear();
        self.open_interactions = self
            .runtime
            .interactions
            .open_for_conversation(self.account(), &self.input.conversation_id)
            .await
            .map_err(OrchestratorError::Interaction)?;
        self.load_cases().await
    }

    /// Step C: the structured card answer, validated against the revision the case is
    /// actually at.
    pub(super) async fn admit_response(
        &mut self,
    ) -> Result<Option<AcceptedInteraction>, OrchestratorError> {
        let Some(response) = self.input.interaction_response.clone() else {
            return Ok(None);
        };
        let record = self
            .runtime
            .interactions
            .get(self.account(), &response.interaction_id)
            .await
            .map_err(OrchestratorError::Interaction)?;
        if let Some(rejection) = unaddressable_card(&self.cases, &record) {
            return Err(OrchestratorError::Interaction(InteractionError::Rejected(
                rejection,
            )));
        }
        let current = self
            .cases
            .get(&record.interaction.case_ref.key())
            .map_or(record.interaction.case_ref.expected_revision, |case| {
                case.case_ref.expected_revision
            });
        let context = ResponseContext::click(
            &self.input.actor,
            &self.input.conversation_id,
            self.input.turn_id,
            current,
            self.now,
        );
        let admission = self
            .runtime
            .interactions
            .accept(context, &response)
            .await
            .map_err(|error| self.observed_rejection(error))?;
        match admission {
            ResponseAdmission::Accepted(accepted) => {
                self.resolving_card = Some(accepted.interaction_id());
                Ok(Some(*accepted))
            }
            // A second click does not execute again; the record says what happened (I14).
            ResponseAdmission::AlreadyAnswered(record) => {
                self.replayed = Some(record);
                Ok(None)
            }
        }
    }

    pub(super) fn observed_rejection(&self, error: InteractionError) -> OrchestratorError {
        if let InteractionError::Rejected(InteractionRejection::Stale { .. }) = &error {
            self.runtime
                .observer
                .observe_labeled(&Signal::InteractionStale, &SignalLabels::none());
        }
        OrchestratorError::Interaction(error)
    }

    /// The earlier messages, the last reply, the documents already shown and what
    /// the conversation was last about. A store that cannot be read costs the context,
    /// not the turn.
    pub(super) async fn load_conversation(&mut self) {
        let window = self
            .runtime
            .config
            .understanding
            .transcript_turns
            .unwrap_or(usize::MAX);
        let Ok(turns) = self
            .runtime
            .stores
            .conversations()
            .load_recent_turns(self.account(), &self.input.conversation_id, window)
            .await
        else {
            return;
        };
        for assistant in turns.iter().filter_map(|turn| turn.assistant.as_ref()) {
            self.artifacts_shown
                .extend(assistant.blocks.iter().filter_map(|block| match block {
                    ResponseBlock::Artifact(view) => Some(view.block_id.clone()),
                    _ => None,
                }));
            // Oldest first, so the latest turn that had a subject wins.
            if !assistant.subjects.is_empty() {
                self.carried_subjects = assistant.subjects.clone();
            }
        }
        (self.recent, self.previous) = recent_messages(turns, self.input.turn_id);
    }

    /// The turn's files, fetched once for the stage that can be shown them.
    pub(super) async fn gather_attachments(&mut self) {
        let composer = &self.runtime.composer;
        self.attachments = TurnAttachments::gather(
            self.runtime.attachment_source.as_ref(),
            self.runtime.config.attachments,
            &self.input.turn_id,
            &self.input.attachments,
            |part| composer.carries(part),
        )
        .await;
    }

    /// Step F: opaque tokens for the cases the actor may address.
    pub(super) fn resolver(&self, answered: Option<&AcceptedInteraction>) -> TargetResolver {
        build_resolver(
            self.account(),
            self.input.turn_id,
            &self.runtime.workflows.definitions(),
            &self.runtime.case_ids,
            &self.cases,
            self.input
                .origin
                .as_ref()
                .map(|origin| &origin.origin_token)
                .zip(self.origin_case.as_ref()),
            blocking_summary(answered, &self.open_interactions),
        )
    }
}
