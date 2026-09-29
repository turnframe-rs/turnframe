//! The steps a turn and a plan-only run share: loading and projecting cases, judging a
//! card answer, issuing tokens, the act a click stands for, and the reduction context.
//!
//! [`crate::orchestrator`] runs them against the live stores and [`crate::planning`]
//! against read-only ones or against state it was handed, so the two roads cannot
//! drift.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::command::ResolutionChannel;
use turnframe_core::error::{InteractionError, InvariantViolation, OrchestratorError, StoreError};
use turnframe_core::flow::{
    ErasedWorkflow, ErasedWorkflowView, WorkflowDefinitions, check_erased_view,
};
use turnframe_core::ids::{AccountId, ConversationId, OriginToken, TurnId, WorkflowKey};
use turnframe_core::interaction::{
    Interaction, InteractionRejection, InteractionStatus, StoredInteractionAction,
    validate_response,
};
use turnframe_core::locale::LocalizedText;
use turnframe_core::observe::{Observer, Signal, SignalLabels};
use turnframe_core::operation::OperationCatalog;
use turnframe_core::plan::limits::PlanLimits;
use turnframe_core::policy::PolicySnapshot;
use turnframe_core::reduce::{
    ActiveInteractionSummary, CommandRef, ReductionContext, ReductionPlan,
};
use turnframe_core::replay::TargetResolutionRecord;
use turnframe_core::response::{AssistantTurn, ClaimClass};
use turnframe_core::turn::{ActorContext, InteractionResponse, TurnInput};
use turnframe_core::understanding::{
    ActAction, ActId, ActStatus, ActTarget, ArgumentValue, RecordValue, Understanding,
    UnderstoodArgument,
};
use turnframe_store::conversation::StoredTurn;
use turnframe_store::interaction::InteractionRecord;

use crate::config::OrchestratorConfig;
use crate::conversation::RecentMessage;
use crate::interactions::{AcceptedInteraction, summarize, summarize_all};
use crate::orchestrator::{CaseCandidate, CaseDirectory};
use crate::policy::PolicyEngine;
use crate::reduce::DefaultTurnReducer;
use crate::resolve::{AuthorizedCase, CaseIdFactory, TargetResolver};
use crate::resume::{Resumption, card_act, card_act_id};

/// One case as a turn loaded it: the reference it was read at, the labels the
/// model sees, the state and the projection.
pub(crate) struct LoadedCase {
    pub(crate) case_ref: CaseRef,
    pub(crate) label: String,
    pub(crate) state: Option<serde_json::Value>,
    pub(crate) view: ErasedWorkflowView,
    /// What the directory declared about it beyond its names.
    pub(crate) terms: DirectoryTerms,
}

/// What a case directory declared about a case it offered, beyond its names.
///
/// One struct and not two arguments, for the reason [`FactScope`] is one: they
/// are both booleans about the same case, and a call site that takes them loose
/// and adjacent is how the wrong one ends up set.
///
/// [`FactScope`]: crate::compose
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DirectoryTerms {
    /// See
    /// [`CaseCandidate::confirm_every_write`](crate::orchestrator::CaseCandidate::confirm_every_write).
    pub(crate) confirm_every_write: bool,
    /// See
    /// [`CaseCandidate::subject_only_when_named`](crate::orchestrator::CaseCandidate::subject_only_when_named).
    pub(crate) subject_only_when_named: bool,
}

/// Projects one case and holds it to the §8.4 invariants (I2).
pub(crate) fn project_case(
    observer: &dyn Observer,
    definition: &Arc<dyn ErasedWorkflow>,
    case_ref: CaseRef,
    label: String,
    state: Option<serde_json::Value>,
    terms: DirectoryTerms,
) -> Result<LoadedCase, OrchestratorError> {
    // Spec §28 asks for *pure* projection time, so the stage is the call and
    // nothing around it: not the load that produced `state`, not the invariant
    // check that follows. A turn that addresses three cases reports three.
    let stage = crate::signals::Stage::enter();
    let view = definition.project(case_ref.clone(), state.as_ref())?;
    stage.observe(
        observer,
        Signal::ProjectionDuration,
        &SignalLabels::workflow(case_ref.workflow.clone()),
    );
    if let Err(violations) = check_erased_view(&view) {
        let first = violations.into_iter().next().unwrap_or(InvariantViolation {
            case_ref: case_ref.clone(),
            kind: turnframe_core::error::InvariantViolationKind::UnserializableObligation,
        });
        return Err(OrchestratorError::InvariantViolation(first));
    }
    Ok(LoadedCase {
        case_ref,
        label,
        state,
        view,
        terms,
    })
}

