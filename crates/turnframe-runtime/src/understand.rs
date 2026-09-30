//! Step G of a turn: what the message asks, read by the understanding pipeline over what
//! the turn loaded, before anything is reduced.
//!
//! The input is built from the loaded cases (tokens, labels, phases, stated fields,
//! obligations, the operations each offers), the card on screen, the earlier messages and
//! the receipts of the last turn. The domain's own compile and validation check each act
//! before it reaches the plan, through [`RegistryChecker`].

use std::collections::BTreeSet;

use chrono::NaiveDate;
use indexmap::IndexMap;
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::error::{DomainRejection, ErasedCallError, OrchestratorError};
use turnframe_core::flow::{ErasedWorkflowView, WorkflowDefinitions};
use turnframe_core::hash::Digest;
use turnframe_core::ids::{CaseRevision, OperationKey};
use turnframe_core::interaction::Interaction;
use turnframe_core::locale::Locale;
use turnframe_core::operation::{OperationCatalog, OperationSpec};
use turnframe_core::plan::TargetPolicy;
use turnframe_core::reduce::ActiveInteractionSummary;
use turnframe_core::response::{AssistantTurn, Expectation, ResponseBlock};
use turnframe_core::target::{ResolvedAct, ResolvedActKind};
use turnframe_core::understanding::{ActAction, ActTarget, ArgumentValue, UnderstoodAct};
use turnframe_understand::{
    ActChecker, OpenCard, PendingAct, PreviousReceipt, RecordBrief, Speaker, UnderstandingInput,
    WorkflowBrief,
};

use crate::config::UnderstandingConfig;
use crate::conversation::{RecentMessage, TranscriptRole};
use crate::planning::LoadedCase;
use crate::resolve::TargetResolver;

/// What the understanding of one turn is built from.
pub(crate) struct Sources<'a> {
    pub turn: turnframe_core::ids::TurnId,
    pub definitions: &'a WorkflowDefinitions,
    pub cases: &'a IndexMap<CaseKey, LoadedCase>,
    pub resolver: &'a TargetResolver,
    pub text: &'a str,
    pub locale: &'a Locale,
    pub today: NaiveDate,
    pub recent: &'a [RecentMessage],
    pub previous: Option<&'a AssistantTurn>,
    pub card: Option<(&'a Interaction, &'a ActiveInteractionSummary)>,
    pub typed_answers_allowed: bool,
    pub config: &'a UnderstandingConfig,
    pub effort: &'a crate::effort::EffortProfile,
    /// Whether a knowledge source can answer a question about the domain in general.
    pub knowledge: bool,
}

/// The operations offered to this turn: every loaded case's, plus what each workflow
/// offers on a record that does not exist yet, and, with none of its records loaded, what
/// one could do.
pub(crate) fn operation_catalog(
    definitions: &WorkflowDefinitions,
    cases: &IndexMap<CaseKey, LoadedCase>,
) -> Result<OperationCatalog, OrchestratorError> {
    let mut offered: IndexMap<OperationKey, OperationSpec> = IndexMap::new();
    for case in cases.values() {
        let definition = definitions.require(&case.case_ref.workflow)?;
        for spec in definition.operations(case.case_ref.clone(), case.state.as_ref())? {
            offered.entry(spec.key.clone()).or_insert(spec);
        }
    }
    for definition in definitions.iter() {
        let fresh = CaseRef::new(definition.key(), "", CaseRevision::ZERO);
        for spec in definition.operations(fresh, None)? {
            offered.entry(spec.key.clone()).or_insert(spec);
        }
        // With none of its records loaded, what one could do is known too: understanding is
        // shown it, and a record this turn opens may be asked it.
        if !cases.keys().any(|key| key.workflow == definition.key()) {
            for spec in definition.record_operations() {
                if spec.availability.is_proposable() {
                    offered.entry(spec.key.clone()).or_insert(spec);
                }
            }
        }
    }
    OperationCatalog::new(offered.into_values()).map_err(OrchestratorError::Reduction)
}

