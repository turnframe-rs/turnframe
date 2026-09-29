//! Command execution: the journal, the domain, the outbox and the one atomic
//! write (spec §16, §23 steps M and N).
//!
//! Everything the runtime does that a user could notice happens here, and it
//! happens in a fixed order that the rest of the library depends on:
//!
//! 1. **Admission before effect.** Every envelope is admitted to the command
//!    journal under `UNIQUE (account_id, idempotency_key)` *before* the domain
//!    is asked to do anything (§16.2). A key that is already there is not a
//!    second command: [`Admission::Settled`] returns the outcome that was
//!    recorded the first time, without running the effect again (I14).
//! 2. **Optimistic concurrency.** The batch carries the revision it was planned
//!    against and the executor checks it in the same statement that writes
//!    (§16.1, I13). A mismatch is [`CommandOutcome::RevisionConflict`], never a
//!    blind overwrite.
//! 3. **Uncertainty is a state.** A timeout after transmission is
//!    [`CommandOutcome::OutcomeUnknown`] carrying an [`AttemptId`], because the
//!    effect may exist. Nothing in this module retries it (I15, §16.5).
//! 4. **One write.** The journal outcomes, the events, the card resolutions,
//!    the new cards, the outbox rows, the replay record and the phase marker
//!    travel in one [`CommitBundle`] and land together or not at all
//!    (§16.3, §23 step N).
//!
//! # What the batch is the unit of
//!
//! A [`CommandBatch`] commits as a whole: one revision, one set of events. The
//! per-command [`CommandOutcomeRecord`]s therefore all report the same
//! revision, and the batch's event identifiers are recorded once, against its
//! first envelope, so two receipts can never cite the same events as if they
//! were two separate outcomes.
//!
//! # What is deliberately not atomic
//!
//! The domain's own state commit may live in another database, and Turnframe
//! does not attempt a distributed transaction. Safety across that seam is the
//! journal: the entry exists before the effect, the executor is idempotent on
//! the key, and [`crate::recover`] resumes a pending entry by that key
//! (§23.1).

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::command::{AtomicityScope, CommandBatch, CommandEnvelope};
use turnframe_core::error::{
    DomainRejection, ErrorClassification, ExecutionError, OrchestratorError,
};
use turnframe_core::event::{Commit, CommittedEvent, OutboxEntry, OutboxStatus};
use turnframe_core::flow::WorkflowRegistry;
use turnframe_core::hash::derive_uuid;
use turnframe_core::ids::{AccountId, AttemptId, CaseRevision, CommandId, EventId, OutboxId};
use turnframe_core::reduce::CommandRef;
use turnframe_core::replay::{CommandOutcome, CommandOutcomeRecord};
use turnframe_store::commit::{CommitBundle, CommitReceipt, CommitStore};
use turnframe_store::events::EventBatch;
use turnframe_store::journal::{
    CommandJournal, CommandJournalEntry, JournalAdmission, JournalOutcome,
};
use turnframe_store::outbox::OutboxStore;

use crate::config::ExecutionConfig;

/// Domain separation of the derived outbox identifiers.
const OUTBOX_ID_DOMAIN: &str = "turnframe.outbox_id.v1";

/// Domain separation of the derived external attempt identifiers.
const ATTEMPT_ID_DOMAIN: &str = "turnframe.attempt_id.v1";

/// Derives the outbox row identifier of `command_id`, so replaying a turn
/// enqueues the same row instead of a second one.
#[must_use]
pub fn derive_outbox_id(command_id: &CommandId) -> OutboxId {
    OutboxId::from(derive_uuid(OUTBOX_ID_DOMAIN, &[&command_id.to_string()]))
}

/// Derives the attempt identifier a command's unknown outcome is reconciled by.
///
/// It names one attempt at one external effect, which is what a reconciler
/// quotes back to the remote system (§16.5, I15).
#[must_use]
pub fn derive_attempt_id(command_id: &CommandId) -> AttemptId {
    AttemptId::new(
        derive_uuid(ATTEMPT_ID_DOMAIN, &[&command_id.to_string()])
            .simple()
            .to_string(),
    )
}