/// The candidates an open card asks the turn to consider: the case it belongs
/// to, so answering it can reach that case, and the cases its
/// [`SelectTarget`](StoredInteractionAction::SelectTarget) options offer, so a
/// choice between cases can reach the one that is chosen (spec §12.3).
///
/// These are proposals, not admissions. Each one still has to pass
/// [`admit_card_case`] before it is loaded, because the card was written under
/// the scope the actor had *then* and this turn belongs to the scope they have
/// *now*.
pub(crate) fn candidates_of_open_cards(
    known: &[CaseCandidate],
    open_interactions: &[Interaction],
) -> Vec<CaseCandidate> {
    let mut extra: Vec<CaseCandidate> = Vec::new();
    let has = |key: &CaseKey, extra: &[CaseCandidate]| {
        known.iter().any(|candidate| candidate.key == *key)
            || extra.iter().any(|candidate| candidate.key == *key)
    };
    for interaction in open_interactions {
        let key = interaction.case_ref.key();
        if !has(&key, &extra) {
            extra.push(CaseCandidate::new(
                key,
                interaction.payload.title.default.clone(),
            ));
        }
        for option in &interaction.payload.options {
            let StoredInteractionAction::SelectTarget { case_ref } = &option.action else {
                continue;
            };
            let key = case_ref.key();
            if !has(&key, &extra) {
                extra.push(CaseCandidate::new(key, option.label.default.clone()));
            }
        }
    }
    extra
}

/// Whether a case an open card named may join this turn, and under which
/// labels (spec §12.3, §25.4).
///
/// `exists` is what the executor answered: `false` is a case with no state, so
/// there is no record to authorize and nothing to read. That is the runtime's
/// own doing — a `StartWorkflow` mints an identifier and the confirmation card
/// that would create the record is written against it — and the directory
/// cannot be expected to list a case that does not exist, so it is admitted
/// unasked. Anything that exists goes to
/// [`CaseDirectory::authorize_case`], whose default refuses.
pub(crate) async fn admit_card_case(
    observer: &dyn Observer,
    directory: &dyn CaseDirectory,
    actor: &ActorContext,
    conversation: &ConversationId,
    candidate: CaseCandidate,
    exists: bool,
) -> Result<Option<CaseCandidate>, StoreError> {
    if !exists {
        return Ok(Some(candidate));
    }
    let Some(mut authorized) = directory
        .authorize_case(actor, conversation, &candidate.key)
        .await?
    else {
        tracing::warn!(
            target: "turnframe.authorization",
            workflow = %candidate.key.workflow,
            "an open card names a case the directory does not authorize for this actor; \
             the card is not part of this turn"
        );
        // Beside the log line, because a rate that climbs is the only way an
        // operator learns that the one containment path below the account has
        // started refusing (§25.4).
        observer.observe_labeled(
            &Signal::CaseNotAuthorized,
            &SignalLabels::workflow(candidate.key.workflow.clone()),
        );
        return Ok(None);
    };
    // The question was about one case, so the answer is about that case: only
    // the labels are taken from the directory, never a different identifier.
    authorized.key = candidate.key;
    Ok(Some(authorized))
}

/// Why a click cannot be admitted at all: the card is still open and sits on a
/// case this turn may not address, so as far as this turn is concerned the card
/// is not there (spec §25.4).
///
/// Only an open card is judged here. A card that has already reached a terminal
/// status is answered by the interaction rules as it always was — a second
/// click returns the original resolution (I14) — and that answer says nothing
/// about a record.
pub(crate) fn unaddressable_card(
    cases: &IndexMap<CaseKey, LoadedCase>,
    record: &InteractionRecord,
) -> Option<InteractionRejection> {
    let open = record.status().is_open();
    let addressable = cases.contains_key(&record.interaction.case_ref.key());
    (open && !addressable).then_some(InteractionRejection::NotFound)
}