fn phase_text(view: &ErasedWorkflowView) -> String {
    match &view.phase {
        serde_json::Value::String(phase) => phase.clone(),
        other => other.to_string(),
    }
}

/// The input understanding reads.
pub(crate) fn input(sources: &Sources<'_>) -> Result<UnderstandingInput, OrchestratorError> {
    let mut turn = UnderstandingInput::new(sources.text, sources.locale.clone(), sources.today);
    for message in sources.recent {
        let speaker = match message.role {
            TranscriptRole::User => Speaker::User,
            TranscriptRole::Assistant => Speaker::Assistant,
        };
        turn = turn.with_earlier(speaker, &message.text);
    }
    for definition in sources.definitions.iter() {
        turn = turn.with_workflow(workflow_brief(sources, definition.as_ref())?);
    }
    if let Some((card, summary)) = sources.card {
        turn = turn.with_card(open_card(sources, card, summary));
    }
    if let Some(expectation) = sources
        .previous
        .and_then(|previous| expected(sources, previous))
    {
        turn = turn.with_expectation(expectation);
    }
    if let Some(previous) = sources.previous {
        for pending in still_waiting(sources, previous) {
            turn = turn.with_waiting(pending);
        }
        for act in did(sources, previous) {
            turn = turn.with_done(act);
        }
    }
    turn = turn
        .with_settings(sources.effort.settings)
        .with_knowledge(sources.knowledge);
    if let Some(previous) = sources.previous {
        for offer in &previous.offers {
            if let Some(offered) = still_offered(&turn, sources, offer) {
                turn = turn.with_offer(offered);
            }
        }
    }
    if let Some(previous) = sources.previous {
        for subject in &previous.subjects {
            if let Some(token) = sources.resolver.token_map().token_for(&subject.key()) {
                turn = turn.with_last_subject(token.clone());
            }
        }
        let receipts = previous.blocks.iter().filter_map(|block| match block {
            ResponseBlock::Receipt(receipt) => Some(&receipt.receipt),
            _ => None,
        });
        for (position, receipt) in receipts.enumerate() {
            let title = receipt.title.resolve(sources.locale);
            let body = receipt.body.resolve(sources.locale);
            turn = turn.with_receipt(PreviousReceipt::new(
                format!("r{}", position + 1),
                format!("{title}: {body}"),
            ));
        }
    }
    Ok(turn)
}

/// An offer of the last reply that its record still offers, as the act it runs in this
/// turn's tokens: the values it knew, and the rest of what its operation takes to ask. An
/// offer to open a record none of exists runs while its workflow may still open one.
fn still_offered(
    turn: &UnderstandingInput,
    sources: &Sources<'_>,
    offer: &turnframe_core::response::Offer,
) -> Option<turnframe_understand::OfferBrief> {
    let (brief, spec) = turn.operation(&offer.operation)?;
    let token = if offer.case_ref.case_id.as_str().is_empty() {
        if !brief.new_case.contains(&offer.operation) {
            return None;
        }
        None
    } else {
        let token = sources
            .resolver
            .token_map()
            .token_for(&offer.case_ref.key())?
            .clone();
        let (_, record) = turn.record(&token)?;
        if !record.offers(&offer.operation) {
            return None;
        }
        Some(token)
    };
    let given: std::collections::BTreeMap<
        String,
        turnframe_core::understanding::UnderstoodArgument,
    > = offer
        .arguments
        .iter()
        .map(|(name, value)| {
            let argument = turnframe_core::understanding::UnderstoodArgument {
                value: ArgumentValue::Json(value.clone()),
                excerpt: None,
            };
            (name.clone(), argument)
        })
        .collect();
    let missing = spec
        .arguments
        .iter()
        .filter(|argument| {
            !matches!(
                argument.source,
                turnframe_core::operation::ArgumentSource::Server { .. }
            ) && !given.contains_key(&argument.name)
        })
        .map(|argument| argument.name.clone())
        .collect();
    Some(turnframe_understand::OfferBrief::new(
        offer.words.clone(),
        PendingAct {
            operation: offer.operation.clone(),
            record: token,
            given,
            missing,
        },
    ))
}