/// A stable label for an erased command, for the journal's `command_type`.
///
/// Erased commands are the JSON a domain's own enum serializes to, so the
/// externally tagged single-key object and the unit-variant string both name
/// their variant; anything else is labelled by the workflow alone rather than
/// by a guess.
#[must_use]
pub fn command_type(case_ref: &CaseRef, command: &serde_json::Value) -> String {
    let workflow = case_ref.workflow.as_str();
    match command {
        serde_json::Value::String(variant) => format!("{workflow}.{variant}"),
        serde_json::Value::Object(map) if map.len() == 1 => match map.keys().next() {
            Some(variant) => format!("{workflow}.{variant}"),
            None => format!("{workflow}.command"),
        },
        _ => format!("{workflow}.command"),
    }
}

/// What the journal said about one envelope before it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Admission {
    /// The key is new; the entry is now persisted as `Pending`.
    Fresh,
    /// The key exists in `Pending` or `Executing`: a previous attempt was
    /// interrupted and this one resumes it by key (§23.1).
    Resume,
    /// The key exists with a recorded outcome. The original outcome is
    /// returned and nothing runs again (I14).
    Settled(Box<JournalOutcome>),
}

impl Admission {
    /// Returns `true` when the domain still has to be called.
    #[must_use]
    pub const fn needs_execution(&self) -> bool {
        matches!(self, Self::Fresh | Self::Resume)
    }
}

/// What executing one turn's batches produced.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ExecutionReport {
    /// One record per envelope, in batch then envelope order.
    pub outcomes: Vec<CommandOutcomeRecord>,
    /// Event batches to append, one per committed command batch.
    pub events: Vec<EventBatch>,
    /// Every committed event, in commit order, for receipts (§17.3).
    pub committed: Vec<CommittedEvent<serde_json::Value>>,
    /// Cases whose revision moved, with the revision they moved to.
    pub changed: IndexMap<CaseKey, CaseRevision>,
    /// Outbox rows for the external effects that were accepted locally (§16.4).
    pub outbox: Vec<OutboxEntry>,
    /// Journal completions for the commit bundle.
    pub completions: Vec<(CommandId, JournalOutcome)>,
    /// Domain rejections this stage decided, with the case each was aimed at. Some can only be
    /// decided here, such as a name no registry entry matches; each reaches the writing stage
    /// with the domain's explanation, as a reducer refusal does, so the reply cannot claim it.
    pub rejections: Vec<(CaseRef, DomainRejection)>,
}

impl ExecutionReport {
    /// Folds an earlier report of the same turn into this one.
    ///
    /// A turn that commits twice executed twice, and everything downstream of
    /// the commit reads one report: the receipts the writing stage may rest on,
    /// the refusals it must not contradict, and whether anything failed. So the
    /// halves are joined **after** the second commit and never before — a bundle
    /// built from a joined report would append the first half's events a second
    /// time.
    ///
    /// `earlier` goes first in every list, because commit order is the order
    /// receipts are read in. A case that moved in both halves keeps the later
    /// revision, which is the one it is at.
    pub fn absorb(&mut self, earlier: Self) {
        let mut merged = earlier;
        merged.outcomes.append(&mut self.outcomes);
        merged.events.append(&mut self.events);
        merged.committed.append(&mut self.committed);
        merged.outbox.append(&mut self.outbox);
        merged.completions.append(&mut self.completions);
        merged.rejections.append(&mut self.rejections);
        for (key, revision) in std::mem::take(&mut self.changed) {
            merged.changed.insert(key, revision);
        }
        *self = merged;
    }

    /// Every event identifier the turn committed, in commit order.
    #[must_use]
    pub fn event_ids(&self) -> Vec<EventId> {
        self.committed.iter().map(|event| event.event_id).collect()
    }