/// The open cards this turn may act on: the ones whose case it loaded.
///
/// A card whose case the directory refused is not part of the turn at all. It
/// does not block, it is not summarized into the reduction context, and it
/// cannot be the card a `Card` target names — which would otherwise resolve to the
/// very case the refusal withheld.
pub(crate) fn addressable_cards(
    cases: &IndexMap<CaseKey, LoadedCase>,
    open_interactions: Vec<Interaction>,
) -> Vec<Interaction> {
    open_interactions
        .into_iter()
        .filter(|interaction| cases.contains_key(&interaction.case_ref.key()))
        .collect()
}

/// Step F: opaque tokens for the cases the actor may address.
pub(crate) fn build_resolver(
    account: &AccountId,
    turn_id: TurnId,
    definitions: &WorkflowDefinitions,
    case_ids: &Arc<dyn CaseIdFactory>,
    cases: &IndexMap<CaseKey, LoadedCase>,
    origin: Option<(&OriginToken, &CaseKey)>,
    active: Option<ActiveInteractionSummary>,
) -> TargetResolver {
    let mut builder =
        TargetResolver::builder(account.clone(), turn_id).case_id_factory(Arc::clone(case_ids));
    // Which workflows a start reaches into rather than mints beside. Declared
    // once per workflow rather than read from any case, so it is asked here and
    // not per act.
    for definition in definitions.iter() {
        if definition.start_behaviour().resumes() {
            builder = builder.resuming(definition.key());
        }
    }
    for case in cases.values() {
        let mut candidate = AuthorizedCase::new(case.case_ref.clone(), case.label.clone());
        if case.terms.subject_only_when_named {
            candidate = candidate.reachable_only();
        }
        builder = builder.candidate(candidate);
    }
    // §12.4: the surface that issued the token knew which record it meant, so
    // the binding is that record and no other.
    if let Some((token, key)) = origin
        && let Some(case) = cases.get(key)
    {
        builder = builder.origin(
            token.clone(),
            AuthorizedCase::new(case.case_ref.clone(), case.label.clone()),
        );
    }
    if let Some(summary) = active {
        builder = builder.active_interaction(summary);
    }
    builder.build()
}

/// The card an `ActiveInteraction` target means: the one being answered right
/// now, else the blocking card of the conversation.
pub(crate) fn blocking_summary(
    answered: Option<&AcceptedInteraction>,
    open_interactions: &[Interaction],
) -> Option<ActiveInteractionSummary> {
    if let Some(accepted) = answered {
        return Some(summarize(&accepted.record.interaction));
    }
    open_interactions
        .iter()
        .find(|interaction| interaction.blocking)
        .map(summarize)
}

/// The workflows whose start preconditions this turn does not meet, with the
/// reason each one gave.
///
/// A precondition is satisfied when some case **in view** belongs to the named
/// workflow and sits in one of the named phases. A case the turn did not load
/// cannot satisfy anything, which is the conservative reading and the right
/// one: the runtime declines to offer a start it cannot see the grounds for.
pub(crate) fn unmet_preconditions(
    definitions: &WorkflowDefinitions,
    cases: &IndexMap<CaseKey, LoadedCase>,
) -> Vec<(WorkflowKey, LocalizedText)> {
    let mut unmet = Vec::new();
    for definition in definitions.iter() {
        for precondition in definition.start_preconditions() {
            if !cases
                .values()
                .any(|case| precondition.satisfied_by(&case.view))
            {
                unmet.push((definition.key(), precondition.reason.clone()));
            }
        }
    }
    unmet
}

/// The arguments an answered card's operation runs with: the ones it was stored
/// with, plus the words the user typed on it, at the place the option named. A value
/// asked for on a card arrives bound to its case and operation.
pub(crate) fn arguments_with_answer(
    stored: &serde_json::Value,
    pointer: Option<&str>,
    answer: Option<&str>,
) -> serde_json::Value {
    let (Some(pointer), Some(answer)) = (pointer, answer) else {
        return stored.clone();
    };
    let mut filled = stored.clone();
    if write_at_pointer(&mut filled, pointer, answer) {
        return filled;
    }
    tracing::warn!(
        target: "turnframe.interactions",
        pointer = %pointer,
        "a card's freeform answer had nowhere to go; the operation gets the \
         arguments it was stored with"
    );
    stored.clone()
}

