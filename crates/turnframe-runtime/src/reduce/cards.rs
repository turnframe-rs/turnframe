//! The cards an act raises when it cannot run as asked, and its refusals.

use super::*;

impl Session<'_> {
    pub(super) fn clarify_target(
        &mut self,
        index: usize,
        candidates: &[TargetCandidate],
    ) -> Result<(), ReductionError> {
        // The card belongs to the smallest key, so ownership does not depend on order.
        let Some(owner) = candidates
            .iter()
            .min_by_key(|candidate| candidate.case_ref.key())
            .map(|candidate| candidate.case_ref.clone())
        else {
            return self.reject(index, rejection::TARGET_MISSING);
        };
        let act = self.acts[index].clone();
        let Some(mut spec) = self.reducer.policy.selection_card(
            format!("select_target:{}", act.id),
            &owner,
            self.reducer.copy.select_target_title.clone(),
            self.reducer.copy.select_target_cancel.clone(),
            candidates,
        ) else {
            return self.reject(index, rejection::TARGET_TOO_MANY_CANDIDATES);
        };
        // The card carries the act, so the answer applies it to the case the user picks.
        spec.payload = DeferredAct::unbound(act).attach_to(spec.payload);
        self.needs_clarification(index, spec)
    }

    /// The card an [`ConstraintKind::ApplyOnlyIf`] raises: "yes" runs the act it
    /// carries, "no" records that it was declined (§13.3).
    pub(super) fn condition_card(
        &self,
        index: usize,
        case_ref: &CaseRef,
        risk: RiskClass,
    ) -> InteractionSpec {
        let copy = &self.reducer.copy;
        let act = &self.acts[index];
        let payload = InteractionPayload::new(copy.condition_title.clone())
            .with_option(
                InteractionOption::new(
                    OptionId::from("yes"),
                    copy.condition_yes.clone(),
                    StoredInteractionAction::ResolveClarification {
                        answer_key: CONDITION_HOLDS_ANSWER.to_owned(),
                    },
                )
                .with_style(OptionStyle::Primary),
            )
            .with_option(InteractionOption::new(
                OptionId::from("no"),
                copy.condition_no.clone(),
                StoredInteractionAction::Dismiss,
            ));
        let payload = DeferredAct::on(act.clone(), case_ref.clone()).attach_to(payload);
        InteractionSpec::new(
            format!("condition:{}", act.id),
            case_ref.clone(),
            InteractionKind::Boolean,
            payload,
        )
        .with_confirms_risk(risk)
        .with_text_resolution(TextResolutionPolicy::Never)
    }

    pub(super) fn needs_clarification(
        &mut self,
        index: usize,
        spec: InteractionSpec,
    ) -> Result<(), ReductionError> {
        self.push_spec(spec.clone());
        self.results[index] = Some(PlannedActResult::NeedsClarification {
            interaction_spec: spec,
        });
        Ok(())
    }

    pub(super) fn push_spec(&mut self, spec: InteractionSpec) {
        if !self.specs.iter().any(|existing| existing.key == spec.key) {
            self.specs.push(spec);
        }
    }

    /// Refuses an act for a reason the runtime decided; it is reported like any other.
    pub(super) fn reject(&mut self, index: usize, code: &str) -> Result<(), ReductionError> {
        self.reject_with(index, code, None)
    }

    /// The same, carrying the workflow's own sentence when it wrote one.
    pub(super) fn reject_with(
        &mut self,
        index: usize,
        code: &str,
        explanation: Option<LocalizedText>,
    ) -> Result<(), ReductionError> {
        let mut rejection = DomainRejection::new(RejectionCode::from(code), code);
        if let Some(explanation) = explanation {
            rejection = rejection.with_explanation(explanation);
        }
        self.note_refusal(None, &rejection);
        self.results[index] = Some(PlannedActResult::Rejected { rejection });
        Ok(())
    }

    /// The reason the first unmet start precondition of `workflow` gave, if any.
    pub(super) fn unmet_precondition(
        &self,
        workflow: &turnframe_core::ids::WorkflowKey,
    ) -> Option<LocalizedText> {
        let definition = self.reducer.workflows.require(workflow).ok()?;
        definition
            .start_preconditions()
            .into_iter()
            .find(|precondition| {
                !self
                    .context
                    .views
                    .values()
                    .any(|view| precondition.satisfied_by(view))
            })
            .map(|precondition| precondition.reason)
    }

    /// Whether `case_ref` may be opened beside the cases of its workflow the turn
    /// works on, and those this plan already opened. Cases in view only because the
    /// actor may reach them are not part of the work.
    pub(super) fn may_open_beside(&self, case_ref: &CaseRef) -> Result<(), DomainRejection> {
        let Ok(definition) = self.reducer.workflows.require(&case_ref.workflow) else {
            return Ok(());
        };
        let mut open: Vec<ErasedWorkflowView> = self
            .context
            .views
            .iter()
            .filter(|(key, _)| key.workflow == case_ref.workflow)
            .filter(|(key, _)| !self.context.is_subject_only_when_named(key))
            .map(|(_, view)| view.clone())
            .collect();
        open.extend(
            self.minted
                .iter()
                .filter(|minted| minted.workflow == case_ref.workflow)
                .filter_map(|minted| definition.project(minted.clone(), None).ok()),
        );
        match definition.may_open_beside(&open) {
            Err(ErasedCallError::Rejected(rejection)) => Err(*rejection),
            Ok(()) | Err(_) => Ok(()),
        }
    }

    pub(super) fn reject_blocked(
        &mut self,
        index: usize,
        reason: BlockReason,
    ) -> Result<(), ReductionError> {
        let code = match &reason {
            BlockReason::Constraint(constraint) => {
                self.record_constraint(*constraint);
                self.add_constraint_notice(*constraint);
                rejection::BLOCKED_BY_CONSTRAINT
            }
            BlockReason::Mode { .. } => rejection::BLOCKED_BY_MODE,
            BlockReason::ForbiddenRiskClass { .. } => rejection::FORBIDDEN_RISK_CLASS,
            BlockReason::HumanReview => rejection::HUMAN_REVIEW_REQUIRED,
        };
        let details = serde_json::to_value(&reason).unwrap_or(serde_json::Value::Null);
        let rejection = DomainRejection::new(RejectionCode::from(code), code).with_details(details);
        // A constraint's own notice names the user's instruction; the others need one.
        if !matches!(reason, BlockReason::Constraint(_)) {
            self.note_refusal(None, &rejection);
        }
        self.results[index] = Some(PlannedActResult::Rejected { rejection });
        Ok(())
    }
}