    /// Returns `true` when every command committed.
    #[must_use]
    pub fn all_committed(&self) -> bool {
        !self.outcomes.is_empty()
            && self
                .outcomes
                .iter()
                .all(|record| matches!(record.outcome, CommandOutcome::Committed { .. }))
    }

    /// Returns `true` when at least one command committed.
    #[must_use]
    pub fn any_committed(&self) -> bool {
        self.outcomes
            .iter()
            .any(|record| matches!(record.outcome, CommandOutcome::Committed { .. }))
    }

    /// Returns `true` when a command ended with an effect that may or may not
    /// have happened, so the turn must say "verification in progress" and a
    /// reconciler must settle it (I15).
    #[must_use]
    pub fn has_unknown_outcome(&self) -> bool {
        self.outcomes
            .iter()
            .any(|record| matches!(record.outcome, CommandOutcome::OutcomeUnknown { .. }))
    }

    /// The attempts a reconciler has to settle.
    #[must_use]
    pub fn pending_attempts(&self) -> Vec<AttemptId> {
        self.outcomes
            .iter()
            .filter_map(|record| match &record.outcome {
                CommandOutcome::OutcomeUnknown { attempt_id } => Some(attempt_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// Returns `true` when no command committed and at least one failed, which
    /// is what makes a receipt impossible.
    #[must_use]
    pub fn failed_outright(&self) -> bool {
        !self.outcomes.is_empty() && !self.any_committed()
    }

    /// Returns `true` when any command did **not** commit — a rejection, a
    /// conflict, a failure, or an outcome nobody knows yet.
    ///
    /// This, rather than [`Self::failed_outright`], is what makes a notice
    /// mandatory: a turn where two commands committed and a third did not is
    /// still a turn that has to say so, and receipts alone would let the user
    /// read the silence as success.
    #[must_use]
    pub fn any_uncommitted(&self) -> bool {
        self.outcomes
            .iter()
            .any(|record| !matches!(record.outcome, CommandOutcome::Committed { .. }))
    }

    /// The bundle items this execution produced: journal completions, events
    /// and outbox rows.
    ///
    /// The caller adds the card resolutions, the new cards, the replay record
    /// and the phase marker, then writes everything at once (§16.3).
    #[must_use]
    pub fn bundle(&self) -> CommitBundle {
        let mut bundle = CommitBundle::new();
        for (command_id, outcome) in &self.completions {
            bundle = bundle.with_journal_completion(*command_id, outcome.clone());
        }
        for batch in &self.events {
            bundle = bundle.with_events(batch.clone());
        }
        for entry in &self.outbox {
            bundle = bundle.with_outbox_entry(entry.clone());
        }
        bundle
    }
}

/// Runs command batches against the domain, the journal and the outbox.
#[derive(Clone)]
pub struct CommandExecutor {
    workflows: Arc<WorkflowRegistry>,
    journal: Arc<dyn CommandJournal>,
    commit: Arc<dyn CommitStore>,
    outbox: Arc<dyn OutboxStore>,
    config: ExecutionConfig,
}

impl fmt::Debug for CommandExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandExecutor")
            .field("workflows", &self.workflows.len())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl CommandExecutor {
    /// Builds an executor.
    #[must_use]
    pub fn new(
        workflows: Arc<WorkflowRegistry>,
        journal: Arc<dyn CommandJournal>,
        commit: Arc<dyn CommitStore>,
        outbox: Arc<dyn OutboxStore>,
        config: ExecutionConfig,
    ) -> Self {
        Self {
            workflows,
            journal,
            commit,
            outbox,
            config,
        }
    }

    /// The execution configuration in force.
    #[must_use]
    pub const fn config(&self) -> &ExecutionConfig {
        &self.config
    }

    /// Admits the commands a confirmation card will authorize, in `Pending`
    /// (spec §15.3).
    ///
    /// They do not run: the card names them, and a later click resumes exactly
    /// these entries by idempotency key. Re-journaling the same entry (a
    /// replayed turn) is accepted and changes nothing.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`] when the journal could not be written.
    pub async fn journal_pending(
        &self,
        batches: &[CommandBatch<serde_json::Value>],
        now: DateTime<Utc>,
    ) -> Result<Vec<CommandId>, OrchestratorError> {
        let mut admitted = Vec::new();
        for batch in batches {
            for envelope in &batch.envelopes {
                let mut entry = self.entry_for(envelope, now)?;
                entry.status = turnframe_store::journal::CommandJournalStatus::AwaitingConfirmation;
                match self.journal.begin(entry).await {
                    Ok(_) => admitted.push(envelope.command_id),
                    Err(error) => return Err(OrchestratorError::Store(error)),
                }
            }
        }
        Ok(admitted)
    }

    /// Rebuilds the batches a confirmed card authorized, from the journal
    /// entries the card names (spec §15.3).
    ///
    /// The commands come back exactly as they were reviewed, with the
    /// idempotency key they were admitted under, so answering the card twice
    /// cannot execute twice. The `origin` replaces the one recorded at
    /// admission: the authority is now the click, and policy is re-checked
    /// against it.
    ///
    /// The rebuilt batches carry [`AtomicityScope::PerCase`], which is what a
    /// reviewed set has to be: the user confirmed one change to one case, so
    /// its commands commit together or not at all.
    ///
    /// Every rebuilt envelope is policed again before it is returned: the
    /// domain's [`CommandPolicy`](turnframe_core::command::CommandPolicy) for
    /// the command, against the origin the click minted
    /// ([`origin_satisfies`](turnframe_core::command::origin_satisfies)). The
    /// card was built for exactly these commands under exactly that policy, so
    /// the check should never fire — which is the point of running it. A
    /// command it refuses is dropped rather than executed.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`] when an entry named by the card is not in
    /// the journal for this account.
    pub async fn resume_confirmed(
        &self,
        account: &AccountId,
        command_refs: &[CommandRef],
        origin: &turnframe_core::command::CommandOrigin,
        actor: &turnframe_core::turn::ActorContext,
        turn_id: turnframe_core::ids::TurnId,
    ) -> Result<Vec<CommandBatch<serde_json::Value>>, OrchestratorError> {
        let mut batches: IndexMap<turnframe_core::ids::BatchId, CommandBatch<serde_json::Value>> =
            IndexMap::new();
        for command_ref in command_refs {
            let entry = self
                .journal
                .get(account, &command_ref.command_id)
                .await
                .map_err(OrchestratorError::Store)?;
            if entry.status.is_terminal() {
                // The card is being answered a second time; the original
                // outcome stands and nothing is rebuilt for it (I14).
                continue;
            }
            if !self.authorizes(account, &entry, origin).await {
                tracing::warn!(
                    target: "turnframe.execute",
                    "a confirmed command's policy refuses the origin that confirmed it; dropped"
                );
                continue;
            }
            let envelope = CommandEnvelope {
                command_id: entry.command_id,
                turn_id,
                actor: actor.clone(),
                case_ref: entry.case_ref.clone(),
                idempotency_key: entry.idempotency_key.clone(),
                origin: origin.clone(),
                command: entry.command_payload.clone(),
            };
            batches
                .entry(command_ref.batch_id)
                .or_insert_with(|| CommandBatch {
                    batch_id: command_ref.batch_id,
                    scope: AtomicityScope::PerCase,
                    envelopes: Vec::new(),
                })
                .envelopes
                .push(envelope);
        }
        Ok(batches.into_values().collect())
    }

    /// Whether the click that answered a card actually satisfies the policy of
    /// the command it is about to run (I12).
    ///
    /// The policy is asked for against the case's **current** state, because
    /// that is the state the command will meet. A workflow that cannot be read
    /// answers `false`: a policy nobody could consult is not a policy that was
    /// satisfied (I19).
    async fn authorizes(
        &self,
        account: &AccountId,
        entry: &CommandJournalEntry,
        origin: &turnframe_core::command::CommandOrigin,
    ) -> bool {
        let Ok(registered) = self.workflows.require(&entry.case_ref.workflow) else {
            return false;
        };
        let Ok(loaded) = registered
            .executor
            .load(account, &entry.case_ref.case_id)
            .await
        else {
            return false;
        };
        let Ok(policy) = registered
            .definition
            .command_policy(loaded.value.as_ref(), &entry.command_payload)
        else {
            return false;
        };
        turnframe_core::command::origin_satisfies(origin, &policy)
    }

    /// Admits one envelope to the journal, before anything runs (§16.2, I14).
    ///
    /// This is the single door every effect goes through, and it is public
    /// because an adopter driving execution themselves has to go through it
    /// too. The three answers are the whole contract: the key is new, the key
    /// is there and unsettled (resume it — never re-plan it), or the key is
    /// there with an outcome (return that outcome and run nothing).
    ///
    /// # Errors
    ///
    /// * [`OrchestratorError::Store`] when the journal could not be written;
    /// * [`OrchestratorError::Execution`] with
    ///   [`ExecutionError::IdempotencyMismatch`] when the key names a
    ///   *different* command, which is a defect nobody may guess past.
    pub async fn admit(
        &self,
        envelope: &CommandEnvelope<serde_json::Value>,
        now: DateTime<Utc>,
    ) -> Result<Admission, OrchestratorError> {
        let entry = self.entry_for(envelope, now)?;
        match self.journal.begin(entry.clone()).await {
            Ok(JournalAdmission::Fresh) => Ok(Admission::Fresh),
            Ok(JournalAdmission::Replay(existing)) => {
                if !existing.same_command(&entry) {
                    return Err(OrchestratorError::Execution(
                        ExecutionError::IdempotencyMismatch {
                            command_id: envelope.command_id,
                        },
                    ));
                }
                Ok(match (existing.status, existing.result.clone()) {
                    (status, Some(outcome)) if !status.is_pending() => {
                        Admission::Settled(Box::new(outcome))
                    }
                    _ => Admission::Resume,
                })
            }
            Err(error) => Err(OrchestratorError::Store(error)),
        }
    }

    /// Executes every batch (spec §23 step M).
    ///
    /// Batches run in order. When
    /// [`ExecutionConfig::allow_cross_case_partial_success`] is off, the first
    /// batch that does not commit stops the rest: a turn that half-happened
    /// across two cases is harder to explain than one that did not start.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`] when the journal itself could not be
    /// reached. A command that merely *failed* is not an error here: it is an
    /// outcome, and it is reported as one.
    pub async fn execute(
        &self,
        account: &AccountId,
        batches: &[CommandBatch<serde_json::Value>],
        now: DateTime<Utc>,
    ) -> Result<ExecutionReport, OrchestratorError> {
        let mut report = ExecutionReport::default();
        for batch in batches {
            if batch.is_empty() {
                continue;
            }
            self.execute_batch(account, batch, now, &mut report).await?;
            if !self.config.allow_cross_case_partial_success
                && report.outcomes.last().is_some_and(|record| {
                    !matches!(record.outcome, CommandOutcome::Committed { .. })
                })
            {
                break;
            }
        }
        Ok(report)
    }

    async fn execute_batch(
        &self,
        account: &AccountId,
        batch: &CommandBatch<serde_json::Value>,
        now: DateTime<Utc>,
        report: &mut ExecutionReport,
    ) -> Result<(), OrchestratorError> {
        let Some(first) = batch.envelopes.first() else {
            return Ok(());
        };
        let case_ref = first.case_ref.clone();
        let Ok(registered) = self.workflows.require(&case_ref.workflow) else {
            self.record_all(
                batch,
                report,
                &CommandOutcome::Failed {
                    code: "unknown_workflow".to_owned(),
                },
                None,
            );
            return Ok(());
        };

        // 1. Admission, before the domain hears about any of it (§16.2).
        let mut admissions = Vec::with_capacity(batch.envelopes.len());
        for envelope in &batch.envelopes {
            match self.admit(envelope, now).await {
                Ok(admission) => admissions.push(admission),
                // The same key names a different command: executing either one
                // would be guessing which the user meant.
                Err(OrchestratorError::Execution(ExecutionError::IdempotencyMismatch {
                    ..
                })) => {
                    self.record_all(
                        batch,
                        report,
                        &CommandOutcome::Failed {
                            code: "idempotency_mismatch".to_owned(),
                        },
                        None,
                    );
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }

        // 2. Everything already settled: return the original outcomes.
        if admissions
            .iter()
            .all(|admission| !admission.needs_execution())
        {
            for (envelope, admission) in batch.envelopes.iter().zip(&admissions) {
                let Admission::Settled(outcome) = admission else {
                    continue;
                };
                // A settled rejection read back from the journal says the same
                // thing it said the first time, so the turn can too.
                if let JournalOutcome::Rejected { rejection } = &**outcome {
                    report
                        .rejections
                        .push((envelope.case_ref.clone(), rejection.clone()));
                }
                report.outcomes.push(CommandOutcomeRecord {
                    command_ref: CommandRef {
                        batch_id: batch.batch_id,
                        command_id: envelope.command_id,
                    },
                    idempotency_key: envelope.idempotency_key.clone(),
                    case_ref: envelope.case_ref.clone(),
                    origin: Some(envelope.origin.clone()),
                    outcome: replayed_outcome(outcome),
                });
            }
            return Ok(());
        }

        // 3. Hand the batch to the domain, under its expected revision (I13).
        for envelope in &batch.envelopes {
            if let Err(error) = self
                .journal
                .mark_executing(account, &envelope.command_id)
                .await
            {
                return Err(OrchestratorError::Store(error));
            }
        }
        let executed = registered.executor.execute(batch.clone()).await;
        match executed {
            Ok(commit) => self.record_commit(account, batch, &case_ref, commit, now, report),
            Err(error) => {
                let outcome = failure_outcome(first.command_id, &error);
                self.record_all(batch, report, &outcome, Some(&error));
            }
        }
        Ok(())
    }

    fn record_commit(
        &self,
        account: &AccountId,
        batch: &CommandBatch<serde_json::Value>,
        case_ref: &CaseRef,
        commit: Commit<serde_json::Value, serde_json::Value>,
        now: DateTime<Utc>,
        report: &mut ExecutionReport,
    ) {
        let Some(first) = batch.envelopes.first() else {
            return;
        };
        let event_ids: Vec<EventId> = commit.event_ids();
        if !commit.events.is_empty() {
            report.events.push(EventBatch::new(
                account.clone(),
                case_ref.key(),
                first.command_id,
                commit.new_revision,
                commit.events.clone(),
            ));
            report.committed.extend(commit.events.iter().cloned());
        }
        report.changed.insert(case_ref.key(), commit.new_revision);

        for (position, envelope) in batch.envelopes.iter().enumerate() {
            // The batch is the unit of commit, so its events are recorded once:
            // against the first envelope. The rest report the revision they
            // reached and cite nothing, which is what keeps two receipts from
            // claiming the same events twice.
            let outcome = CommandOutcome::Committed {
                new_revision: commit.new_revision,
                event_ids: if position == 0 {
                    event_ids.clone()
                } else {
                    Vec::new()
                },
            };
            report.outcomes.push(CommandOutcomeRecord {
                command_ref: CommandRef {
                    batch_id: batch.batch_id,
                    command_id: envelope.command_id,
                },
                idempotency_key: envelope.idempotency_key.clone(),
                case_ref: envelope.case_ref.clone(),
                origin: Some(envelope.origin.clone()),
                outcome,
            });
            report.completions.push((
                envelope.command_id,
                JournalOutcome::Committed {
                    new_revision: commit.new_revision,
                    event_ids: if position == 0 {
                        event_ids.clone()
                    } else {
                        Vec::new()
                    },
                },
            ));
            if let AtomicityScope::ExternalSaga { saga } = &batch.scope {
                report.outbox.push(outbox_row(envelope, saga, now));
            }
        }
    }

    /// Records the same outcome for every envelope of a batch that did not
    /// commit, and the matching journal completion.
    fn record_all(
        &self,
        batch: &CommandBatch<serde_json::Value>,
        report: &mut ExecutionReport,
        outcome: &CommandOutcome,
        error: Option<&ExecutionError>,
    ) {
        // The domain's own words, once for the batch that carried them. Without
        // this the explanation reaches nobody and the writing stage has nothing
        // either way about the write it is about to claim.
        if let Some(ExecutionError::Rejected(rejection)) = error
            && let Some(first) = batch.envelopes.first()
        {
            report
                .rejections
                .push((first.case_ref.clone(), rejection.clone()));
        }
        for envelope in &batch.envelopes {
            report.outcomes.push(CommandOutcomeRecord {
                command_ref: CommandRef {
                    batch_id: batch.batch_id,
                    command_id: envelope.command_id,
                },
                idempotency_key: envelope.idempotency_key.clone(),
                case_ref: envelope.case_ref.clone(),
                origin: Some(envelope.origin.clone()),
                outcome: outcome.clone(),
            });
            let completion = match error {
                Some(error) => JournalOutcome::from_execution_error(envelope.command_id, error),
                None => JournalOutcome::Failed {
                    code: outcome_code(outcome),
                },
            };
            report.completions.push((envelope.command_id, completion));
        }
    }

    fn entry_for(
        &self,
        envelope: &CommandEnvelope<serde_json::Value>,
        now: DateTime<Utc>,
    ) -> Result<CommandJournalEntry, OrchestratorError> {
        let label = command_type(&envelope.case_ref, &envelope.command);
        CommandJournalEntry::from_envelope(envelope, label, now).map_err(OrchestratorError::Store)
    }

    /// Writes the bundle, all or nothing (spec §16.3, §23 step N).
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`]. A [`StoreError::Timeout`](turnframe_core::error::StoreError::Timeout) means the write
    /// *may* have landed: the caller must re-read rather than retry, and the
    /// bundle's own atomicity guarantees that whatever it finds is either all
    /// of it or none of it (§16.5).
    pub async fn commit(
        &self,
        account: &AccountId,
        bundle: CommitBundle,
    ) -> Result<CommitReceipt, OrchestratorError> {
        self.commit.commit(account, bundle).await.map_err(|error| {
            if error.reconciliation_required() {
                tracing::error!(
                    target: "turnframe.execute",
                    "commit bundle did not confirm; the turn must re-read rather than retry"
                );
            }
            OrchestratorError::Store(error)
        })
    }

    /// The outbox the dispatcher reads, for callers that reconcile from the
    /// same executor.
    #[must_use]
    pub fn outbox(&self) -> &Arc<dyn OutboxStore> {
        &self.outbox
    }

    /// The command journal.
    #[must_use]
    pub fn journal(&self) -> &Arc<dyn CommandJournal> {
        &self.journal
    }
}

/// One outbox row for an external effect that was accepted locally (§16.4).
fn outbox_row(
    envelope: &CommandEnvelope<serde_json::Value>,
    saga: &str,
    now: DateTime<Utc>,
) -> OutboxEntry {
    OutboxEntry {
        outbox_id: derive_outbox_id(&envelope.command_id),
        command_id: envelope.command_id,
        destination: saga.to_owned(),
        payload: envelope.command.clone(),
        idempotency_key: envelope.idempotency_key.clone(),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        next_attempt_at: None,
        created_at: now,
        completed_at: None,
    }
}

/// The outcome a persisted journal result stands for on a repeat.
fn replayed_outcome(outcome: &JournalOutcome) -> CommandOutcome {
    match outcome {
        JournalOutcome::Committed {
            new_revision,
            event_ids,
        } => CommandOutcome::Committed {
            new_revision: *new_revision,
            event_ids: event_ids.clone(),
        },
        JournalOutcome::Rejected { rejection } => CommandOutcome::Rejected {
            code: rejection.code.clone(),
        },
        JournalOutcome::RevisionConflict { current_revision } => CommandOutcome::RevisionConflict {
            current_revision: *current_revision,
        },
        JournalOutcome::Failed { code } => CommandOutcome::Failed { code: code.clone() },
        JournalOutcome::OutcomeUnknown { attempt_id, .. } => CommandOutcome::OutcomeUnknown {
            attempt_id: attempt_id.clone(),
        },
        // A persisted outcome this version does not know is still a settled
        // one: it is reported as a failure rather than re-executed.
        _ => CommandOutcome::Failed {
            code: "unknown_recorded_outcome".to_owned(),
        },
    }
}

/// Maps an execution failure onto the outcome the turn records.
fn failure_outcome(command_id: CommandId, error: &ExecutionError) -> CommandOutcome {
    match error {
        ExecutionError::RevisionConflict(conflict) => CommandOutcome::RevisionConflict {
            current_revision: conflict.current_revision,
        },
        ExecutionError::Rejected(rejection) => CommandOutcome::Rejected {
            code: rejection.code.clone(),
        },
        ExecutionError::OutcomeUnknown(unknown) => CommandOutcome::OutcomeUnknown {
            attempt_id: unknown.attempt_id.clone(),
        },
        // A timeout is not a failure: the effect may exist, so it becomes an
        // attempt somebody has to settle rather than one to repeat (I15).
        ExecutionError::Timeout => CommandOutcome::OutcomeUnknown {
            attempt_id: derive_attempt_id(&command_id),
        },
        ExecutionError::Store(store) if store.effect_may_have_happened() => {
            CommandOutcome::OutcomeUnknown {
                attempt_id: derive_attempt_id(&command_id),
            }
        }
        ExecutionError::Store(_) => CommandOutcome::Failed {
            code: "store".to_owned(),
        },
        ExecutionError::IdempotencyMismatch { .. } => CommandOutcome::Failed {
            code: "idempotency_mismatch".to_owned(),
        },
        ExecutionError::ScopeViolation => CommandOutcome::Failed {
            code: "scope_violation".to_owned(),
        },
        ExecutionError::Erasure(_) => CommandOutcome::Failed {
            code: "erasure".to_owned(),
        },
        ExecutionError::Other { code } => CommandOutcome::Failed { code: code.clone() },
        _ => CommandOutcome::Failed {
            code: "other".to_owned(),
        },
    }
}

/// The stable code of an outcome, for a journal completion that has no error.
fn outcome_code(outcome: &CommandOutcome) -> String {
    match outcome {
        CommandOutcome::Failed { code } => code.clone(),
        CommandOutcome::Rejected { code } => code.as_str().to_owned(),
        CommandOutcome::RevisionConflict { .. } => "revision_conflict".to_owned(),
        _ => "other".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use turnframe_core::ids::CaseRevision;

    use super::*;

    fn case() -> CaseRef {
        CaseRef::new("trip", "i1", CaseRevision(3))
    }

    #[test]
    fn a_command_type_names_its_variant() {
        assert_eq!(
            command_type(&case(), &serde_json::json!({"set_name": {"value": "x"}})),
            "trip.set_name"
        );
        assert_eq!(
            command_type(&case(), &serde_json::json!("rebook")),
            "trip.rebook"
        );
        assert_eq!(
            command_type(&case(), &serde_json::json!({"a": 1, "b": 2})),
            "trip.command"
        );
    }

    #[test]
    fn a_timeout_is_an_unknown_outcome_and_not_a_failure() {
        let command_id = CommandId::nil();
        let outcome = failure_outcome(command_id, &ExecutionError::Timeout);
        assert!(matches!(outcome, CommandOutcome::OutcomeUnknown { .. }));
        assert_eq!(
            derive_attempt_id(&command_id),
            derive_attempt_id(&command_id),
            "a reconciler must be able to name the same attempt twice"
        );
    }

    #[test]
    fn outbox_identifiers_are_derived_from_the_command() {
        let command_id = CommandId::nil();
        assert_eq!(derive_outbox_id(&command_id), derive_outbox_id(&command_id));
    }
}