/// Acts the last reply left waiting for a record the user named, in this turn's tokens:
/// those carried from earlier turns and those it asks about beside the first.
fn still_waiting(sources: &Sources<'_>, previous: &AssistantTurn) -> Vec<PendingAct> {
    use turnframe_core::understanding::{ArgumentValue, RecordValue};
    previous
        .expectations
        .iter()
        .filter_map(|expectation| match expectation {
            Expectation::AwaitingValue {
                act,
                case_ref,
                missing,
            }
            | Expectation::StillWaiting {
                act,
                case_ref,
                missing,
            } => {
                let named = missing.iter().any(|name| {
                    matches!(
                        act.arguments.get(name).map(|argument| &argument.value),
                        Some(ArgumentValue::Record(RecordValue::Named { .. }))
                    )
                });
                let record = match case_ref {
                    Some(case_ref) => Some(
                        sources
                            .resolver
                            .token_map()
                            .token_for(&case_ref.key())?
                            .clone(),
                    ),
                    None => None,
                };
                if !named {
                    return None;
                }
                Some(PendingAct {
                    operation: act.operation()?.clone(),
                    record,
                    given: act.arguments.clone(),
                    missing: missing.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

/// The acts the last turn did, in this turn's tokens, with the values they were given. A
/// value naming a record, or quoting that turn's words, means nothing now: only plain values
/// are kept.
fn did(sources: &Sources<'_>, previous: &AssistantTurn) -> Vec<PendingAct> {
    previous
        .done
        .iter()
        .filter_map(|done| {
            let operation = done.act.operation()?.clone();
            let record = sources
                .resolver
                .token_map()
                .token_for(&done.case_ref.key())
                .cloned()?;
            let given = done
                .act
                .arguments
                .iter()
                .filter(|(_, argument)| matches!(argument.value, ArgumentValue::Json(_)))
                .map(|(name, argument)| {
                    let plain = turnframe_core::understanding::UnderstoodArgument {
                        value: argument.value.clone(),
                        excerpt: None,
                    };
                    (name.clone(), plain)
                })
                .collect();
            Some(PendingAct {
                operation,
                record: Some(record),
                given,
                missing: Vec::new(),
            })
        })
        .collect()
}

/// What the last reply asked for, in this turn's tokens. One at a time: the first.
fn expected(
    sources: &Sources<'_>,
    previous: &AssistantTurn,
) -> Option<turnframe_understand::Expectation> {
    let token = |case_ref: &CaseRef| {
        sources
            .resolver
            .token_map()
            .token_for(&case_ref.key())
            .cloned()
    };
    previous
        .expectations
        .iter()
        .find_map(|expectation| match expectation {
            Expectation::AwaitingValue {
                act,
                case_ref,
                missing,
            } => {
                let operation = act.operation()?.clone();
                Some(turnframe_understand::Expectation::Values(PendingAct {
                    operation,
                    record: case_ref.as_ref().and_then(token),
                    given: act.arguments.clone(),
                    missing: missing.clone(),
                }))
            }
            Expectation::AwaitingObligation {
                case_ref,
                obligation,
            } => Some(turnframe_understand::Expectation::Obligation {
                record: token(case_ref)?,
                sentence: obligation.clone(),
            }),
            Expectation::AwaitingOperation { case_ref, act, .. } => {
                Some(turnframe_understand::Expectation::Values(PendingAct {
                    operation: act.operation.clone(),
                    record: Some(token(case_ref)?),
                    given: act
                        .given
                        .iter()
                        .map(|(name, value)| {
                            (
                                name.clone(),
                                turnframe_core::understanding::UnderstoodArgument {
                                    value: turnframe_core::understanding::ArgumentValue::Json(
                                        value.clone(),
                                    ),
                                    excerpt: None,
                                },
                            )
                        })
                        .collect(),
                    missing: act.asks.clone(),
                }))
            }
            _ => None,
        })
}

fn workflow_brief(
    sources: &Sources<'_>,
    definition: &dyn turnframe_core::flow::ErasedWorkflow,
) -> Result<WorkflowBrief, OrchestratorError> {
    let key = definition.key();
    let mut brief = WorkflowBrief::new(key.clone());
    if let Some(summary) = definition.summary() {
        brief = brief.summary(summary);
    }
    for term in definition.glossary() {
        brief = brief.term(term);
    }
    let startable = definition
        .start_preconditions()
        .into_iter()
        .all(|precondition| {
            sources
                .cases
                .values()
                .any(|case| precondition.satisfied_by(&case.view))
        });
    if startable {
        brief = brief.startable();
    }
    let mut offered: BTreeSet<OperationKey> = BTreeSet::new();
    let fresh = CaseRef::new(key.clone(), "", CaseRevision::ZERO);
    // What a record that does not exist yet offers opens one: nothing, when the
    // workflow cannot start. Existing records still offer their own below.
    let fresh_operations = if startable {
        definition.operations(fresh, None)?
    } else {
        Vec::new()
    };
    for spec in fresh_operations {
        if matches!(
            spec.target_policy,
            TargetPolicy::NewCaseOnly | TargetPolicy::AllowsNewCase
        ) {
            brief = brief.on_new_case(spec.key.clone());
        }
        if offered.insert(spec.key.clone()) {
            brief = brief.operation(spec);
        }
    }
    let mut subjects: BTreeSet<String> = BTreeSet::new();
    let mut listed = false;
    for case in sources
        .cases
        .values()
        .filter(|case| case.case_ref.workflow == key)
    {
        let Some(token) = sources.resolver.token_map().token_for(&case.case_ref.key()) else {
            continue;
        };
        let operations = definition.operations(case.case_ref.clone(), case.state.as_ref())?;
        let mut record =
            RecordBrief::new(token.clone(), case.label.clone(), phase_text(&case.view))
                .offering(operations.iter().map(|spec| spec.key.clone()));
        for field in &case.view.state {
            subjects.insert(field.field.clone());
            record = record.field(field.clone());
        }
        for obligation in &case.view.obligations {
            let sentence = obligation.sentence.as_ref().map_or_else(
                || obligation.value.to_string(),
                |text| text.resolve(sources.locale).to_owned(),
            );
            record = record.obligation(sentence);
        }
        if let Some(briefing) = definition.briefing(case.case_ref.clone(), case.state.as_ref())? {
            record = record.briefing(sources.config.briefing_budget.apply(&briefing));
        }
        if let Ok(enumerations) =
            definition.enumerations(case.case_ref.clone(), case.state.as_ref())
        {
            subjects.extend(enumerations.into_iter().map(|e| e.subject.to_string()));
        }
        for spec in operations {
            if offered.insert(spec.key.clone()) {
                brief = brief.operation(spec);
            }
        }
        brief = brief.record(record);
        listed = true;
    }
    // With none of its records in view, what one could do is still asked for: shown, it is
    // told there is none yet rather than left unread.
    if !listed {
        for spec in definition.record_operations() {
            let creates = matches!(
                spec.target_policy,
                TargetPolicy::NewCaseOnly | TargetPolicy::AllowsNewCase
            );
            if !creates && spec.availability.is_proposable() && offered.insert(spec.key.clone()) {
                brief = brief.operation(spec);
            }
        }
    }
    for subject in subjects {
        brief = brief.subject(subject);
    }
    Ok(brief)
}

fn open_card(
    sources: &Sources<'_>,
    card: &Interaction,
    summary: &ActiveInteractionSummary,
) -> OpenCard {
    let locale = sources.locale;
    let mut open = OpenCard::new(
        card.case_ref.workflow.clone(),
        card.payload.title.resolve(locale),
    );
    if let Some(token) = sources.resolver.token_map().token_for(&card.case_ref.key()) {
        open = open.about(token.clone());
    }
    for option in &card.payload.options {
        open = open.option(option.id.clone(), option.label.resolve(locale));
    }
    if !(sources.typed_answers_allowed && summary.accepts_text_resolution()) {
        open = open.click_only();
    }
    open
}

/// The domain's own check of an act, against the state the turn loaded: its pure
/// `compile_act` and `validate_command`, dry-run.
pub(crate) struct RegistryChecker<'a> {
    pub definitions: &'a WorkflowDefinitions,
    pub cases: &'a IndexMap<CaseKey, LoadedCase>,
    pub resolver: &'a TargetResolver,
    pub operations: &'a OperationCatalog,
}

impl RegistryChecker<'_> {
    fn target(&self, act: &UnderstoodAct) -> Option<(CaseRef, Option<&serde_json::Value>)> {
        match &act.target {
            ActTarget::Record { token } => {
                let resolution = self
                    .resolver
                    .token_map()
                    .resolve(self.resolver.account_id(), token);
                let case_ref = resolution.exact()?.clone();
                let state = self
                    .cases
                    .get(&case_ref.key())
                    .and_then(|case| case.state.as_ref());
                Some((case_ref, state))
            }
            ActTarget::New { workflow } => {
                Some((CaseRef::new(workflow.clone(), "", CaseRevision::ZERO), None))
            }
            _ => None,
        }
    }
}

impl ActChecker for RegistryChecker<'_> {
    fn check(&self, act: &UnderstoodAct) -> Result<(), DomainRejection> {
        let ActAction::Apply { operation } = &act.action else {
            return Ok(());
        };
        let Some(spec) = self.operations.get(operation) else {
            return Ok(());
        };
        let Some((case_ref, state)) = self.target(act) else {
            return Ok(());
        };
        let mut values = Vec::with_capacity(act.arguments.len());
        for (name, argument) in &act.arguments {
            match &argument.value {
                ArgumentValue::Json(value) => values.push((name.clone(), value.clone())),
                _ => return Ok(()),
            }
        }
        let arguments = spec.arguments_value(values);
        if spec.check_arguments(&arguments).is_err() {
            return Ok(());
        }
        let Ok(definition) = self.definitions.require(&case_ref.workflow) else {
            return Ok(());
        };
        let resolved = ResolvedAct {
            act: act.id,
            kind: ResolvedActKind::ApplyOperation {
                operation: operation.clone(),
            },
            case_ref: case_ref.clone(),
            arguments,
            evidence_digest: Digest::of_bytes(b"turnframe.check"),
        };
        let commands = match definition.compile_act(case_ref, state, &resolved) {
            Ok(commands) => commands,
            Err(ErasedCallError::Rejected(rejection)) => return Err(*rejection),
            Err(_) => return Ok(()),
        };
        for command in &commands {
            if let Err(ErasedCallError::Rejected(rejection)) =
                definition.validate_command(state, command)
            {
                return Err(*rejection);
            }
        }
        Ok(())
    }
}

/// What understanding a turn produced, with the record of every call it made.
pub(crate) struct Understood {
    pub understanding: turnframe_core::understanding::Understanding,
    pub tasks: Vec<turnframe_core::replay::TaskRecord>,
    pub budget: turnframe_core::replay::BudgetReport,
}

/// Understands the turn `sources` describe. A turn with no text is a click, and costs
/// no model call.
pub(crate) async fn run(
    understander: &dyn turnframe_understand::TurnUnderstander,
    sources: &Sources<'_>,
    operations: &OperationCatalog,
    steps: &dyn turnframe_understand::StepSink,
) -> Result<Understood, OrchestratorError> {
    let scope = turnframe_tasks::TaskScope::new(sources.effort.budget, sources.locale.clone())
        .for_turn(sources.turn.to_string())
        .with_profiles(sources.effort.tasks.clone())
        .with_effort(sources.effort.effort);
    if sources.text.trim().is_empty() {
        return Ok(Understood {
            understanding: turnframe_core::understanding::Understanding::default(),
            tasks: Vec::new(),
            budget: scope.budget_report(),
        });
    }
    let turn = input(sources)?;
    let checker = RegistryChecker {
        definitions: sources.definitions,
        cases: sources.cases,
        resolver: sources.resolver,
        operations,
    };
    let understanding = understander
        .understand(&scope, &turn, steps, &checker)
        .await;
    Ok(Understood {
        understanding,
        tasks: scope.records(),
        budget: scope.budget_report(),
    })
}
