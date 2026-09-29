//! Complete sample domains, and the pieces they share: a travel-disruption desk.
//!
//! Three workflows ship with the kit and are meant to be reused by every other
//! crate, by the examples and by integration tests:
//!
//! * [`trip`]: the disruption case of one booking, with obligations open at once,
//!   one of them per extra; a rebooking card bound to the case revision and to the
//!   quote it shows; an airline that may answer late or never, whose states are
//!   never collapsed into "done"; and a leg the domain refuses to change once the
//!   traveler asked to keep it;
//! * [`traveler`]: the flat onboarding slice, where deleting is destructive and a
//!   new email is a sensitive change behind a review card, and where one field is
//!   three-valued (untouched, answered, declined) because a two-valued field cannot
//!   tell "we have not asked" from "they said no";
//! * [`claim`]: a receipt arrives, values are read from it and held as a proposal,
//!   and a review card turns the proposal into state: the worked recipe for
//!   *proposed values awaiting review*, which is domain state rather than framework
//!   vocabulary.
//!
//! All three render a receipt for a
//! [`ReceiptEvent::Redacted`](turnframe_core::event::ReceiptEvent), because a turn
//! that quietly drops one reads as a turn in which nothing happened. The copy says
//! the least any of them could honestly say: the step is on record, its detail was
//! erased. The receipt still cites the event, so it still passes the claim guard,
//! which `tests/claim_guard.rs` pins.
//!
//! Each implements [`PureWorkflow`], so the same pure `apply` drives the
//! [`InMemoryExecutor`] of the integration tests and the
//! [`WorkflowModel`](crate::explore::WorkflowModel) of exploration.

pub mod claim;
pub mod traveler;
pub mod trip;

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use turnframe_core::case::Versioned;
use turnframe_core::command::{AtomicityScope, CommandBatch, IdempotencyKey};
use turnframe_core::error::{DomainRejection, ExecutionError, RevisionConflict, StoreError};
use turnframe_core::event::{Commit, CommittedEvent};
use turnframe_core::flow::{WorkflowDefinition, WorkflowExecutor};
use turnframe_core::hash::{Digest, canonical_digest, derive_uuid};
use turnframe_core::ids::{AccountId, CaseId, CaseRevision, EventId};

use crate::explore::SimulatedTransition;

/// The state and the events one command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied<S, E> {
    /// State after the command.
    pub state: S,
    /// Events the executor commits, in order.
    pub events: Vec<E>,
}

impl<S, E> Applied<S, E> {
    /// Pairs a state with its events.
    #[must_use]
    pub const fn new(state: S, events: Vec<E>) -> Self {
        Self { state, events }
    }
}