/// Writes `text` at `pointer` (RFC 6901) in `value`, creating the objects the
/// path needs, and says whether it could.
///
/// Only objects are created: a pointer through an array, or through a scalar
/// that is already there, is a pointer this document cannot have — the
/// deployment wrote it wrong, and the operation is better off refused by the
/// domain for a missing argument than filled at a place nobody meant.
fn write_at_pointer(value: &mut serde_json::Value, pointer: &str, text: &str) -> bool {
    let Some(path) = pointer.strip_prefix('/') else {
        return false;
    };
    let mut cursor = value;
    let mut segments = path.split('/').peekable();
    while let Some(segment) = segments.next() {
        // RFC 6901 escapes, in the order the standard gives them.
        let key = segment.replace("~1", "/").replace("~0", "~");
        if key.is_empty() {
            return false;
        }
        if !cursor.is_object() {
            if cursor.is_null() {
                *cursor = serde_json::Value::Object(serde_json::Map::new());
            } else {
                return false;
            }
        }
        let Some(map) = cursor.as_object_mut() else {
            return false;
        };
        if segments.peek().is_none() {
            map.insert(key, serde_json::Value::String(text.to_owned()));
            return true;
        }
        cursor = map.entry(key).or_insert(serde_json::Value::Null);
    }
    false
}

/// Step C without the write: the answer is validated against the revision the
/// case is really at, and the card is left exactly as it was.
///
/// A card that somebody has already answered is not an error here any more than
/// it is in a real turn: the turn carries on without it (I14).
pub(crate) fn admit(
    input: &TurnInput,
    cases: &IndexMap<CaseKey, LoadedCase>,
    record: InteractionRecord,
    now: DateTime<Utc>,
) -> Result<Option<AcceptedInteraction>, OrchestratorError> {
    let Some(response) = input.interaction_response.as_ref() else {
        return Ok(None);
    };
    if let Some(rejection) = unaddressable_card(cases, &record) {
        return Err(OrchestratorError::Interaction(InteractionError::Rejected(
            rejection,
        )));
    }
    let current = cases
        .get(&record.interaction.case_ref.key())
        .map_or(record.interaction.case_ref.expected_revision, |case| {
            case.case_ref.expected_revision
        });
    match validate_response(
        &record.interaction,
        response,
        ResolutionChannel::Click,
        &input.actor,
        &input.conversation_id,
        current,
        now,
    ) {
        Ok(accepted) => Ok(Some(AcceptedInteraction {
            response: accepted,
            record,
        })),
        Err(InteractionRejection::AlreadyResolved { .. })
        | Err(InteractionRejection::NotActive {
            status: InteractionStatus::Resolving,
        }) => Ok(None),
        Err(rejection) => Err(OrchestratorError::Interaction(InteractionError::Rejected(
            rejection,
        ))),
    }
}

/// The resolutions the replay record of a real turn would carry.
pub(crate) fn target_resolution_records(plan: &ReductionPlan) -> Vec<TargetResolutionRecord> {
    plan.acts
        .iter()
        .filter_map(|planned| {
            planned
                .target
                .clone()
                .map(|resolution| TargetResolutionRecord {
                    act: planned.act.id,
                    resolution,
                })
        })
        .collect()
}

/// Every command the plan would execute this turn.
pub(crate) fn command_refs(plan: &ReductionPlan) -> Vec<CommandRef> {
    plan.batches
        .iter()
        .flat_map(|batch| {
            batch.envelopes.iter().map(|envelope| CommandRef {
                batch_id: batch.batch_id,
                command_id: envelope.command_id,
            })
        })
        .collect()
}

/// The claim classes the answer would be entitled to state (spec §18.3).
pub(crate) fn would_claim(plan: &ReductionPlan) -> Vec<ClaimClass> {
    let would_commit = plan.batches.iter().any(|batch| !batch.envelopes.is_empty());
    let would_show_card = !plan.pre_execution_interactions.is_empty();
    ClaimClass::ALL
        .into_iter()
        .filter(|class| match class {
            // The library has no notification channel, so a promise to use one
            // could never be kept, planned or not (§18.4).
            ClaimClass::FutureNotification => false,
            ClaimClass::InteractionVisibility => would_show_card,
            _ => would_commit,
        })
        .collect()
}

