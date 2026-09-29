//! The persistent interaction engine (spec §15, §23 steps C, L and P).
//!
//! A card is a durable server-owned record, not a model tool call. This module
//! is where the four rules that make that true are enforced against a real
//! store:
//!
//! 1. **Persistence precedes the sentence.** [`InteractionEngine::persist`]
//!    writes every specification the reducer asked for *before* any block of
//!    the response may refer to a card (§15.5). What it could not write is
//!    reported in [`PersistedInteractions::failed`], and the caller must then
//!    say nothing about a card the user cannot see.
//! 2. **One blocking card per case.** A blocking specification goes in through
//!    [`InteractionWriter::insert_replacing_blocking`](turnframe_store::interaction::InteractionWriter::insert_replacing_blocking), so the previous occupant
//!    is invalidated and the replacement gets a **new** identifier (§15.6). A
//!    `Resolving` occupant is never replaced: its commands are running.
//! 3. **Resolution is compare-and-set.** [`InteractionEngine::accept`]
//!    validates the client's answer through core's [`validate_response`] and
//!    then moves `Active → Resolving` through the store's own compare-and-set.
//!    A second click loses that race and comes back as
//!    [`ResponseAdmission::AlreadyAnswered`], carrying the record instead of
//!    authorizing a second execution (§15.5, I14).
//! 4. **Resolved means committed.** A card becomes `Resolved` only once the
//!    command its answer authorized has committed; otherwise it is `Failed` or
//!    restored to `Active` according to policy. There is no path here that
//!    marks a card resolved on the strength of an intention.
//!
//! # The channel is part of the authorization
//!
//! [`validate_response`] takes a [`ResolutionChannel`], and so does
//! [`InteractionEngine::accept`]: a structured click, a typed word that matched
//! a stored alias exactly, and a "yes" the interpreter inferred are three
//! different authorities, and the stored
//! [`TextResolutionPolicy`](turnframe_core::interaction::TextResolutionPolicy)
//! decides which of them the card admits (§15.7). The channel travels into the
//! [`CommandOrigin`] the answer mints, so a replay can say which one happened
//! (I20).

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::command::{CommandOrigin, ResolutionChannel};
use turnframe_core::error::InteractionError;
use turnframe_core::hash::derive_uuid;
use turnframe_core::ids::{
    AccountId, CaseRevision, ConversationId, EventId, InteractionId, TurnId,
};
use turnframe_core::interaction::{
    AcceptedResponse, Interaction, InteractionRejection, InteractionSpec, InteractionStatus,
    validate_response,
};
use turnframe_core::observe::{NoopObserver, Observer, Signal, SignalLabels};
use turnframe_core::reduce::ActiveInteractionSummary;
use turnframe_core::turn::{ActorContext, InteractionResponse};
use turnframe_store::error::StoreError;
use turnframe_store::interaction::{InteractionRecord, InteractionStore, ResolutionOutcome};

use crate::config::InteractionConfig;

/// Domain separation of the derived interaction identifiers.
const INTERACTION_ID_DOMAIN: &str = "turnframe.interaction_id.v1";

/// Derives the identifier of the card `key` of `turn`.
///
/// Deterministic on purpose: a turn replayed after a crash re-creates the same
/// card under the same identifier instead of a second one the client would show
/// twice (I20, spec §23.1).
#[must_use]
pub fn derive_interaction_id(turn_id: &TurnId, key: &str) -> InteractionId {
    InteractionId::from(derive_uuid(
        INTERACTION_ID_DOMAIN,
        &[&turn_id.to_string(), key],
    ))
}

/// What [`InteractionEngine::persist`] managed to write.
///
/// `failed` is the whole point of the type. A caller that ignores it and
/// narrates "I have prepared the change below" is exactly the defect §15.5
/// forbids, so the outcome makes the failure impossible to overlook while still
/// handing back the cards that *were* written.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PersistedInteractions {
    /// The cards now visible to the client, in specification order.
    pub created: Vec<Interaction>,
    /// Cards a replacing insert invalidated, in application order.
    pub invalidated: Vec<InteractionId>,
    /// The first failure, when one specification could not be written.
    pub failed: Option<InteractionError>,
}

impl PersistedInteractions {
    /// Returns `true` when every specification was written.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failed.is_none()
    }

    /// The cards, as the client-facing views a response block carries.
    #[must_use]
    pub fn views(&self) -> Vec<turnframe_core::interaction::InteractionView> {
        self.created.iter().map(Interaction::view).collect()
    }
}