/// A workflow whose transitions are one pure function.
///
/// Splitting this out of [`WorkflowDefinition`] keeps the definition free of
/// execution concerns while letting the kit derive both an executor and an
/// exploration model from a single description of what a command does.
pub trait PureWorkflow: WorkflowDefinition {
    /// Applies one command to one state, refusing it the way the real domain
    /// would. Must be deterministic and must not mutate `state`.
    fn apply(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<Applied<Self::State, Self::Event>, DomainRejection>;

    /// Stable event type label of an event, e.g. `"trip.extra_added"`.
    fn event_type(&self, event: &Self::Event) -> String;
}

/// The operations with the Italian summary `summaries` gives each by key, for a turn in
/// Italian: a deployment writes its summaries in every language it serves.
pub(crate) fn in_italian(
    specs: Vec<turnframe_core::operation::OperationSpec>,
    summaries: &[(&str, &str)],
) -> Vec<turnframe_core::operation::OperationSpec> {
    specs
        .into_iter()
        .map(
            |spec| match summaries.iter().find(|(key, _)| spec.key.as_str() == *key) {
                Some((_, italian)) => spec.summary_in("it-IT", *italian),
                None => spec,
            },
        )
        .collect()
}

/// Turns [`PureWorkflow::apply`] into a [`SimulatedTransition`], so a model
/// only has to describe its initial states and candidate commands.
pub fn simulate<W: PureWorkflow>(
    definition: &W,
    state: Option<&W::State>,
    command: &W::Command,
) -> SimulatedTransition<W::State, W::Event> {
    match definition.apply(state, command) {
        Ok(applied) => SimulatedTransition::applied(applied.state, applied.events),
        Err(rejection) => SimulatedTransition::rejected(rejection),
    }
}

/// Domain separation of the derived event identifiers.
const EVENT_ID_DOMAIN: &str = "turnframe.test.event.v1";

/// First second of the executor's synthetic clock (2023-11-14T22:13:20Z).
const CLOCK_EPOCH_SECONDS: i64 = 1_700_000_000;

/// What one already-executed envelope remembers.
///
/// The unit is the **envelope**, not the batch. Keying the memory on the batch
/// would make a batch that half-executed unrepresentable: its first command
/// really did commit, and pretending otherwise is exactly the crash-recovery
/// bug the executor exists to model (spec §23.1).
#[derive(Clone)]
struct Replayable<S, E> {
    /// Digest of *this envelope's* command, so a key reused with a different
    /// command is a mismatch (I14).
    command_digest: Digest,
    /// Revision the case was at when the batch that contains this envelope
    /// started.
    revision_before: CaseRevision,
    /// Events this envelope committed.
    events: Vec<CommittedEvent<E>>,
    /// State after this envelope.
    state_after: S,
}

struct Store<S, E> {
    cases: HashMap<(AccountId, CaseId), Versioned<S>>,
    /// Every case, in the order it first appeared.
    created: Vec<(AccountId, CaseId)>,
    replays: HashMap<IdempotencyKey, Replayable<S, E>>,
    sequence: u64,
}

/// An in-memory [`WorkflowExecutor`] for any [`PureWorkflow`].
///
/// It does the things an executor must do and nothing else:
///
/// * it refuses a batch whose expected revision is not the current one
///   ([`ExecutionError::RevisionConflict`], I13);
/// * it replays known idempotency keys instead of repeating the effect, and
///   refuses a key reused with a different command
///   ([`ExecutionError::IdempotencyMismatch`], I14);
/// * it resumes a batch that only half-executed, replaying the prefix and
///   executing the rest;
/// * it emits one [`CommittedEvent`] per event the pure `apply` produced.
///
/// Event identifiers and timestamps are derived, not random: two runs of the
/// same sequence of batches produce byte-identical commits, which is what makes
/// snapshot and replay assertions possible.
///
/// # A batch owns one revision
///
/// A batch moves the case from `expected_revision` to `expected_revision + 1`
/// however many envelopes it carries, and a resumed batch lands on the *same*
/// revision the interrupted one reached. That is what makes
/// [`execute_prefix`](Self::execute_prefix) followed by
/// [`execute`](WorkflowExecutor::execute) indistinguishable, from the case's
/// point of view, from one uninterrupted `execute` — which is the property
/// crash recovery relies on.
pub struct InMemoryExecutor<W: PureWorkflow> {
    definition: W,
    store: Mutex<Store<W::State, W::Event>>,
}

impl<W: PureWorkflow> fmt::Debug for InMemoryExecutor<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryExecutor")
            .field("workflow", &self.definition.key())
            .field("version", &self.definition.version())
            .finish_non_exhaustive()
    }
}

impl<W: PureWorkflow + Default> Default for InMemoryExecutor<W> {
    fn default() -> Self {
        Self::new(W::default())
    }
}

impl<W: PureWorkflow> InMemoryExecutor<W> {
    /// Builds an empty executor for `definition`.
    #[must_use]
    pub fn new(definition: W) -> Self {
        Self {
            definition,
            store: Mutex::new(Store {
                cases: HashMap::new(),
                created: Vec::new(),
                replays: HashMap::new(),
                sequence: 0,
            }),
        }
    }

    /// The definition transitions are validated against.
    #[must_use]
    pub const fn definition(&self) -> &W {
        &self.definition
    }

    /// Locks the store, recovering from a poisoned mutex rather than panicking:
    /// a test that already failed must not cascade into unrelated failures.
    fn store(&self) -> std::sync::MutexGuard<'_, Store<W::State, W::Event>> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Installs a case at a chosen revision, bypassing commands. Use it to
    /// start a test from a state that would take many turns to reach.
    pub fn seed(
        &self,
        account: &AccountId,
        case_id: &CaseId,
        state: W::State,
        revision: CaseRevision,
    ) {
        let key = (account.clone(), case_id.clone());
        let mut store = self.store();
        store.remember(&key);
        store.cases.insert(key, Versioned::new(state, revision));
    }

    /// The account's cases, in the order each first appeared: what a directory over
    /// this executor lists.
    #[must_use]
    pub fn case_ids(&self, account: &AccountId) -> Vec<CaseId> {
        self.store()
            .created
            .iter()
            .filter(|(owner, _)| owner == account)
            .map(|(_, case_id)| case_id.clone())
            .collect()
    }

