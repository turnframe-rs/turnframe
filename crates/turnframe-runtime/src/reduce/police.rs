//! Validation, policy and batching of the commands an act compiled to.

use super::*;

impl Session<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn police(
        &mut self,
        index: usize,
        case_ref: &CaseRef,
        definition: &Arc<dyn ErasedWorkflow>,
        state: Option<&serde_json::Value>,
        commands: &[serde_json::Value],
        resolved: &ResolvedAct,
    ) -> Result<(), ReductionError> {
        let turn_id = self.turn_id();
        let act_id = self.acts[index].id;
        let origin = self.origin_for(index);
        let confirm_key = format!("confirm:{act_id}");
        let mut refs = Vec::with_capacity(commands.len());
        let mut envelopes = Vec::with_capacity(commands.len());
        let mut scopes = Vec::with_capacity(commands.len());
        let mut aggregate: Option<CommandPolicy> = None;
        let mut diff: Vec<ReviewDiffEntry> = Vec::new();
        let mut needs_confirmation = false;
        let mut blocked: Option<BlockReason> = None;

        for (position, command) in commands.iter().enumerate() {
            match definition.validate_command(state, command) {
                Ok(()) => {}
                Err(ErasedCallError::Rejected(rejection)) => {
                    let rejection = *rejection;
                    self.note_refusal(Some(case_ref), &rejection);
                    self.results[index] = Some(rejection.into());
                    return Ok(());
                }
                Err(_) => {
                    return Err(ReductionError::InconsistentPlan {
                        detail: format!("act {act_id} failed domain validation at the boundary"),
                    });
                }
            }
            let policy = definition.command_policy(state, command).map_err(|_| {
                ReductionError::InconsistentPlan {
                    detail: format!("act {act_id} has an unreadable command policy"),
                }
            })?;
            let command_id = CommandId::derive(&turn_id, act_id, position);
            let batch_id = BatchId::derive(&turn_id, &case_ref.key(), &policy.atomicity);
            let command_ref = CommandRef {
                batch_id,
                command_id,
            };
            let request = PolicyRequest {
                command_ref,
                interaction_key: confirm_key.clone(),
                case_ref,
                policy: Some(&policy),
                origin: &origin,
                command,
            };
            if self.raises_confirmation(&policy) {
                self.record_constraint(ConstraintKind::AskBeforeApplying);
            }
            // A case every write on which needs a click raises the confirmation for
            // each command on it, which is what `AskBeforeApplying` means.
            let constraints: std::borrow::Cow<'_, [ConstraintKind]> =
                if self.context.confirms_every_write(&case_ref.key()) {
                    let mut raised = self.constraints.clone();
                    if !raised.contains(&ConstraintKind::AskBeforeApplying) {
                        raised.push(ConstraintKind::AskBeforeApplying);
                    }
                    std::borrow::Cow::Owned(raised)
                } else {
                    std::borrow::Cow::Borrowed(&self.constraints)
                };
            let outcome = self
                .reducer
                .policy
                .decide(&self.context.policy, &request, &constraints);
            diff.extend(self.reducer.policy.review_diff(&request));
            match &outcome {
                PolicyOutcome::Allowed { .. } => {}
                PolicyOutcome::NeedsConfirmation { .. } => needs_confirmation = true,
                PolicyOutcome::Blocked { reason, .. } => {
                    if blocked.is_none() {
                        blocked = Some(reason.clone());
                    }
                }
            }
            let decision = outcome.into_decision();
            aggregate = Some(match aggregate.take() {
                None => decision.policy.clone(),
                Some(previous) => strongest(previous, &decision.policy),
            });
            let idempotency_key = IdempotencyKey::derive(
                self.input.account_id(),
                &turn_id,
                case_ref,
                &origin,
                command,
            )
            .map_err(|_| ReductionError::Hash)?;
            envelopes.push(CommandEnvelope {
                command_id,
                turn_id,
                actor: self.input.actor.clone(),
                case_ref: case_ref.clone(),
                idempotency_key,
                origin: origin.clone(),
                command: command.clone(),
            });
            scopes.push(policy.atomicity.clone());
            refs.push(command_ref);
            self.decisions.push(decision);
        }

        let aggregate = aggregate.unwrap_or_else(CommandPolicy::conservative);
        if let Some(reason) = blocked {
            return self.reject_blocked(index, reason);
        }
        if self.constraints.contains(&ConstraintKind::ApplyOnlyIf) {
            // The turn is conditional and no code can evaluate the condition (§10.4).
            self.record_constraint(ConstraintKind::ApplyOnlyIf);
            let spec = self.condition_card(index, case_ref, aggregate.risk);
            return self.needs_clarification(index, spec);
        }
        if needs_confirmation {
            dedup_diff(&mut diff);
            let subject = definition
                .confirmation_subject(case_ref.clone(), state, resolved)
                .ok()
                .flatten()
                .or_else(|| self.stated_values(index));
            let subject_title = subject
                .as_ref()
                .and_then(|subject| subject.title.as_ref())
                .map(|title| title.resolve(&self.input.locale).to_owned());
            let Some(spec) = self.reducer.policy.confirmation_card(
                confirm_key,
                case_ref,
                &aggregate,
                refs,
                diff,
                subject,
            ) else {
                return self.reject(index, rejection::HUMAN_REVIEW_REQUIRED);
            };
            self.awaiting_confirmation
                .push(NarratableFact::ActAwaitingConfirmation {
                    case_ref: case_ref.clone(),
                    operation: match &resolved.kind {
                        ResolvedActKind::ApplyOperation { operation } => {
                            operation.as_str().to_owned()
                        }
                        other => format!("{other:?}"),
                    },
                    subject: subject_title,
                });
            self.push_spec(spec.clone());
            // The card executes exactly the commands that were reviewed (§15.3).
            for (envelope, scope) in envelopes.into_iter().zip(scopes) {
                self.hold(case_ref.key(), scope, envelope);
            }
            self.results[index] = Some(PlannedActResult::AwaitingConfirmation {
                interaction_spec: spec,
            });
            return Ok(());
        }
        for (envelope, scope) in envelopes.into_iter().zip(scopes) {
            self.batch(case_ref.key(), scope, envelope);
        }
        self.results[index] = Some(PlannedActResult::ReadyToExecute { command_refs: refs });
        Ok(())
    }

    /// Groups an envelope with the others of its case and scope (§13.4).
    pub(super) fn batch(
        &mut self,
        case: CaseKey,
        scope: AtomicityScope,
        envelope: CommandEnvelope<serde_json::Value>,
    ) {
        let key = (case.clone(), scope_key(&scope));
        let batch_id = BatchId::derive(&self.input.turn_id, &case, &scope);
        self.batches
            .entry(key)
            .or_insert_with(|| CommandBatch {
                batch_id,
                scope,
                envelopes: Vec::new(),
            })
            .envelopes
            .push(envelope);
    }

    /// Groups an envelope that may only run once a card authorizes it.
    pub(super) fn hold(
        &mut self,
        case: CaseKey,
        scope: AtomicityScope,
        envelope: CommandEnvelope<serde_json::Value>,
    ) {
        let key = (case.clone(), scope_key(&scope));
        let batch_id = BatchId::derive(&self.input.turn_id, &case, &scope);
        self.pending
            .entry(key)
            .or_insert_with(|| CommandBatch {
                batch_id,
                scope,
                envelopes: Vec::new(),
            })
            .envelopes
            .push(envelope);
    }

    pub(super) fn origin_for(&self, index: usize) -> CommandOrigin {
        if let Some((confirmed, card_act)) = &self.reducer.confirmed_origin
            && *card_act == self.acts[index].id
        {
            return confirmed.clone();
        }
        CommandOrigin::DirectSafeUserAct {
            evidence_digest: self.evidence_digests[index].clone(),
        }
    }

    pub(super) fn raises_confirmation(&self, policy: &CommandPolicy) -> bool {
        policy.confirmation == ConfirmationPolicy::None
            && self
                .constraints
                .contains(&ConstraintKind::AskBeforeApplying)
    }
}