/// The understanding with the act a card answer stands for, if any.
pub(crate) struct WithCardAct {
    pub(crate) understanding: Understanding,
    /// Whether the answer declined an instruction.
    pub(crate) declined: bool,
    /// The card's own act: the only act its answer authorizes.
    pub(crate) card_act: Option<ActId>,
}

/// Turns the stored option of an accepted card answer into the act it stands for
/// (spec §13.3, §15.3, I7), placed before the message's own acts.
pub(crate) fn card_act_of(
    mut understanding: Understanding,
    answered: Option<&AcceptedInteraction>,
    resolver: &TargetResolver,
) -> WithCardAct {
    let Some(accepted) = answered else {
        return WithCardAct {
            understanding,
            declined: false,
            card_act: None,
        };
    };
    if let StoredInteractionAction::ApplyOperation {
        operation,
        arguments,
        freeform_argument,
    } = &accepted.response.action
    {
        let token = resolver
            .token_map()
            .token_for(&accepted.response.case_ref.key())
            .cloned();
        let Some(token) = token else {
            return WithCardAct {
                understanding,
                declined: false,
                card_act: None,
            };
        };
        // A card that asked for a value carries the typed answer into the arguments.
        let arguments = arguments_with_answer(
            arguments,
            freeform_argument.as_deref(),
            accepted.response.freeform_input.as_deref(),
        );
        let mut act = card_act(
            ActAction::Apply {
                operation: operation.clone(),
            },
            ActTarget::Record { token },
        );
        if let serde_json::Value::Object(map) = arguments {
            act.arguments = map
                .into_iter()
                .map(|(name, value)| {
                    let argument = UnderstoodArgument {
                        value: ArgumentValue::Json(value),
                        excerpt: None,
                    };
                    (name, argument)
                })
                .collect();
        }
        understanding.acts.insert(0, act);
        return WithCardAct {
            understanding,
            declined: false,
            card_act: Some(card_act_id()),
        };
    }
    match crate::resume::resume(
        &accepted.response,
        &accepted.record.interaction.payload,
        resolver.token_map(),
    ) {
        Resumption::Continue(act) => {
            understanding.acts.insert(0, *act);
            WithCardAct {
                understanding,
                declined: false,
                card_act: Some(card_act_id()),
            }
        }
        Resumption::Declined { record } => {
            let card_act = record.as_ref().map(|act| act.id);
            if let Some(act) = record {
                understanding.acts.insert(0, *act);
            }
            WithCardAct {
                understanding,
                declined: true,
                card_act,
            }
        }
        _ => WithCardAct {
            understanding,
            declined: false,
            card_act: None,
        },
    }
}

/// The response a typed card answer stands for: the card on screen, the option
/// understanding read, validated like a click but on the model-interpreted channel,
/// which authorizes only what needs no confirmation (fixes D4).
pub(crate) fn typed_response(
    card: &Interaction,
    option: &turnframe_core::ids::OptionId,
) -> InteractionResponse {
    InteractionResponse {
        interaction_id: card.id,
        option_id: option.clone(),
        expected_case_revision: card.case_ref.expected_revision,
        freeform_input: None,
    }
}

/// Step C for a typed answer without the write: validated on the model-interpreted
/// channel against the revision the case is at.
pub(crate) fn admit_typed(
    input: &TurnInput,
    cases: &IndexMap<CaseKey, LoadedCase>,
    card: &Interaction,
    response: &InteractionResponse,
    now: DateTime<Utc>,
) -> Result<AcceptedInteraction, InteractionRejection> {
    let current = cases
        .get(&card.case_ref.key())
        .map_or(card.case_ref.expected_revision, |case| {
            case.case_ref.expected_revision
        });
    validate_response(
        card,
        response,
        ResolutionChannel::ModelInterpreted,
        &input.actor,
        &input.conversation_id,
        current,
        now,
    )
    .map(|accepted| AcceptedInteraction {
        response: accepted,
        record: InteractionRecord::new(card.clone()),
    })
}