    /// The current revision of a case, or [`CaseRevision::ZERO`] when it does
    /// not exist for this account.
    #[must_use]
    pub fn revision_of(&self, account: &AccountId, case_id: &CaseId) -> CaseRevision {
        self.store()
            .cases
            .get(&(account.clone(), case_id.clone()))
            .map_or(CaseRevision::ZERO, |case| case.revision)
    }

    /// The state of a case, for a test that asserts on what a turn wrote.
    #[must_use]
    pub fn state_of(&self, account: &AccountId, case_id: &CaseId) -> Option<W::State> {
        self.store()
            .cases
            .get(&(account.clone(), case_id.clone()))
            .map(|case| case.value.clone())
    }

    /// Number of distinct cases stored.
    #[must_use]
    pub fn case_count(&self) -> usize {
        self.store().cases.len()
    }

    /// Returns `true` when this idempotency key has already executed.
    #[must_use]
    pub fn has_executed(&self, key: &IdempotencyKey) -> bool {
        self.store().replays.contains_key(key)
    }

    /// How many envelopes at the front of `batch` already executed.
    ///
    /// `0` means the batch is untouched, `batch.envelopes.len()` that the whole
    /// batch is a replay, anything between that it was interrupted.
    #[must_use]
    pub fn replayed_prefix_of(&self, batch: &CommandBatch<W::Command>) -> usize {
        let store = self.store();
        batch
            .envelopes
            .iter()
            .take_while(|envelope| store.replays.contains_key(&envelope.idempotency_key))
            .count()
    }

    /// Executes only the first `applied` envelopes of `batch`, as a process
    /// that died mid-batch would have left it.
    ///
    /// The case moves to `expected_revision + 1` and the applied envelopes are
    /// remembered, so executing the whole batch afterwards replays them and
    /// runs only the rest. This is how a test *creates* a partially replayed
    /// batch; nothing else in the kit produces one.
    ///
    /// # Errors
    ///
    /// [`ExecutionError::ScopeViolation`] when `applied` is zero or larger than
    /// the batch, plus everything [`execute`](WorkflowExecutor::execute) can
    /// return.
    pub fn execute_prefix(
        &self,
        batch: &CommandBatch<W::Command>,
        applied: usize,
    ) -> Result<Commit<W::State, W::Event>, ExecutionError> {
        if applied == 0 || applied > batch.envelopes.len() {
            return Err(ExecutionError::ScopeViolation);
        }
        self.run(batch, applied)
    }