/// A client answer that passed every §15.5 rule and holds the card in
/// `Resolving`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AcceptedInteraction {
    /// The validated answer.
    pub response: AcceptedResponse,
    /// The stored record, now in `Resolving`.
    pub record: InteractionRecord,
}

impl AcceptedInteraction {
    /// The command origin the answer authorizes, or `None` when it authorizes
    /// nothing (a selection, a clarification, a dismissal).
    #[must_use]
    pub fn origin(&self) -> Option<CommandOrigin> {
        self.response.origin()
    }

    /// The case the card belongs to, at the revision it was bound to.
    #[must_use]
    pub fn case_ref(&self) -> &CaseRef {
        &self.response.case_ref
    }

    /// The interaction identifier.
    #[must_use]
    pub fn interaction_id(&self) -> InteractionId {
        self.response.interaction_id
    }
}

/// What a client answer is judged against (spec §15.5).
///
/// It exists so the six things that decide whether a click is valid — who is
/// clicking, in which conversation and turn, how the answer arrived, what
/// revision the case is really at, and when "now" is — travel together and
/// none of them can be forgotten at a call site.
#[derive(Debug, Clone, Copy)]
pub struct ResponseContext<'a> {
    /// The authenticated actor.
    pub actor: &'a ActorContext,
    /// The conversation the card belongs to.
    pub conversation: &'a ConversationId,
    /// The turn that is answering.
    pub turn_id: TurnId,
    /// How the answer arrived (spec §15.7).
    pub channel: ResolutionChannel,
    /// The revision the case is at, read fresh.
    pub current_revision: CaseRevision,
    /// The instant to judge expiry against.
    pub now: DateTime<Utc>,
}

impl<'a> ResponseContext<'a> {
    /// A structured click.
    #[must_use]
    pub fn click(
        actor: &'a ActorContext,
        conversation: &'a ConversationId,
        turn_id: TurnId,
        current_revision: CaseRevision,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            actor,
            conversation,
            turn_id,
            channel: ResolutionChannel::Click,
            current_revision,
            now,
        }
    }

    /// Returns a copy that arrived on another channel.
    #[must_use]
    pub const fn through(mut self, channel: ResolutionChannel) -> Self {
        self.channel = channel;
        self
    }
}

/// What happened to a client answer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ResponseAdmission {
    /// The answer was accepted and the card is now `Resolving`.
    Accepted(Box<AcceptedInteraction>),
    /// The card had already been answered — a second click, or a race the
    /// client lost. It covers both a resolution that is still running
    /// (`Resolving`) and one that finished (`Resolved`): in either case the
    /// record comes back so the turn can repeat the original *result* without
    /// repeating the original *effect* (§15.5, I14).
    AlreadyAnswered(Box<InteractionRecord>),
}

impl ResponseAdmission {
    /// The accepted answer, when there was one.
    #[must_use]
    pub fn accepted(&self) -> Option<&AcceptedInteraction> {
        match self {
            Self::Accepted(accepted) => Some(accepted),
            Self::AlreadyAnswered(_) => None,
        }
    }

    /// Returns `true` when nothing new may execute for this answer.
    #[must_use]
    pub fn is_replay(&self) -> bool {
        matches!(self, Self::AlreadyAnswered(_))
    }
}

/// Reads, writes and settles persisted interactions (spec §15).
#[derive(Clone)]
pub struct InteractionEngine {
    store: Arc<dyn InteractionStore>,
    config: InteractionConfig,
    observer: Arc<dyn Observer>,
}