/// The context every reduction of this turn runs in.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reduction_context(
    cases: &IndexMap<CaseKey, LoadedCase>,
    open_interactions: &[Interaction],
    resolver: &TargetResolver,
    operations: &OperationCatalog,
    policy: &PolicySnapshot,
    limits: PlanLimits,
    now: DateTime<Utc>,
) -> ReductionContext {
    ReductionContext {
        views: cases
            .iter()
            .map(|(key, case)| (key.clone(), case.view.clone()))
            .collect(),
        confirm_every_write: cases
            .iter()
            .filter(|(_, case)| case.terms.confirm_every_write)
            .map(|(key, _)| key.clone())
            .collect(),
        subject_only_when_named: cases
            .iter()
            .filter(|(_, case)| case.terms.subject_only_when_named)
            .map(|(key, _)| key.clone())
            .collect(),
        active_interactions: summarize_all(open_interactions),
        target_map: resolver.token_map().clone(),
        operations: operations.clone(),
        policy: policy.clone(),
        limits,
        now,
    }
}

/// The reducer of this turn: the loaded states, and the card answer's origin for the
/// card's own act.
pub(crate) fn turn_reducer(
    definitions: &WorkflowDefinitions,
    cases: &IndexMap<CaseKey, LoadedCase>,
    resolver: &TargetResolver,
    policy_engine: &PolicyEngine,
    config: &OrchestratorConfig,
    answered: Option<&AcceptedInteraction>,
    card_act: Option<ActId>,
) -> DefaultTurnReducer {
    let mut reducer = DefaultTurnReducer::new(
        definitions.clone(),
        resolver.clone(),
        policy_engine.clone(),
        config,
    )
    .with_states(
        cases
            .iter()
            .filter_map(|(key, case)| case.state.clone().map(|state| (key.clone(), state))),
    );
    if let Some((origin, act)) = answered.and_then(|accepted| Some((accepted.origin()?, card_act?)))
    {
        reducer = reducer.with_confirmed_origin(origin, act);
    }
    reducer
}

/// The earlier messages of the conversation, oldest first, and the last assistant turn.
pub(crate) fn recent_messages(
    turns: Vec<StoredTurn>,
    current: TurnId,
) -> (Vec<RecentMessage>, Option<AssistantTurn>) {
    let mut messages = Vec::new();
    let mut previous = None;
    for turn in turns {
        if turn.user.input.turn_id == current {
            continue;
        }
        if let Some(text) = turn.user.input.text.as_deref()
            && !text.trim().is_empty()
        {
            messages.push(RecentMessage::user(text));
        }
        if let Some(assistant) = turn.assistant {
            let said = crate::orchestrator::assistant_text(&assistant);
            if !said.is_empty() {
                messages.push(RecentMessage::assistant(said));
            }
            previous = Some(assistant);
        }
    }
    (messages, previous)
}

/// The acts aimed at a record the message named and the turn did not have: the act,
/// its workflow, and the words that named it.
pub(crate) fn unlisted(
    understanding: &Understanding,
    text: &str,
) -> Vec<(ActId, WorkflowKey, Option<String>)> {
    understanding
        .acts
        .iter()
        .filter_map(|act| match &act.target {
            ActTarget::NotListed { workflow, words } => {
                let named = words.and_then(|range| text.get(range.start..range.end));
                Some((act.id, workflow.clone(), named.map(str::to_owned)))
            }
            _ => None,
        })
        .collect()
}

/// Each record argument naming a record not in view: its act, argument, workflow and
/// the words that name it.
pub(crate) fn unlisted_arguments(
    understanding: &Understanding,
) -> Vec<(ActId, String, WorkflowKey, String)> {
    understanding
        .acts
        .iter()
        .flat_map(|act| {
            act.arguments
                .iter()
                .filter_map(|(name, argument)| match &argument.value {
                    ArgumentValue::Record(RecordValue::Named { workflow, named }) => {
                        Some((act.id, name.clone(), workflow.clone(), named.clone()))
                    }
                    _ => None,
                })
        })
        .collect()
}