    /// The whole batch, or as much of it as `limit` allows.
    fn run(
        &self,
        batch: &CommandBatch<W::Command>,
        limit: usize,
    ) -> Result<Commit<W::State, W::Event>, ExecutionError> {
        let first = batch
            .envelopes
            .first()
            .ok_or(ExecutionError::ScopeViolation)?;
        if matches!(batch.scope, AtomicityScope::PerCase) && !batch.is_single_case() {
            return Err(ExecutionError::ScopeViolation);
        }
        let key = (first.account_id().clone(), first.case_ref.case_id.clone());
        let mut store = self.store();
        let envelopes = &batch.envelopes[..limit.min(batch.envelopes.len())];

        // What of this batch already executed, and does it still mean the same?
        let mut recorded: Vec<Option<Replayable<W::State, W::Event>>> =
            Vec::with_capacity(envelopes.len());
        for envelope in envelopes {
            let digest = canonical_digest(&envelope.command)
                .map_err(|_| ExecutionError::Store(StoreError::Serialization))?;
            match store.replays.get(&envelope.idempotency_key) {
                None => recorded.push(None),
                Some(entry) if entry.command_digest == digest => recorded.push(Some(entry.clone())),
                Some(_) => {
                    return Err(ExecutionError::IdempotencyMismatch {
                        command_id: envelope.command_id,
                    });
                }
            }
        }
        let replayed = recorded.iter().take_while(|entry| entry.is_some()).count();
        if let Some(position) = recorded[replayed..].iter().position(Option::is_some) {
            // A key from the middle of the batch executed while an earlier one
            // did not: these envelopes never travelled together.
            return Err(ExecutionError::IdempotencyMismatch {
                command_id: envelopes[replayed + position].command_id,
            });
        }

        let current = store.cases.get(&key).cloned();
        let current_revision = current.as_ref().map_or(CaseRevision::ZERO, |c| c.revision);
        let conflict = |current_revision| {
            ExecutionError::RevisionConflict(RevisionConflict {
                expected: first.case_ref.clone(),
                current_revision,
            })
        };

        let last_replayed = recorded[..replayed].last().and_then(Option::as_ref);
        let (mut state, revision_before, mut events) = match last_replayed {
            // Nothing of this batch ran yet: the ordinary optimistic check.
            None => {
                if current_revision != first.case_ref.expected_revision {
                    return Err(conflict(current_revision));
                }
                (current.map(|c| c.value), current_revision, Vec::new())
            }
            // The batch was interrupted. Its own revision is the one to check
            // against, and the case must still be where the interrupted batch
            // left it — otherwise something else wrote in between and resuming
            // would silently overwrite it.
            Some(entry) => {
                if entry.revision_before != first.case_ref.expected_revision
                    || current_revision != entry.revision_before.next()
                {
                    return Err(conflict(current_revision));
                }
                let events = recorded[..replayed]
                    .iter()
                    .flatten()
                    .flat_map(|entry| entry.events.clone())
                    .collect();
                (
                    Some(entry.state_after.clone()),
                    entry.revision_before,
                    events,
                )
            }
        };

        if replayed == envelopes.len() {
            // Every envelope is a replay: return the original outcome without
            // repeating a single effect (I14).
            let Some(committed) = state else {
                return Err(ExecutionError::ScopeViolation);
            };
            return Ok(Commit {
                state: Some(committed),
                new_revision: revision_before.next(),
                events,
                idempotency_replay: true,
            });
        }

        let new_revision = revision_before.next();
        let mut fresh = Vec::new();
        for envelope in &envelopes[replayed..] {
            self.definition
                .validate_command(state.as_ref(), &envelope.command)
                .map_err(ExecutionError::Rejected)?;
            let applied = self
                .definition
                .apply(state.as_ref(), &envelope.command)
                .map_err(ExecutionError::Rejected)?;
            let mut committed_events = Vec::with_capacity(applied.events.len());
            for payload in applied.events {
                let event_type = self.definition.event_type(&payload);
                let (sequence, occurred_at) = store.tick();
                committed_events.push(CommittedEvent {
                    event_id: EventId::from(derive_uuid(
                        EVENT_ID_DOMAIN,
                        &[
                            key.0.as_str(),
                            key.1.as_str(),
                            &sequence.to_string(),
                            &event_type,
                        ],
                    )),
                    event_type,
                    occurred_at,
                    payload,
                });
            }
            let digest = canonical_digest(&envelope.command)
                .map_err(|_| ExecutionError::Store(StoreError::Serialization))?;
            state = Some(applied.state.clone());
            fresh.push((
                envelope.idempotency_key.clone(),
                Replayable {
                    command_digest: digest,
                    revision_before,
                    events: committed_events.clone(),
                    state_after: applied.state,
                },
            ));
            events.extend(committed_events);
        }

        let Some(committed) = state else {
            return Err(ExecutionError::ScopeViolation);
        };
        store.remember(&key);
        store
            .cases
            .insert(key, Versioned::new(committed.clone(), new_revision));
        for (idempotency_key, entry) in fresh {
            store.replays.insert(idempotency_key, entry);
        }
        Ok(Commit {
            state: Some(committed),
            new_revision,
            events,
            // Part of the batch came back from the journal rather than from the
            // domain, which is what a caller has to know before it renders a
            // receipt for "what just happened".
            idempotency_replay: replayed > 0,
        })
    }
}

impl<S, E> Store<S, E> {
    fn remember(&mut self, key: &(AccountId, CaseId)) {
        if !self.cases.contains_key(key) {
            self.created.push(key.clone());
        }
    }

    /// The next synthetic instant, one second after the previous one.
    fn tick(&mut self) -> (u64, DateTime<Utc>) {
        self.sequence += 1;
        let seconds = CLOCK_EPOCH_SECONDS.saturating_add(self.sequence as i64);
        (
            self.sequence,
            DateTime::from_timestamp(seconds, 0).unwrap_or_default(),
        )
    }
}

#[async_trait::async_trait]
impl<W: PureWorkflow> WorkflowExecutor<W> for InMemoryExecutor<W> {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<W::State>>, StoreError> {
        Ok(self
            .store()
            .cases
            .get(&(account.clone(), case_id.clone()))
            .map_or_else(
                || Versioned::new(None, CaseRevision::ZERO),
                |case| Versioned::new(Some(case.value.clone()), case.revision),
            ))
    }

    async fn execute(
        &self,
        batch: CommandBatch<W::Command>,
    ) -> Result<Commit<W::State, W::Event>, ExecutionError> {
        self.run(&batch, batch.envelopes.len())
    }
}
