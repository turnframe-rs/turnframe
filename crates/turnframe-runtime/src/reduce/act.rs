//! One act through prerequisites, the catalog, its arguments and its target, to the
//! commands it compiles to.

use super::*;

impl Session<'_> {
    pub(super) fn plan_act(
        &mut self,
        index: usize,
        understanding: &Understanding,
    ) -> Result<(), ReductionError> {
        let act = self.acts[index].clone();
        match &act.status {
            ActStatus::NeedsValue { arguments, reason } => {
                return self.needs_value(index, arguments, reason.clone());
            }
            ActStatus::Held { because } => {
                let words = understanding
                    .units
                    .iter()
                    .find(|unit| unit.id == *because)
                    .map(|unit| self.words(unit.words).to_owned())
                    .unwrap_or_default();
                self.changed_nothing.push(NarratableFact::ActHeld {
                    operation: Self::operation_name(&act),
                    because: words,
                });
                self.results[index] = Some(PlannedActResult::Held { because: *because });
                return Ok(());
            }
            _ => {}
        }
        if let Some(prerequisite) = self.prerequisite_not_ready(&act) {
            return match prerequisite {
                Some(earlier) => {
                    self.results[index] =
                        Some(PlannedActResult::AwaitingPrerequisite { act: earlier });
                    Ok(())
                }
                None => self.reject(index, rejection::PREREQUISITE_NOT_DONE),
            };
        }
        let spec = match &act.action {
            ActAction::Apply { .. } => match self.spec(&act) {
                Some(spec) => Some(spec),
                None => return self.reject(index, rejection::UNKNOWN_OPERATION),
            },
            ActAction::Start { workflow } => {
                if let Some(reason) = self.unmet_precondition(workflow) {
                    return self.reject_with(index, rejection::PRECONDITION_UNMET, Some(reason));
                }
                None
            }
        };
        if let Some(spec) = spec
            && !spec.availability.is_proposable()
            && !self.is_card_act(&act)
        {
            return self.reject(index, rejection::NOT_PROPOSABLE);
        }
        let arguments = match self.arguments_of(&act, spec) {
            Ok(arguments) => arguments,
            Err(code) => return self.reject(index, code),
        };
        if let Some(spec) = spec
            && spec.check_arguments(&arguments).is_err()
        {
            return self.reject(index, rejection::INVALID_ARGUMENTS);
        }
        let outcome = self.reducer.resolver.resolve(&act, spec, &self.opened_by);
        self.outcomes[index] = Some(outcome.clone());
        let case_ref = match &outcome {
            TargetOutcome::PolicyMismatch { .. } => {
                return self.reject(index, rejection::TARGET_POLICY_MISMATCH);
            }
            TargetOutcome::NoActiveInteraction => {
                return self.reject(index, rejection::NO_ACTIVE_INTERACTION);
            }
            TargetOutcome::SeveralOpenCases => {
                return self.reject(index, rejection::SEVERAL_OPEN_CASES);
            }
            TargetOutcome::SameTurn { case_ref } => case_ref.clone(),
            TargetOutcome::Resolved { resolution, .. } => match resolution {
                TargetResolution::Exact { case_ref } => case_ref.clone(),
                TargetResolution::Ambiguous { candidates } => {
                    return self.clarify_target(index, candidates);
                }
                TargetResolution::Missing => return self.reject(index, rejection::TARGET_MISSING),
                TargetResolution::Unauthorized => {
                    return self.reject(index, rejection::TARGET_UNAUTHORIZED);
                }
                TargetResolution::Stale { .. } => {
                    return self.reject(index, rejection::TARGET_STALE);
                }
                _ => return self.reject(index, rejection::TARGET_UNRESOLVED),
            },
            _ => return self.reject(index, rejection::TARGET_UNRESOLVED),
        };
        if outcome.is_new_case() {
            // Opening a record is starting its workflow, whichever act does it.
            if let Some(reason) = self.unmet_precondition(&case_ref.workflow) {
                return self.reject_with(index, rejection::PRECONDITION_UNMET, Some(reason));
            }
            if let Err(rejection) = self.may_open_beside(&case_ref) {
                self.note_refusal(Some(&case_ref), &rejection);
                self.results[index] = Some(rejection.into());
                return Ok(());
            }
            self.minted.push(case_ref.clone());
            self.opened_by.insert(act.id, case_ref.clone());
        }
        self.compile(index, &case_ref, outcome.is_unborn(), arguments)
    }

    /// Whether an act this one needs has not run. `Some(Some(act))` when that act waits
    /// for a click; `Some(None)` when it will not run at all.
    pub(super) fn prerequisite_not_ready(&self, act: &UnderstoodAct) -> Option<Option<ActId>> {
        for earlier in &act.depends_on {
            let position = self.acts.iter().position(|other| other.id == *earlier);
            match position.and_then(|position| self.results[position].as_ref()) {
                Some(PlannedActResult::ReadyToExecute { .. } | PlannedActResult::NoChange) => {}
                Some(
                    PlannedActResult::AwaitingConfirmation { .. }
                    | PlannedActResult::AwaitingPrerequisite { .. },
                ) => return Some(Some(*earlier)),
                _ => return Some(None),
            }
        }
        None
    }

    /// Stores each act that waits on a confirmation on that confirmation's card, so
    /// answering the card runs it once the confirmed commands commit (spec §6.7).
    pub(super) fn attach_dependents(&mut self) {
        for index in 0..self.acts.len() {
            let Some(PlannedActResult::AwaitingPrerequisite { act: earlier }) =
                &self.results[index]
            else {
                continue;
            };
            let Some(root) = self.confirming_act(*earlier) else {
                continue;
            };
            let act = self.acts[index].clone();
            let made = act
                .depends_on
                .iter()
                .filter_map(|earlier| {
                    let case_ref = self.opened_by.get(earlier)?.clone();
                    Some(crate::resume::MadeCase {
                        act: *earlier,
                        case_ref,
                    })
                })
                .collect();
            let dependent = crate::resume::DependentAct { act, made };
            let key = format!("confirm:{root}");
            for spec in &mut self.specs {
                if spec.key == key {
                    spec.payload = dependent.clone().attach_to(spec.payload.clone());
                }
            }
            let position = self.acts.iter().position(|act| act.id == root);
            if let Some(Some(PlannedActResult::AwaitingConfirmation { interaction_spec })) =
                position.map(|position| &mut self.results[position])
            {
                interaction_spec.payload = dependent.attach_to(interaction_spec.payload.clone());
            }
        }
    }

    /// The act whose confirmation card `act` ultimately waits on.
    pub(super) fn confirming_act(&self, mut act: ActId) -> Option<ActId> {
        for _ in 0..self.acts.len() {
            let position = self.acts.iter().position(|other| other.id == act)?;
            match self.results[position].as_ref()? {
                PlannedActResult::AwaitingConfirmation { .. } => return Some(act),
                PlannedActResult::AwaitingPrerequisite { act: earlier } => act = *earlier,
                _ => return None,
            }
        }
        None
    }

    pub(super) fn is_card_act(&self, act: &UnderstoodAct) -> bool {
        act.id == crate::resume::card_act_id()
            || self
                .reducer
                .confirmed_origin
                .as_ref()
                .is_some_and(|(_, card)| *card == act.id)
    }

    /// The act's arguments as the operation's document: record values resolved to the
    /// cases they name.
    /// The name the act that opens a record gives it, when its operation declares one.
    fn name_given_to(&self, opener: ActId) -> Option<String> {
        let act = self.acts.iter().find(|act| act.id == opener)?;
        let spec = self.spec(act)?;
        let naming = spec
            .arguments
            .iter()
            .find(|argument| argument.names_the_record)?;
        match &act.arguments.get(&naming.name)?.value {
            ArgumentValue::Json(serde_json::Value::String(name)) => Some(name.clone()),
            _ => None,
        }
    }

    pub(super) fn arguments_of(
        &self,
        act: &UnderstoodAct,
        spec: Option<&OperationSpec>,
    ) -> Result<serde_json::Value, &'static str> {
        let Some(spec) = spec else {
            return Ok(serde_json::Value::Null);
        };
        let mut values = Vec::with_capacity(act.arguments.len());
        for (name, argument) in &act.arguments {
            let value = match &argument.value {
                ArgumentValue::Json(value) => value.clone(),
                ArgumentValue::Record(RecordValue::Record { token }) => {
                    let resolver = &self.reducer.resolver;
                    let resolution = resolver.token_map().resolve(resolver.account_id(), token);
                    let case_ref = resolution.exact().ok_or(rejection::TARGET_MISSING)?;
                    let label = resolver.candidate(token).map(|case| case.label.as_str());
                    record_argument(case_ref, label)?
                }
                ArgumentValue::Record(RecordValue::SameTurn { act }) => {
                    let case_ref = self
                        .opened_by
                        .get(act)
                        .ok_or(rejection::PREREQUISITE_NOT_DONE)?;
                    record_argument(case_ref, self.name_given_to(*act).as_deref())?
                }
                // Looked up before reduction; one still named found nothing usable.
                ArgumentValue::Record(RecordValue::Named { .. }) => {
                    return Err(rejection::TARGET_MISSING);
                }
            };
            values.push((name.clone(), value));
        }
        Ok(spec.arguments_value(values))
    }

    pub(super) fn needs_value(
        &mut self,
        index: usize,
        arguments: &[String],
        reason: Option<String>,
    ) -> Result<(), ReductionError> {
        let act = self.acts[index].clone();
        let spec = self.spec(&act);
        let labels = arguments
            .iter()
            .map(|name| {
                spec.and_then(|spec| spec.argument_named(name))
                    .and_then(|argument| argument.labels_for(&self.input.locale).next())
                    .map_or_else(|| name.clone(), str::to_owned)
            })
            .collect();
        let outcome = self.reducer.resolver.resolve(&act, spec, &self.opened_by);
        let case_ref = outcome.exact().cloned();
        self.outcomes[index] = Some(outcome);
        self.changed_nothing.push(NarratableFact::ValueNeeded {
            case_ref,
            operation: Self::operation_name(&act),
            arguments: labels,
            reason: reason.clone(),
        });
        // Why a value is asked for again is the server's finding: it is on screen in the
        // server's words, never left to the reply's.
        if let Some(reason) = &reason {
            self.add_notice(
                notice::VALUE_ASKED_AGAIN,
                NoticeSeverity::Info,
                LocalizedText::new(reason.clone()),
            );
        }
        self.results[index] = Some(PlannedActResult::NeedsValue {
            arguments: arguments.to_vec(),
            reason,
        });
        Ok(())
    }

    pub(super) fn compile(
        &mut self,
        index: usize,
        case_ref: &CaseRef,
        unborn: bool,
        arguments: serde_json::Value,
    ) -> Result<(), ReductionError> {
        let reducer = self.reducer;
        let act = self.acts[index].clone();
        let Ok(definition) = reducer.workflows.require(&case_ref.workflow) else {
            return self.reject(index, rejection::UNKNOWN_WORKFLOW);
        };
        if !unborn && !self.context.views.contains_key(&case_ref.key()) {
            return Err(ReductionError::CaseNotLoaded { act: act.id });
        }
        // A case the turn opens stands as the earlier acts on it leave it.
        let planned = if unborn {
            self.unborn.get(&case_ref.key()).cloned()
        } else {
            None
        };
        let state = if unborn {
            planned.as_ref()
        } else {
            reducer.states.get(&case_ref.key())
        };
        let kind = match &act.action {
            ActAction::Apply { operation } => ResolvedActKind::ApplyOperation {
                operation: operation.clone(),
            },
            ActAction::Start { .. } => ResolvedActKind::StartWorkflow,
        };
        let resolved = ResolvedAct {
            act: act.id,
            kind,
            case_ref: case_ref.clone(),
            arguments,
            evidence_digest: self.evidence_digests[index].clone(),
        };
        let commands = match definition.compile_act(case_ref.clone(), state, &resolved) {
            Ok(commands) => commands,
            Err(ErasedCallError::Rejected(rejection)) => {
                let rejection = *rejection;
                self.note_refusal(Some(case_ref), &rejection);
                self.results[index] = Some(rejection.into());
                return Ok(());
            }
            Err(_) => {
                return Err(ReductionError::InconsistentPlan {
                    detail: format!("act {} failed at the workflow erasure boundary", act.id),
                });
            }
        };
        if commands.is_empty() {
            // A start writes no command: it makes the workflow the subject of the turn.
            if matches!(resolved.kind, ResolvedActKind::StartWorkflow) {
                self.results[index] = Some(PlannedActResult::NoChange);
                return Ok(());
            }
            let explanation = definition
                .nothing_changed(state, &resolved)
                .map(|text| text.resolve(&self.input.locale).to_owned())
                .unwrap_or_default();
            self.changed_nothing
                .push(NarratableFact::ActChangedNothing {
                    case_ref: case_ref.clone(),
                    operation: resolved
                        .operation()
                        .map(|operation| operation.as_str().to_owned()),
                    explanation,
                });
            self.results[index] = Some(PlannedActResult::NoChange);
            return Ok(());
        }
        self.command_count += commands.len();
        self.police(index, case_ref, definition, state, &commands, &resolved)?;
        if unborn
            && matches!(
                self.results[index],
                Some(PlannedActResult::ReadyToExecute { .. })
            )
        {
            let mut after = state.cloned();
            for command in &commands {
                after = definition
                    .state_after(after.as_ref(), command)
                    .ok()
                    .flatten();
                if after.is_none() {
                    break;
                }
            }
            match after {
                Some(after) => self.unborn.insert(case_ref.key(), after),
                None => self.unborn.remove(&case_ref.key()),
            };
        }
        Ok(())
    }
}

/// A record argument as the operation receives it: the record, and its label when the
/// record was in view.
fn record_argument(
    case_ref: &CaseRef,
    label: Option<&str>,
) -> Result<serde_json::Value, &'static str> {
    let mut value = serde_json::to_value(case_ref).map_err(|_| rejection::INVALID_ARGUMENTS)?;
    if let (Some(label), serde_json::Value::Object(map)) = (label, &mut value) {
        map.insert("label".to_owned(), serde_json::Value::from(label));
    }
    Ok(value)
}