/// Gives each looked-up argument the one record found; one that found none, or
/// several, is asked for again with `reason`, keeping the name for the record the user
/// may register next.
pub(crate) fn fill_found_arguments(
    understanding: &mut Understanding,
    found: &std::collections::BTreeMap<(ActId, String), (Vec<CaseKey>, String)>,
    resolver: &TargetResolver,
) {
    for act in &mut understanding.acts {
        for ((id, name), (keys, reason)) in found {
            if *id != act.id {
                continue;
            }
            let token = match keys.as_slice() {
                [key] => resolver.token_map().token_for(key).cloned(),
                _ => None,
            };
            if let Some(token) = token {
                if let Some(argument) = act.arguments.get_mut(name) {
                    argument.value = ArgumentValue::Record(RecordValue::Record { token });
                }
                continue;
            }
            act.status = match std::mem::replace(&mut act.status, ActStatus::Ready) {
                ActStatus::NeedsValue {
                    mut arguments,
                    reason: earlier,
                } => {
                    arguments.push(name.clone());
                    ActStatus::NeedsValue {
                        arguments,
                        reason: earlier.or_else(|| Some(reason.clone())),
                    }
                }
                _ => ActStatus::NeedsValue {
                    arguments: vec![name.clone()],
                    reason: Some(reason.clone()),
                },
            };
        }
    }
}

/// Aims each looked-up act at what the lookup found: the one record, or a choice
/// among several. An act the lookup found nothing for keeps its target.
pub(crate) fn aim_at_found(
    understanding: &mut Understanding,
    found: &std::collections::BTreeMap<ActId, Vec<CaseKey>>,
    resolver: &TargetResolver,
) {
    for act in &mut understanding.acts {
        let Some(keys) = found.get(&act.id) else {
            continue;
        };
        let mut tokens: Vec<_> = keys
            .iter()
            .filter_map(|key| resolver.token_map().token_for(key).cloned())
            .collect();
        act.target = match tokens.len() {
            0 => continue,
            1 => ActTarget::Record {
                token: tokens.remove(0),
            },
            _ => ActTarget::Ambiguous { candidates: tokens },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cards_freeform_answer_lands_where_the_option_points() {
        let mut arguments = serde_json::json!({ "cabin": "economy" });
        assert!(write_at_pointer(
            &mut arguments,
            "/traveler",
            "Marta Bianchi"
        ));
        assert_eq!(
            arguments,
            serde_json::json!({ "cabin": "economy", "traveler": "Marta Bianchi" })
        );
        let mut nested = serde_json::json!({});
        assert!(write_at_pointer(&mut nested, "/fields/name", "anna"));
        assert_eq!(nested, serde_json::json!({ "fields": { "name": "anna" } }));
        // RFC 6901 escapes, so a field whose name carries a slash is reachable.
        let mut escaped = serde_json::json!({});
        assert!(write_at_pointer(&mut escaped, "/a~1b", "x"));
        assert_eq!(escaped, serde_json::json!({ "a/b": "x" }));
    }

    #[test]
    fn a_pointer_that_does_not_fit_writes_nothing() {
        let mut through_a_scalar = serde_json::json!({ "traveler": "already here" });
        assert!(!write_at_pointer(
            &mut through_a_scalar,
            "/traveler/name",
            "anna"
        ));
        assert_eq!(
            through_a_scalar,
            serde_json::json!({ "traveler": "already here" })
        );
        let mut anything = serde_json::json!({});
        assert!(!write_at_pointer(&mut anything, "traveler", "anna"));
        assert!(!write_at_pointer(&mut anything, "/", "anna"));
        assert_eq!(anything, serde_json::json!({}));
    }

    #[test]
    fn the_arguments_of_an_answered_card_carry_the_answer() {
        let stored = serde_json::json!({ "cabin": "economy" });
        assert_eq!(
            arguments_with_answer(&stored, Some("/traveler"), Some("Marta Bianchi")),
            serde_json::json!({ "cabin": "economy", "traveler": "Marta Bianchi" })
        );
        assert_eq!(
            arguments_with_answer(&stored, None, Some("ignored")),
            stored
        );
        assert_eq!(
            arguments_with_answer(&stored, Some("/traveler"), None),
            stored
        );
        assert_eq!(
            arguments_with_answer(&stored, Some("/cabin/name"), Some("x")),
            stored
        );
    }
}