/// The strongest of two policies: the riskier class, the stronger confirmation, the
/// first atomicity scope and the most restrictive claim mode.
fn strongest(mut policy: CommandPolicy, other: &CommandPolicy) -> CommandPolicy {
    policy.risk = policy.risk.max(other.risk);
    policy.confirmation = policy.confirmation.max(other.confirmation);
    policy.claim_mode = policy.claim_mode.min(other.claim_mode);
    policy
}

/// Stable grouping key of an atomicity scope.
fn scope_key(scope: &AtomicityScope) -> String {
    format!(
        "{}\u{0}{}",
        scope.discriminant(),
        scope.group_name().unwrap_or("")
    )
}

fn dedup_diff(entries: &mut Vec<ReviewDiffEntry>) {
    let mut seen = BTreeSet::new();
    entries.retain(|entry| seen.insert(entry.field.clone()));
}

impl Session<'_> {
    /// What a card says when the domain gives no subject: each value the act would write,
    /// under its argument's label, so the user sees what they are confirming.
    fn stated_values(&self, index: usize) -> Option<turnframe_core::flow::ConfirmationSubject> {
        let act = &self.acts[index];
        let spec = self.spec(act)?;
        let stated: Vec<(&turnframe_core::operation::ArgumentSpec, String)> = spec
            .arguments
            .iter()
            .filter_map(|argument| {
                let value = match &act.arguments.get(&argument.name)?.value {
                    ArgumentValue::Json(serde_json::Value::String(text)) => text.clone(),
                    ArgumentValue::Json(serde_json::Value::Null) => return None,
                    ArgumentValue::Json(other) => other.to_string(),
                    ArgumentValue::Record(_) => return None,
                };
                Some((argument, value))
            })
            .collect();
        if stated.is_empty() {
            return None;
        }
        let lines = |locale: Option<&turnframe_core::locale::Locale>| -> String {
            stated
                .iter()
                .map(|(argument, value)| {
                    let label = argument
                        .labels
                        .iter()
                        .find(|label| label.locale.as_ref() == locale)
                        .or_else(|| argument.labels.iter().find(|label| label.locale.is_none()))
                        .map_or(argument.name.as_str(), |label| label.text.as_str());
                    format!("{label}: {value}")
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut body = LocalizedText::new(lines(None));
        let locales: std::collections::BTreeSet<&turnframe_core::locale::Locale> = stated
            .iter()
            .flat_map(|(argument, _)| argument.labels.iter())
            .filter_map(|label| label.locale.as_ref())
            .collect();
        for locale in locales {
            body = body.with(locale.clone(), lines(Some(locale)));
        }
        Some(turnframe_core::flow::ConfirmationSubject::describing(body))
    }
}