impl fmt::Debug for InteractionEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InteractionEngine")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl InteractionEngine {
    /// Builds an engine over `store`.
    #[must_use]
    pub fn new(store: Arc<dyn InteractionStore>, config: InteractionConfig) -> Self {
        Self {
            store,
            config,
            observer: Arc::new(NoopObserver),
        }
    }

    /// Sends this stage's signals to `observer` (spec §26.2).
    ///
    /// [`Orchestrator`](crate::orchestrator::Orchestrator) sets it from its own
    /// observer when it is built, so a card settled **out of band** — the
    /// §15.7 free-text channel, an operator tool, a reconciliation job — is
    /// counted on the same series as one a turn settled.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = observer;
        self
    }

    /// The interaction configuration in force.
    #[must_use]
    pub const fn config(&self) -> &InteractionConfig {
        &self.config
    }

    /// Materializes one specification and writes it (spec §15.5, §15.6).
    ///
    /// A blocking card replaces the case's current blocking occupant and
    /// invalidates it; a non-blocking one is inserted next to whatever is
    /// there. The identifier is derived from the turn and the specification
    /// key, so a replay writes the same card rather than a duplicate.
    ///
    /// # Errors
    ///
    /// * [`InteractionError::InvalidSpec`] when the card could not be answered;
    /// * [`InteractionError::NotPersisted`] when the store refused the write —
    ///   after which the response must not mention the card.
    pub async fn create(
        &self,
        spec: InteractionSpec,
        account: &AccountId,
        conversation: ConversationId,
        turn_id: TurnId,
        now: DateTime<Utc>,
    ) -> Result<(Interaction, Vec<InteractionId>), InteractionError> {
        let id = derive_interaction_id(&turn_id, &spec.key);
        let mut spec = spec;
        if spec.expires_in.is_none()
            && let Some(ttl) = self.config.default_ttl
        {
            spec = spec.expires_in(ttl);
        }
        let interaction =
            Interaction::from_spec(spec, id, account.clone(), conversation, turn_id, now)?;
        let invalidated = if interaction.blocking {
            self.store
                .insert_replacing_blocking(interaction.clone())
                .await
                .map_err(persistence_failure)?
        } else {
            self.store
                .insert(interaction.clone())
                .await
                .map_err(persistence_failure)?;
            Vec::new()
        };
        Ok((interaction, invalidated))
    }

    /// Writes every specification, stopping at the first failure.
    ///
    /// Nothing here is best effort in the sense of "carry on and hope": the
    /// first failure stops the loop and is reported, precisely so the caller
    /// cannot describe cards that are not there.
    pub async fn persist(
        &self,
        specs: &[InteractionSpec],
        account: &AccountId,
        conversation: ConversationId,
        turn_id: TurnId,
        now: DateTime<Utc>,
    ) -> PersistedInteractions {
        let mut created = Vec::with_capacity(specs.len());
        let mut invalidated = Vec::new();
        let mut failed = None;
        for spec in specs {
            match self
                .create(spec.clone(), account, conversation, turn_id, now)
                .await
            {
                Ok((interaction, gone)) => {
                    created.push(interaction);
                    invalidated.extend(gone);
                }
                Err(error) => {
                    tracing::warn!(
                        target: "turnframe.interactions",
                        key = %spec.key,
                        "interaction persistence failed; the response may not mention this card"
                    );
                    failed = Some(error);
                    break;
                }
            }
        }
        PersistedInteractions {
            created,
            invalidated,
            failed,
        }
    }

    /// Validates a client answer and takes the card into `Resolving`
    /// (spec §15.5, §23 step C).
    ///
    /// `current_revision` is the revision the case is actually at, read fresh:
    /// a card bound to an older one is
    /// [`Stale`](InteractionRejection::Stale) whatever the client echoed.
    ///
    /// # Errors
    ///
    /// [`InteractionError::Rejected`] carrying the §15.5 rejection. An
    /// identifier of another tenant is [`InteractionRejection::NotFound`],
    /// exactly like one that never existed (spec §25.4).
    pub async fn accept(
        &self,
        context: ResponseContext<'_>,
        response: &InteractionResponse,
    ) -> Result<ResponseAdmission, InteractionError> {
        let ResponseContext {
            actor,
            conversation,
            turn_id,
            channel,
            current_revision,
            now,
        } = context;
        let record = match self
            .store
            .get(&actor.account_id, &response.interaction_id)
            .await
        {
            Ok(record) => record,
            // Unknown and other-tenant identifiers are indistinguishable, and
            // a store that is merely unavailable must not look like either.
            Err(StoreError::NotFound) => {
                return Err(InteractionError::Rejected(InteractionRejection::NotFound));
            }
            Err(_) => return Err(InteractionError::NotPersisted),
        };
        let accepted = match validate_response(
            &record.interaction,
            response,
            channel,
            actor,
            conversation,
            current_revision,
            now,
        ) {
            Ok(accepted) => accepted,
            // Both mean "somebody already answered this": one because the
            // resolution finished, the other because it is still running. A
            // second click authorizes nothing either way (I14).
            Err(InteractionRejection::AlreadyResolved { .. })
            | Err(InteractionRejection::NotActive {
                status: InteractionStatus::Resolving,
            }) => {
                return Ok(ResponseAdmission::AlreadyAnswered(Box::new(record)));
            }
            Err(rejection) => return Err(InteractionError::Rejected(rejection)),
        };
        match self
            .store
            .begin_resolution(
                &actor.account_id,
                &response.interaction_id,
                InteractionStatus::Active,
                accepted.option_id.clone(),
                turn_id,
            )
            .await
        {
            Ok(record) => Ok(ResponseAdmission::Accepted(Box::new(AcceptedInteraction {
                response: accepted,
                record,
            }))),
            // The compare-and-set lost: somebody else already began resolving
            // this card. That is a double click, and the honest answer is the
            // resolution that is already under way (I14).
            Err(StoreError::Conflict) => {
                let record = self
                    .store
                    .get(&actor.account_id, &response.interaction_id)
                    .await
                    .map_err(|_| InteractionError::NotPersisted)?;
                Ok(ResponseAdmission::AlreadyAnswered(Box::new(record)))
            }
            Err(StoreError::NotFound) => {
                Err(InteractionError::Rejected(InteractionRejection::NotFound))
            }
            Err(_) => Err(InteractionError::NotPersisted),
        }
    }

    /// Settles a `Resolving` card.
    ///
    /// # Errors
    ///
    /// [`InteractionError::NotPersisted`] when the store refused, and
    /// [`InteractionError::Rejected`] with
    /// [`NotFound`](InteractionRejection::NotFound) when the card is not there.
    pub async fn settle(
        &self,
        account: &AccountId,
        id: &InteractionId,
        outcome: ResolutionOutcome,
    ) -> Result<InteractionRecord, InteractionError> {
        let settled = self
            .store
            .finish_resolution(account, id, outcome)
            .await
            .map_err(|error| match error {
                StoreError::NotFound => InteractionError::Rejected(InteractionRejection::NotFound),
                _ => InteractionError::NotPersisted,
            })?;
        // Only once the store said so: a card that reached a status is one the
        // store wrote, not one this process intended to write.
        observe_settled(self.observer.as_ref(), &settled);
        Ok(settled)
    }

    /// Marks a card `Resolved` — only legitimate once the command its answer
    /// authorized has committed, which is why the events are required (§15.5).
    ///
    /// # Errors
    ///
    /// See [`Self::settle`].
    pub async fn mark_resolved(
        &self,
        account: &AccountId,
        id: &InteractionId,
        event_ids: Vec<EventId>,
    ) -> Result<InteractionRecord, InteractionError> {
        self.settle(account, id, ResolutionOutcome::Resolved { event_ids })
            .await
    }

    /// Marks a card `Failed` after the command it authorized did not commit.
    ///
    /// # Errors
    ///
    /// See [`Self::settle`].
    pub async fn mark_failed(
        &self,
        account: &AccountId,
        id: &InteractionId,
        code: impl Into<String>,
    ) -> Result<InteractionRecord, InteractionError> {
        self.settle(account, id, ResolutionOutcome::Failed { code: code.into() })
            .await
    }

    /// Puts a card back to `Active` so the user may answer again.
    ///
    /// # Errors
    ///
    /// See [`Self::settle`].
    pub async fn restore(
        &self,
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, InteractionError> {
        self.settle(account, id, ResolutionOutcome::RestoreActive)
            .await
    }

    /// The open cards of a conversation, oldest first.
    ///
    /// # Errors
    ///
    /// [`InteractionError::NotPersisted`] when the store could not answer;
    /// silently pretending there are none would let a blocking card be
    /// bypassed (I19).
    pub async fn open_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Vec<Interaction>, InteractionError> {
        self.store
            .list_open_for_conversation(account, conversation)
            .await
            .map_err(|_| InteractionError::NotPersisted)
    }

    /// The open cards of one case, oldest first.
    ///
    /// # Errors
    ///
    /// See [`Self::open_for_conversation`].
    pub async fn open_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Result<Vec<Interaction>, InteractionError> {
        self.store
            .list_open_for_case(account, case_key)
            .await
            .map_err(|_| InteractionError::NotPersisted)
    }

    /// Whether the user has already answered a blocking card of this case at
    /// this revision.
    ///
    /// What stops a declined card going straight back up. See
    /// [`InteractionReader::blocking_answered_at`](turnframe_store::interaction::InteractionReader::blocking_answered_at).
    ///
    /// # Errors
    ///
    /// See [`Self::open_for_conversation`].
    pub async fn blocking_answered_at(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        revision: turnframe_core::ids::CaseRevision,
    ) -> Result<bool, InteractionError> {
        self.store
            .blocking_answered_at(account, case_key, revision)
            .await
            .map_err(|_| InteractionError::NotPersisted)
    }

    /// One stored card, whatever its status.
    ///
    /// # Errors
    ///
    /// See [`Self::open_for_conversation`]; an identifier of another tenant is
    /// [`InteractionRejection::NotFound`].
    pub async fn get(
        &self,
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, InteractionError> {
        self.store
            .get(account, id)
            .await
            .map_err(|error| match error {
                StoreError::NotFound => InteractionError::Rejected(InteractionRejection::NotFound),
                _ => InteractionError::NotPersisted,
            })
    }
}

/// The dimensions every settlement signal carries: the workflow the card sits
/// on and the shape of the card. Never the card identifier.
fn settlement_labels(card: &Interaction) -> SignalLabels {
    SignalLabels::workflow(card.case_ref.workflow.clone()).with_interaction(card.kind)
}

/// Reports a card that reached a terminal status, as the store left it
/// (spec §15.5, §26.2).
///
/// Only the two terminal statuses are counted.
/// [`RestoreActive`](ResolutionOutcome::RestoreActive) puts the card back in
/// front of the user, which is neither an abandonment nor a resolution, and
/// counting it as either would move the abandonment panel for a card the user
/// is about to answer.
pub(crate) fn observe_settled(observer: &dyn Observer, settled: &InteractionRecord) {
    let card = &settled.interaction;
    let labels = settlement_labels(card);
    match card.status {
        InteractionStatus::Resolved => {
            observer.observe_labeled(&Signal::InteractionResolved, &labels);
        }
        InteractionStatus::Failed => {
            let code = settled
                .failure_code
                .clone()
                .unwrap_or_else(|| String::from("not_committed"));
            observer.observe_labeled(&Signal::InteractionFailed, &labels.with_error_code(code));
        }
        _ => {}
    }
}

/// The same, for a card settled inside a turn's commit bundle.
///
/// The bundle is the one write of the turn (spec §16.3), so there is no
/// [`InteractionRecord`] to read back: the outcome the bundle carried *is* what
/// the card became, and the caller emits this only once the bundle landed.
pub(crate) fn observe_resolution(
    observer: &dyn Observer,
    card: &Interaction,
    outcome: &ResolutionOutcome,
) {
    let labels = settlement_labels(card);
    match outcome {
        ResolutionOutcome::Resolved { .. } => {
            observer.observe_labeled(&Signal::InteractionResolved, &labels);
        }
        ResolutionOutcome::Failed { code } => {
            observer.observe_labeled(
                &Signal::InteractionFailed,
                &labels.with_error_code(code.clone()),
            );
        }
        ResolutionOutcome::RestoreActive => {}
    }
}

/// A store refusal while writing a card is never anything but "the card is not
/// there": the caller's only correct reaction is to say nothing about it.
fn persistence_failure(_error: StoreError) -> InteractionError {
    InteractionError::NotPersisted
}

/// The summary the reducer needs about one stored card.
#[must_use]
pub fn summarize(interaction: &Interaction) -> ActiveInteractionSummary {
    ActiveInteractionSummary {
        interaction_id: interaction.id,
        case_ref: interaction.case_ref.clone(),
        kind: interaction.kind,
        blocking: interaction.blocking,
        option_ids: interaction.payload.option_ids(),
        text_resolution: interaction.text_resolution.clone(),
        confirms_risk: interaction.confirms_risk,
        payload_hash: interaction.payload_hash.clone(),
    }
}

/// The summaries of a set of stored cards, in the order given.
#[must_use]
pub fn summarize_all(interactions: &[Interaction]) -> Vec<ActiveInteractionSummary> {
    interactions.iter().map(summarize).collect()
}

#[cfg(test)]
mod tests {
    use turnframe_core::case::CaseRef;
    use turnframe_core::ids::{CaseRevision, OptionId};
    use turnframe_core::interaction::{
        InteractionKind, InteractionOption, InteractionPayload, StoredInteractionAction,
    };
    use turnframe_store::memory::MemoryStores;

    use super::*;

    fn engine() -> (InteractionEngine, Arc<MemoryStores>) {
        let memory = Arc::new(MemoryStores::new());
        let engine = InteractionEngine::new(
            Arc::clone(&memory) as Arc<dyn InteractionStore>,
            InteractionConfig::conservative(),
        );
        (engine, memory)
    }

    fn case() -> CaseRef {
        CaseRef::new("trip", "trip-1", CaseRevision(3))
    }

    fn spec(key: &str) -> InteractionSpec {
        InteractionSpec::new(
            key,
            case(),
            InteractionKind::SingleSelect,
            InteractionPayload::new("Which one?").with_option(InteractionOption::new(
                OptionId::from("a"),
                "The first",
                StoredInteractionAction::ResolveClarification {
                    answer_key: "a".to_owned(),
                },
            )),
        )
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("a valid fixed instant")
    }

    #[tokio::test]
    async fn a_second_blocking_card_replaces_and_invalidates_the_first() {
        let (engine, _memory) = engine();
        let account = AccountId::from("aurora");
        let conversation = ConversationId::nil();

        let (first, gone) = engine
            .create(
                spec("first"),
                &account,
                conversation,
                TurnId::from(uuid::Uuid::from_u128(1)),
                now(),
            )
            .await
            .expect("the slot was free");
        assert!(gone.is_empty());

        let (second, gone) = engine
            .create(
                spec("second"),
                &account,
                conversation,
                TurnId::from(uuid::Uuid::from_u128(2)),
                now(),
            )
            .await
            .expect("the occupant is replaceable");
        assert_eq!(
            gone,
            vec![first.id],
            "the previous blocking card was invalidated (I5, §15.6)"
        );
        assert_ne!(second.id, first.id, "and the replacement is a new card");

        let open = engine
            .open_for_case(&account, &case().key())
            .await
            .expect("the store answers");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, second.id);
    }

    #[tokio::test]
    async fn a_card_is_resolved_only_once_its_command_committed() {
        let (engine, _memory) = engine();
        let account = AccountId::from("aurora");
        let conversation = ConversationId::nil();
        let turn = TurnId::from(uuid::Uuid::from_u128(1));
        let (card, _) = engine
            .create(spec("first"), &account, conversation, turn, now())
            .await
            .expect("the card is written");

        let actor = ActorContext::new(account.clone(), "u1");
        let response = InteractionResponse {
            interaction_id: card.id,
            option_id: OptionId::from("a"),
            expected_case_revision: CaseRevision(3),
            freeform_input: None,
        };
        let admitted = engine
            .accept(
                ResponseContext::click(&actor, &conversation, turn, CaseRevision(3), now()),
                &response,
            )
            .await
            .expect("the answer is valid");
        let accepted = admitted.accepted().expect("it was accepted");
        assert_eq!(
            accepted.record.status(),
            InteractionStatus::Resolving,
            "answering starts a resolution; it does not finish one"
        );

        // A second click loses the compare-and-set and never authorizes a
        // second execution (I14).
        let again = engine
            .accept(
                ResponseContext::click(
                    &actor,
                    &conversation,
                    TurnId::from(uuid::Uuid::from_u128(2)),
                    CaseRevision(3),
                    now(),
                ),
                &response,
            )
            .await
            .expect("the second click is answered, not accepted");
        assert!(again.is_replay());

        let settled = engine
            .mark_resolved(&account, &card.id, Vec::new())
            .await
            .expect("the command committed");
        assert_eq!(settled.status(), InteractionStatus::Resolved);
    }

    #[tokio::test]
    async fn a_card_of_another_tenant_is_simply_not_there() {
        let (engine, _memory) = engine();
        let (card, _) = engine
            .create(
                spec("first"),
                &AccountId::from("aurora"),
                ConversationId::nil(),
                TurnId::nil(),
                now(),
            )
            .await
            .expect("the card is written");

        let stranger = engine.get(&AccountId::from("other"), &card.id).await;
        let unknown = engine
            .get(
                &AccountId::from("other"),
                &InteractionId::from(uuid::Uuid::from_u128(999)),
            )
            .await;
        assert_eq!(
            format!("{stranger:?}"),
            format!("{unknown:?}"),
            "another tenant's card and one that never existed must be one answer (§25.4)"
        );
    }

    #[test]
    fn interaction_ids_are_derived_and_stable() {
        let turn = TurnId::nil();
        assert_eq!(
            derive_interaction_id(&turn, "confirm:acts[0]"),
            derive_interaction_id(&turn, "confirm:acts[0]"),
        );
        assert_ne!(
            derive_interaction_id(&turn, "confirm:acts[0]"),
            derive_interaction_id(&turn, "confirm:acts[1]"),
        );
    }
}
