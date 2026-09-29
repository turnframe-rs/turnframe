//! The shared state behind [`MemoryStores`](super::MemoryStores) and every rule
//! it enforces.
//!
//! Everything here is synchronous and takes `&mut self`, for two reasons. It
//! keeps the lock out of the trait implementations, which only lock, call one
//! method and release, so no guard can ever be held across an await. And it
//! lets [`Inner::apply_bundle`] reach the very same rule functions a single
//! write would, so a bundle can never enforce a rule differently from the trait
//! method that enforces it alone.
//!
//! Collections are `BTreeMap`s keyed first by account, so iteration order is a
//! function of the data and never of a hash seed: two runs over the same writes
//! produce the same lists.
//!
//! # All-or-nothing without copying the state
//!
//! A bundle is applied in place, and every mutation it makes first records how
//! to undo itself. On any error the recorded reversals are replayed newest
//! first and the state is exactly what the bundle found; on success the
//! recording is dropped. [`Inner`] therefore does not implement [`Clone`], on
//! purpose: the earlier design copied the whole state per commit, which is
//! quadratic in the number of commits a process makes, and the missing `Clone`
//! is what stops it coming back. [`Inner::apply_bundle`] carries the reasoning
//! and the measurement.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use turnframe_core::case::CaseKey;
use turnframe_core::command::IdempotencyKey;
use turnframe_core::event::{EventRedaction, OutboxEntry, OutboxStatus};
use turnframe_core::ids::{
    AccountId, CaseRevision, CommandId, ConversationId, EventId, InteractionId, OptionId, OutboxId,
    RedactionAuthority, TurnId,
};
use turnframe_core::interaction::{Interaction, InteractionStatus};
use turnframe_core::replay::{ReplayRecord, TurnPhase};
use turnframe_core::response::AssistantTurn;

use crate::commit::{CommitBundle, CommitReceipt};
use crate::conversation::{
    ConversationRecord, RecoveryScope, StoredTurn, StoredUserTurn, TurnPhaseMarker,
};
use crate::error::{StoreError, identity_mismatch, invalid_record};
use crate::events::{EventBatch, EventCursor, EventPage, StoredEvent};
use crate::interaction::{
    InteractionRecord, InvalidationReason, InvalidationRecord, ResolutionOutcome,
};
use crate::journal::{CommandJournalEntry, CommandJournalStatus, JournalAdmission, JournalOutcome};
use crate::memory::fault::FailurePoint;
use crate::outbox::{OutboxClaim, OutboxRecord};

/// Asks whether a failure is armed at `point`, and consumes it if so.
///
/// Passed into [`Inner::apply_bundle`] so the bundle can reach the chaos
/// boundaries of spec §27.7 without the state knowing where the arming lives.
pub(super) type FaultProbe<'a> = &'a mut dyn FnMut(FailurePoint) -> Option<StoreError>;

/// One persisted turn: the user side, the assistant side once composed, and the
/// crash-recovery phase marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TurnEntry {
    pub(super) user: StoredUserTurn,
    pub(super) assistant: Option<AssistantTurn>,
    pub(super) phase: TurnPhase,
    pub(super) updated_at: DateTime<Utc>,
}

impl TurnEntry {
    fn stored(&self) -> StoredTurn {
        StoredTurn {
            user: self.user.clone(),
            assistant: self.assistant.clone(),
            phase: self.phase,
        }
    }

    fn marker(&self, account: &AccountId) -> TurnPhaseMarker {
        TurnPhaseMarker {
            account_id: account.clone(),
            conversation_id: self.user.conversation_id(),
            turn_id: self.user.turn_id(),
            phase: self.phase,
            updated_at: self.updated_at,
        }
    }
}

/// The command journal of one account: entries plus the idempotency index that
/// makes `UNIQUE (account_id, idempotency_key)` real.
#[derive(Debug, Clone, Default)]
struct JournalShard {
    entries: BTreeMap<CommandId, CommandJournalEntry>,
    by_key: BTreeMap<IdempotencyKey, CommandId>,
}

/// One reversal: the exact prior value of one entity a bundle touched.
///
/// Recorded at the moment of mutation and replayed in reverse, so touching the
/// same entity twice inside one bundle still restores the value it had before
/// the bundle started — the earliest record is applied last and wins.
#[derive(Debug, Clone)]
enum Undo {
    /// A journal entry `journal_complete` was about to settle.
    Journal {
        account: AccountId,
        command_id: CommandId,
        before: Box<CommandJournalEntry>,
    },
    /// The tail of the event log one `append_events` was about to add to.
    Events {
        events_len: usize,
        next_sequence: u64,
        account: AccountId,
        /// Whether the account already had an index shard, so a rollback
        /// leaves exactly the shards the bundle found.
        index_shard_existed: bool,
    },
    /// An interaction about to be inserted, settled or invalidated. `before` is
    /// `None` for an insert.
    Interaction {
        account: AccountId,
        id: InteractionId,
        before: Option<Box<InteractionRecord>>,
        shard_existed: bool,
    },
    /// An outbox row about to be enqueued. Enqueue only ever inserts, and only
    /// after proving both keys free, so removing them is its exact inverse.
    Outbox {
        outbox_id: OutboxId,
        unique: (String, IdempotencyKey),
    },
    /// The replay record of a turn, upserted.
    Replay {
        account: AccountId,
        turn_id: TurnId,
        before: Option<Box<ReplayRecord>>,
        shard_existed: bool,
    },
    /// The two columns `set_turn_phase` moves.
    TurnPhase {
        account: AccountId,
        turn_id: TurnId,
        phase: TurnPhase,
        updated_at: DateTime<Utc>,
    },
}

/// Everything the in-memory store knows.
#[derive(Debug, Default)]
pub(super) struct Inner {
    conversations: BTreeMap<AccountId, BTreeMap<ConversationId, ConversationRecord>>,
    turns: BTreeMap<AccountId, BTreeMap<TurnId, TurnEntry>>,
    interactions: BTreeMap<AccountId, BTreeMap<InteractionId, InteractionRecord>>,
    journal: BTreeMap<AccountId, JournalShard>,
    /// Every event ever appended, in append order; the index is the sequence.
    events: Vec<StoredEvent>,
    /// `account -> event id -> position in `events``, for duplicate detection
    /// and account-scoped reads by identifier.
    event_index: BTreeMap<AccountId, BTreeMap<EventId, usize>>,
    outbox: BTreeMap<OutboxId, OutboxRecord>,
    outbox_keys: BTreeMap<(String, IdempotencyKey), OutboxId>,
    replays: BTreeMap<AccountId, BTreeMap<TurnId, ReplayRecord>>,
    next_sequence: u64,
    /// Armed only for the duration of [`Inner::apply_bundle`]. While it is
    /// `Some`, every mutation records how to undo itself; the rest of the time
    /// the recording calls compile down to a null check.
    undo: Option<Vec<Undo>>,
}

// ---------------------------------------------------------------------------
// The undo journal
// ---------------------------------------------------------------------------

impl Inner {
    /// Records `entry`, if a bundle is in flight.
    fn push_undo(&mut self, entry: Undo) {
        if let Some(log) = self.undo.as_mut() {
            log.push(entry);
        }
    }

    /// Remembers a journal entry before it is settled.
    fn snapshot_journal_entry(&mut self, account: &AccountId, command_id: &CommandId) {
        if self.undo.is_none() {
            return;
        }
        let before = self
            .journal
            .get(account)
            .and_then(|shard| shard.entries.get(command_id))
            .cloned();
        if let Some(before) = before {
            self.push_undo(Undo::Journal {
                account: account.clone(),
                command_id: *command_id,
                before: Box::new(before),
            });
        }
    }

    /// Remembers where the event log ended before an append.
    fn snapshot_events(&mut self, account: &AccountId) {
        if self.undo.is_none() {
            return;
        }
        let entry = Undo::Events {
            events_len: self.events.len(),
            next_sequence: self.next_sequence,
            account: account.clone(),
            index_shard_existed: self.event_index.contains_key(account),
        };
        self.push_undo(entry);
    }

    /// Remembers an interaction before it is inserted, settled or invalidated.
    fn snapshot_interaction(&mut self, account: &AccountId, id: &InteractionId) {
        if self.undo.is_none() {
            return;
        }
        let shard_existed = self.interactions.contains_key(account);
        let before = self
            .interactions
            .get(account)
            .and_then(|shard| shard.get(id))
            .cloned()
            .map(Box::new);
        self.push_undo(Undo::Interaction {
            account: account.clone(),
            id: *id,
            before,
            shard_existed,
        });
    }

    /// Remembers a replay record before it is upserted.
    fn snapshot_replay(&mut self, account: &AccountId, turn_id: &TurnId) {
        if self.undo.is_none() {
            return;
        }
        let shard_existed = self.replays.contains_key(account);
        let before = self
            .replays
            .get(account)
            .and_then(|shard| shard.get(turn_id))
            .cloned()
            .map(Box::new);
        self.push_undo(Undo::Replay {
            account: account.clone(),
            turn_id: *turn_id,
            before,
            shard_existed,
        });
    }

    /// Remembers the phase columns of a turn before they move.
    fn snapshot_turn_phase(&mut self, account: &AccountId, turn_id: &TurnId) {
        if self.undo.is_none() {
            return;
        }
        let before = self
            .turns
            .get(account)
            .and_then(|shard| shard.get(turn_id))
            .map(|entry| (entry.phase, entry.updated_at));
        if let Some((phase, updated_at)) = before {
            self.push_undo(Undo::TurnPhase {
                account: account.clone(),
                turn_id: *turn_id,
                phase,
                updated_at,
            });
        }
    }

    /// Restores everything the armed log recorded, latest first, and disarms.
    fn rollback(&mut self) {
        let Some(log) = self.undo.take() else {
            return;
        };
        for entry in log.into_iter().rev() {
            match entry {
                Undo::Journal {
                    account,
                    command_id,
                    before,
                } => {
                    if let Some(shard) = self.journal.get_mut(&account) {
                        shard.entries.insert(command_id, *before);
                    }
                }
                Undo::Events {
                    events_len,
                    next_sequence,
                    account,
                    index_shard_existed,
                } => {
                    let removed = self.events.split_off(events_len.min(self.events.len()));
                    self.next_sequence = next_sequence;
                    if let Some(index) = self.event_index.get_mut(&account) {
                        for event in &removed {
                            index.remove(&event.event_id);
                        }
                        if index.is_empty() && !index_shard_existed {
                            self.event_index.remove(&account);
                        }
                    }
                }
                Undo::Interaction {
                    account,
                    id,
                    before,
                    shard_existed,
                } => match before {
                    Some(record) => {
                        self.interactions
                            .entry(account)
                            .or_default()
                            .insert(id, *record);
                    }
                    None => {
                        if let Some(shard) = self.interactions.get_mut(&account) {
                            shard.remove(&id);
                            if shard.is_empty() && !shard_existed {
                                self.interactions.remove(&account);
                            }
                        }
                    }
                },
                Undo::Outbox { outbox_id, unique } => {
                    self.outbox.remove(&outbox_id);
                    self.outbox_keys.remove(&unique);
                }
                Undo::Replay {
                    account,
                    turn_id,
                    before,
                    shard_existed,
                } => match before {
                    Some(record) => {
                        self.replays
                            .entry(account)
                            .or_default()
                            .insert(turn_id, *record);
                    }
                    None => {
                        if let Some(shard) = self.replays.get_mut(&account) {
                            shard.remove(&turn_id);
                            if shard.is_empty() && !shard_existed {
                                self.replays.remove(&account);
                            }
                        }
                    }
                },
                Undo::TurnPhase {
                    account,
                    turn_id,
                    phase,
                    updated_at,
                } => {
                    if let Some(entry) = self
                        .turns
                        .get_mut(&account)
                        .and_then(|shard| shard.get_mut(&turn_id))
                    {
                        entry.phase = phase;
                        entry.updated_at = updated_at;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Conversations, turns and phase markers
// ---------------------------------------------------------------------------

impl Inner {
    pub(super) fn create_conversation(
        &mut self,
        record: ConversationRecord,
    ) -> Result<(), StoreError> {
        let shard = self
            .conversations
            .entry(record.account_id.clone())
            .or_default();
        if shard.contains_key(&record.id) {
            return Err(StoreError::Conflict);
        }
        shard.insert(record.id, record);
        Ok(())
    }

    pub(super) fn load_conversation(
        &self,
        account: &AccountId,
        id: &ConversationId,
    ) -> Result<ConversationRecord, StoreError> {
        self.conversation(account, id).cloned()
    }

    fn conversation(
        &self,
        account: &AccountId,
        id: &ConversationId,
    ) -> Result<&ConversationRecord, StoreError> {
        self.conversations
            .get(account)
            .and_then(|shard| shard.get(id))
            .ok_or(StoreError::NotFound)
    }

    pub(super) fn append_user_turn(
        &mut self,
        turn: StoredUserTurn,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let account = turn.account_id().clone();
        let conversation = turn.conversation_id();
        self.conversation(&account, &conversation)?;
        let shard = self.turns.entry(account).or_default();
        let turn_id = turn.turn_id();
        if shard.contains_key(&turn_id) {
            return Err(StoreError::Conflict);
        }
        shard.insert(
            turn_id,
            TurnEntry {
                user: turn,
                assistant: None,
                phase: TurnPhase::Received,
                updated_at: now,
            },
        );
        Ok(())
    }

    pub(super) fn append_assistant_turn(
        &mut self,
        account: &AccountId,
        turn: AssistantTurn,
    ) -> Result<(), StoreError> {
        let entry = self
            .turns
            .get_mut(account)
            .and_then(|shard| shard.get_mut(&turn.turn_id))
            .ok_or(StoreError::NotFound)?;
        if entry.user.conversation_id() != turn.conversation_id {
            return Err(identity_mismatch());
        }
        if entry.assistant.is_some() {
            return Err(StoreError::Conflict);
        }
        entry.assistant = Some(turn);
        Ok(())
    }

    pub(super) fn load_recent_turns(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<StoredTurn>, StoreError> {
        self.conversation(account, conversation)?;
        let mut selected: Vec<&TurnEntry> = self
            .turns
            .get(account)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|entry| entry.user.conversation_id() == *conversation)
            .collect();
        selected.sort_by(|a, b| {
            a.user
                .received_at
                .cmp(&b.user.received_at)
                .then_with(|| a.user.turn_id().cmp(&b.user.turn_id()))
        });
        let skip = selected.len().saturating_sub(limit);
        Ok(selected[skip..]
            .iter()
            .map(|entry| entry.stored())
            .collect())
    }

    pub(super) fn load_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<StoredTurn, StoreError> {
        self.turn(account, turn_id).map(TurnEntry::stored)
    }

    fn turn(&self, account: &AccountId, turn_id: &TurnId) -> Result<&TurnEntry, StoreError> {
        self.turns
            .get(account)
            .and_then(|shard| shard.get(turn_id))
            .ok_or(StoreError::NotFound)
    }

    pub(super) fn turn_phase(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<TurnPhaseMarker, StoreError> {
        self.turn(account, turn_id)
            .map(|entry| entry.marker(account))
    }

    pub(super) fn set_turn_phase(
        &mut self,
        account: &AccountId,
        turn_id: &TurnId,
        phase: TurnPhase,
        now: DateTime<Utc>,
    ) -> Result<TurnPhaseMarker, StoreError> {
        self.snapshot_turn_phase(account, turn_id);
        let entry = self
            .turns
            .get_mut(account)
            .and_then(|shard| shard.get_mut(turn_id))
            .ok_or(StoreError::NotFound)?;
        if entry.phase.is_terminal() && entry.phase != phase {
            return Err(StoreError::Conflict);
        }
        entry.phase = phase;
        entry.updated_at = now;
        Ok(entry.marker(account))
    }

    pub(super) fn list_unfinished_turns(
        &self,
        scope: &RecoveryScope,
        limit: usize,
    ) -> Vec<TurnPhaseMarker> {
        let mut found: Vec<(DateTime<Utc>, TurnPhaseMarker)> = self
            .turns
            .iter()
            .filter(|(account, _)| scope.includes(account))
            .flat_map(|(account, shard)| {
                shard
                    .values()
                    .filter(|entry| !entry.phase.is_terminal())
                    .map(move |entry| (entry.user.received_at, entry.marker(account)))
            })
            .collect();
        found.sort_by(|(a_at, a), (b_at, b)| a_at.cmp(b_at).then(a.turn_id.cmp(&b.turn_id)));
        found.truncate(limit);
        found.into_iter().map(|(_, marker)| marker).collect()
    }
}

// ---------------------------------------------------------------------------
// Interactions
// ---------------------------------------------------------------------------

impl Inner {
    /// Shared body of `insert` and `insert_replacing_blocking`.
    ///
    /// With `replace_blocking`, an `Active` occupant of the blocking slot is
    /// invalidated as `Superseded`; without it, an occupied slot is a conflict.
    /// A `Resolving` occupant is never replaced: its commands are executing.
    pub(super) fn insert_interaction(
        &mut self,
        interaction: Interaction,
        replace_blocking: bool,
        now: DateTime<Utc>,
    ) -> Result<Vec<InteractionId>, StoreError> {
        if interaction.status != InteractionStatus::Active {
            return Err(invalid_record());
        }
        let account = interaction.account_id.clone();
        let id = interaction.id;
        if self
            .interactions
            .get(&account)
            .is_some_and(|shard| shard.contains_key(&id))
        {
            return Err(StoreError::Conflict);
        }
        let mut invalidated = Vec::new();
        if interaction.blocking
            && let Some((occupant, status)) =
                self.open_blocking(&account, &interaction.case_ref.key())
        {
            // A `Resolving` occupant is executing its commands and is never
            // swept away underneath them, whatever the caller asked for.
            if !replace_blocking || status != InteractionStatus::Active {
                return Err(StoreError::Conflict);
            }
            self.invalidate_interaction(
                &account,
                &occupant,
                InvalidationReason::Superseded { by: id },
                None,
                now,
            )?;
            invalidated.push(occupant);
        }
        self.snapshot_interaction(&account, &id);
        self.interactions
            .entry(account)
            .or_default()
            .insert(id, InteractionRecord::new(interaction));
        Ok(invalidated)
    }

    /// The card currently holding the blocking slot of `case`, with its status.
    fn open_blocking(
        &self,
        account: &AccountId,
        case: &CaseKey,
    ) -> Option<(InteractionId, InteractionStatus)> {
        self.sorted_interactions(account, |record| {
            record.interaction.blocking
                && record.status().is_open()
                && record.interaction.case_ref.key() == *case
        })
        .first()
        .map(|record| (record.id(), record.status()))
    }

    /// Every interaction of `account` matching `predicate`, ordered by creation
    /// time then identifier.
    fn sorted_interactions(
        &self,
        account: &AccountId,
        predicate: impl Fn(&InteractionRecord) -> bool,
    ) -> Vec<&InteractionRecord> {
        let mut found: Vec<&InteractionRecord> = self
            .interactions
            .get(account)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|record| predicate(record))
            .collect();
        found.sort_by(|a, b| {
            a.interaction
                .created_at
                .cmp(&b.interaction.created_at)
                .then_with(|| a.id().cmp(&b.id()))
        });
        found
    }

    fn interaction_mut(
        &mut self,
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<&mut InteractionRecord, StoreError> {
        self.interactions
            .get_mut(account)
            .and_then(|shard| shard.get_mut(id))
            .ok_or(StoreError::NotFound)
    }

    pub(super) fn get_interaction(
        &self,
        account: &AccountId,
        id: &InteractionId,
    ) -> Result<InteractionRecord, StoreError> {
        self.interactions
            .get(account)
            .and_then(|shard| shard.get(id))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    pub(super) fn list_open_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Vec<Interaction> {
        self.sorted_interactions(account, |record| {
            record.status().is_open() && record.interaction.conversation_id == *conversation
        })
        .into_iter()
        .map(|record| record.interaction.clone())
        .collect()
    }

    pub(super) fn list_open_for_case(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
    ) -> Vec<Interaction> {
        self.sorted_interactions(account, |record| {
            record.status().is_open() && record.interaction.case_ref.key() == *case_key
        })
        .into_iter()
        .map(|record| record.interaction.clone())
        .collect()
    }

    /// Whether the user has already answered a blocking card of this case at
    /// this revision. See [`InteractionReader::blocking_answered_at`].
    ///
    /// [`InteractionReader::blocking_answered_at`]: crate::interaction::InteractionReader::blocking_answered_at
    pub(super) fn blocking_answered_at(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        revision: CaseRevision,
    ) -> bool {
        self.sorted_interactions(account, |record| {
            record.interaction.blocking
                && record.interaction.case_ref.key() == *case_key
                && record.interaction.case_ref.expected_revision == revision
                && matches!(
                    record.status(),
                    InteractionStatus::Resolved
                        | InteractionStatus::Declined
                        | InteractionStatus::Dismissed
                )
        })
        .into_iter()
        .next()
        .is_some()
    }

    pub(super) fn begin_resolution(
        &mut self,
        account: &AccountId,
        id: &InteractionId,
        expected_status: InteractionStatus,
        option_id: OptionId,
        resolved_by: TurnId,
        now: DateTime<Utc>,
    ) -> Result<InteractionRecord, StoreError> {
        let record = self.interaction_mut(account, id)?;
        if record.status() != expected_status
            || !InteractionStatus::can_transition(expected_status, InteractionStatus::Resolving)
        {
            return Err(StoreError::Conflict);
        }
        record
            .interaction
            .begin_resolution(option_id, now)
            .map_err(|_| StoreError::Conflict)?;
        record.resolved_by_turn = Some(resolved_by);
        Ok(record.clone())
    }

    pub(super) fn finish_resolution(
        &mut self,
        account: &AccountId,
        id: &InteractionId,
        outcome: ResolutionOutcome,
    ) -> Result<InteractionRecord, StoreError> {
        self.snapshot_interaction(account, id);
        let record = self.interaction_mut(account, id)?;
        let target = outcome.target_status();
        if record.status() != InteractionStatus::Resolving {
            // Not resolving: the only accepted repeat is the identical outcome
            // already recorded, so recovery may replay a bundle safely.
            return if record.status() == target && settled_with(record, &outcome) {
                Ok(record.clone())
            } else {
                Err(StoreError::Conflict)
            };
        }
        record
            .interaction
            .transition(target)
            .map_err(|_| StoreError::Conflict)?;
        match &outcome {
            ResolutionOutcome::Resolved { event_ids } => {
                // `resolved_at` keeps the moment the answer was accepted, set
                // by `begin_resolution`. When the commands committed is the
                // journal entry's and the events' business, not the card's.
                record.resolution_event_ids = event_ids.clone();
                record.failure_code = None;
            }
            ResolutionOutcome::Failed { code } => {
                record.failure_code = Some(code.clone());
                record.resolution_event_ids.clear();
            }
            ResolutionOutcome::RestoreActive => {
                record.resolution_event_ids.clear();
                record.failure_code = None;
                record.resolved_by_turn = None;
                record.interaction.resolved_option_id = None;
                record.interaction.resolved_at = None;
            }
        }
        Ok(record.clone())
    }

    pub(super) fn invalidate_for_case(
        &mut self,
        account: &AccountId,
        case_key: &CaseKey,
        new_revision: CaseRevision,
        reason: InvalidationReason,
        now: DateTime<Utc>,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let stale: Vec<InteractionId> = self
            .sorted_interactions(account, |record| {
                record.status() == InteractionStatus::Active
                    && record.interaction.case_ref.key() == *case_key
                    && record
                        .interaction
                        .bound_revision()
                        .is_some_and(|bound| bound != new_revision)
            })
            .into_iter()
            .map(InteractionRecord::id)
            .collect();
        for id in &stale {
            self.invalidate_interaction(account, id, reason.clone(), Some(new_revision), now)?;
        }
        Ok(stale)
    }

    pub(super) fn invalidate_case_cards(
        &mut self,
        account: &AccountId,
        case_key: &CaseKey,
        reason: InvalidationReason,
        now: DateTime<Utc>,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let open: Vec<InteractionId> = self
            .sorted_interactions(account, |record| {
                record.status() == InteractionStatus::Active
                    && record.interaction.case_ref.key() == *case_key
            })
            .into_iter()
            .map(InteractionRecord::id)
            .collect();
        for id in &open {
            // No revision is recorded: the case did not move, and writing one
            // would say it had.
            self.invalidate_interaction(account, id, reason.clone(), None, now)?;
        }
        Ok(open)
    }

    fn invalidate_interaction(
        &mut self,
        account: &AccountId,
        id: &InteractionId,
        reason: InvalidationReason,
        new_revision: Option<CaseRevision>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.snapshot_interaction(account, id);
        let record = self.interaction_mut(account, id)?;
        record
            .interaction
            .transition(InteractionStatus::Invalidated)
            .map_err(|_| StoreError::Conflict)?;
        record.invalidation = Some(InvalidationRecord {
            reason,
            new_revision,
            at: now,
        });
        Ok(())
    }

    pub(super) fn expire_due(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<Vec<InteractionId>, StoreError> {
        let due: Vec<(AccountId, InteractionId)> = self
            .interactions
            .iter()
            .flat_map(|(account, shard)| {
                shard
                    .values()
                    .filter(|record| {
                        record.status() == InteractionStatus::Active
                            && record.interaction.is_expired(now)
                    })
                    .map(move |record| (account.clone(), record.id()))
            })
            .collect();
        let mut expired = Vec::with_capacity(due.len());
        for (account, id) in due {
            let record = self.interaction_mut(&account, &id)?;
            record
                .interaction
                .transition(InteractionStatus::Expired)
                .map_err(|_| StoreError::Conflict)?;
            expired.push(id);
        }
        Ok(expired)
    }
}

/// Returns `true` when `record` already carries exactly what `outcome` would
/// write, so repeating the finish is a no-op rather than a conflict.
fn settled_with(record: &InteractionRecord, outcome: &ResolutionOutcome) -> bool {
    match outcome {
        ResolutionOutcome::Resolved { event_ids } => &record.resolution_event_ids == event_ids,
        ResolutionOutcome::Failed { code } => record.failure_code.as_deref() == Some(code.as_str()),
        ResolutionOutcome::RestoreActive => {
            record.resolved_by_turn.is_none() && record.interaction.resolved_option_id.is_none()
        }
    }
}

// ---------------------------------------------------------------------------
// Command journal
// ---------------------------------------------------------------------------

impl Inner {
    pub(super) fn journal_begin(
        &mut self,
        entry: CommandJournalEntry,
    ) -> Result<JournalAdmission, StoreError> {
        let shard = self.journal.entry(entry.account_id.clone()).or_default();
        if let Some(existing) = shard.by_key.get(&entry.idempotency_key) {
            return shard
                .entries
                .get(existing)
                .cloned()
                .map(JournalAdmission::replay)
                .ok_or(StoreError::Corrupt);
        }
        if shard.entries.contains_key(&entry.command_id) {
            return Err(StoreError::Conflict);
        }
        shard
            .by_key
            .insert(entry.idempotency_key.clone(), entry.command_id);
        shard.entries.insert(entry.command_id, entry);
        Ok(JournalAdmission::Fresh)
    }

    fn journal_entry_mut(
        &mut self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<&mut CommandJournalEntry, StoreError> {
        self.journal
            .get_mut(account)
            .and_then(|shard| shard.entries.get_mut(command_id))
            .ok_or(StoreError::NotFound)
    }

    pub(super) fn journal_mark_executing(
        &mut self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<(), StoreError> {
        let entry = self.journal_entry_mut(account, command_id)?;
        match entry.status {
            CommandJournalStatus::Executing => Ok(()),
            CommandJournalStatus::Pending | CommandJournalStatus::AwaitingConfirmation => {
                entry.status = CommandJournalStatus::Executing;
                Ok(())
            }
            _ => Err(StoreError::Conflict),
        }
    }

    pub(super) fn journal_complete(
        &mut self,
        account: &AccountId,
        command_id: &CommandId,
        outcome: JournalOutcome,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.snapshot_journal_entry(account, command_id);
        let entry = self.journal_entry_mut(account, command_id)?;
        let target = outcome.status();
        if entry.status == target {
            return if entry.result.as_ref() == Some(&outcome) {
                Ok(())
            } else {
                Err(StoreError::Conflict)
            };
        }
        if !CommandJournalStatus::can_transition(entry.status, target) {
            return Err(StoreError::Conflict);
        }
        entry.status = target;
        entry.result = Some(outcome);
        entry.completed_at = Some(now);
        Ok(())
    }

    pub(super) fn journal_get(
        &self,
        account: &AccountId,
        command_id: &CommandId,
    ) -> Result<CommandJournalEntry, StoreError> {
        self.journal
            .get(account)
            .and_then(|shard| shard.entries.get(command_id))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    pub(super) fn journal_for_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
        only_pending: bool,
    ) -> Vec<CommandJournalEntry> {
        let mut found: Vec<CommandJournalEntry> = self
            .journal
            .get(account)
            .into_iter()
            .flat_map(|shard| shard.entries.values())
            .filter(|entry| {
                entry.turn_id == *turn_id && (!only_pending || entry.status.is_pending())
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.command_id.cmp(&b.command_id))
        });
        found
    }
}

// ---------------------------------------------------------------------------
// Event journal
// ---------------------------------------------------------------------------

impl Inner {
    pub(super) fn append_events(&mut self, batch: EventBatch) -> Result<Vec<EventId>, StoreError> {
        if batch.is_empty() {
            return Err(invalid_record());
        }
        self.snapshot_events(&batch.account_id);
        let known = self
            .event_index
            .entry(batch.account_id.clone())
            .or_default();
        let mut seen = Vec::with_capacity(batch.events.len());
        for event in &batch.events {
            if known.contains_key(&event.event_id) || seen.contains(&event.event_id) {
                return Err(StoreError::Conflict);
            }
            seen.push(event.event_id);
        }
        let mut appended = Vec::with_capacity(batch.events.len());
        for event in batch.events {
            self.next_sequence = self.next_sequence.saturating_add(1);
            let position = self.events.len();
            self.events.push(StoredEvent {
                sequence: self.next_sequence,
                event_id: event.event_id,
                account_id: batch.account_id.clone(),
                case_key: batch.case_key.clone(),
                case_revision: batch.revision,
                command_id: batch.command_id,
                event_type: event.event_type,
                payload: event.payload,
                occurred_at: event.occurred_at,
                redaction: None,
            });
            appended.push(event.event_id);
            self.event_index
                .entry(batch.account_id.clone())
                .or_default()
                .insert(event.event_id, position);
        }
        Ok(appended)
    }

    pub(super) fn list_events_since(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        since: CaseRevision,
        limit: usize,
    ) -> Vec<StoredEvent> {
        self.events
            .iter()
            .filter(|event| {
                event.account_id == *account
                    && event.case_key == *case_key
                    && event.case_revision > since
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Backs [`EventJournalReader::read_from`](crate::events::EventJournalReader::read_from).
    ///
    /// `events` is append-only and its sequences increase with position, so the
    /// first candidate is found by binary search and the scan from there only
    /// ever walks events the cursor has not seen. Events of other accounts are
    /// skipped without consuming the limit.
    pub(super) fn read_events_from(
        &self,
        account: &AccountId,
        after: EventCursor,
        limit: usize,
    ) -> EventPage {
        let start = self
            .events
            .partition_point(|event| event.sequence <= after.value());
        let page = self.events[start..]
            .iter()
            .filter(|event| event.account_id == *account)
            .take(limit)
            .cloned()
            .collect();
        EventPage::new(page, after)
    }

    pub(super) fn events_by_ids(&self, account: &AccountId, ids: &[EventId]) -> Vec<StoredEvent> {
        let Some(index) = self.event_index.get(account) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| index.get(id))
            .filter_map(|position| self.events.get(*position))
            .cloned()
            .collect()
    }

    /// Backs
    /// [`EventJournalWriter::redact_payload`](crate::events::EventJournalWriter::redact_payload).
    ///
    /// The event is found through the same index the reads use and mutated in
    /// place, so its position in `events` — and therefore its sequence and its
    /// place in every page — is untouched by construction: there is no code
    /// path here that could remove or move an element. Only the payload and the
    /// erasure record change.
    pub(super) fn redact_event_payload(
        &mut self,
        account: &AccountId,
        event_id: &EventId,
        authority: &RedactionAuthority,
        now: DateTime<Utc>,
    ) -> Result<EventRedaction, StoreError> {
        let position = self
            .event_index
            .get(account)
            .and_then(|index| index.get(event_id))
            .copied()
            .ok_or(StoreError::NotFound)?;
        let event = self.events.get_mut(position).ok_or(StoreError::Corrupt)?;
        // Retrying an erasure request must not rewrite who erased what.
        if let Some(existing) = &event.redaction {
            return Ok(existing.clone());
        }
        let redaction = EventRedaction {
            redacted_at: now,
            authority: authority.clone(),
        };
        event.payload = serde_json::Value::Null;
        event.redaction = Some(redaction.clone());
        Ok(redaction)
    }

    pub(super) fn count_events(&self, account: &AccountId, case_key: &CaseKey) -> u64 {
        let total = self
            .events
            .iter()
            .filter(|event| event.account_id == *account && event.case_key == *case_key)
            .count();
        u64::try_from(total).unwrap_or(u64::MAX)
    }
}

// ---------------------------------------------------------------------------
// Outbox
// ---------------------------------------------------------------------------

impl Inner {
    pub(super) fn enqueue_outbox(&mut self, entry: OutboxEntry) -> Result<(), StoreError> {
        if entry.status != OutboxStatus::Pending {
            return Err(invalid_record());
        }
        if self.outbox.contains_key(&entry.outbox_id) {
            return Err(StoreError::Conflict);
        }
        let unique = (entry.destination.clone(), entry.idempotency_key.clone());
        if self.outbox_keys.contains_key(&unique) {
            return Err(StoreError::Conflict);
        }
        self.push_undo(Undo::Outbox {
            outbox_id: entry.outbox_id,
            unique: unique.clone(),
        });
        self.outbox_keys.insert(unique, entry.outbox_id);
        self.outbox
            .insert(entry.outbox_id, OutboxRecord::new(entry));
        Ok(())
    }

    pub(super) fn get_outbox(&self, outbox_id: &OutboxId) -> Result<OutboxRecord, StoreError> {
        self.outbox
            .get(outbox_id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    pub(super) fn outbox_for_command(&self, command_id: &CommandId) -> Vec<OutboxRecord> {
        let mut found: Vec<OutboxRecord> = self
            .outbox
            .values()
            .filter(|record| record.entry.command_id == *command_id)
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            a.entry
                .created_at
                .cmp(&b.entry.created_at)
                .then_with(|| a.entry.outbox_id.cmp(&b.entry.outbox_id))
        });
        found
    }

    pub(super) fn claim_due(
        &mut self,
        now: DateTime<Utc>,
        limit: usize,
        worker_id: &str,
    ) -> Vec<OutboxEntry> {
        let mut due: Vec<OutboxId> = self
            .outbox
            .values()
            .filter(|record| {
                record.entry.status == OutboxStatus::Pending
                    && record.entry.next_attempt_at.is_none_or(|at| at <= now)
            })
            .map(|record| record.entry.outbox_id)
            .collect();
        due.sort_by_key(|id| {
            self.outbox
                .get(id)
                .map(|record| (record.entry.created_at, record.entry.outbox_id))
        });
        due.truncate(limit);
        let mut claimed = Vec::with_capacity(due.len());
        for id in due {
            if let Some(record) = self.outbox.get_mut(&id) {
                record.entry.status = OutboxStatus::Dispatching;
                record.entry.attempt_count = record.entry.attempt_count.saturating_add(1);
                record.claim = Some(OutboxClaim {
                    worker_id: worker_id.to_owned(),
                    claimed_at: now,
                });
                claimed.push(record.entry.clone());
            }
        }
        claimed
    }

    pub(super) fn mark_outbox_completed(
        &mut self,
        outbox_id: &OutboxId,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let record = self.outbox.get_mut(outbox_id).ok_or(StoreError::NotFound)?;
        if record.entry.status == OutboxStatus::Completed {
            return Ok(());
        }
        if !OutboxStatus::can_transition(record.entry.status, OutboxStatus::Completed) {
            return Err(StoreError::Conflict);
        }
        record.entry.status = OutboxStatus::Completed;
        record.entry.completed_at = Some(now);
        record.claim = None;
        Ok(())
    }

    pub(super) fn mark_outbox_failed(
        &mut self,
        outbox_id: &OutboxId,
        reason: String,
        retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let record = self.outbox.get_mut(outbox_id).ok_or(StoreError::NotFound)?;
        // Every rejection is decided BEFORE anything is written: a `Conflict`
        // means nothing changed, including the failure reason. Recording the
        // reason first and refusing afterwards would leave a terminal row
        // carrying the text of an attempt the store also says never happened.
        match retry_at {
            Some(at) => {
                if record.entry.status.is_terminal() {
                    return Err(StoreError::Conflict);
                }
                record.last_failure = Some(reason);
                record.entry.status = OutboxStatus::Pending;
                record.entry.next_attempt_at = Some(at);
                record.claim = None;
                Ok(())
            }
            None => {
                if record.entry.status == OutboxStatus::Failed {
                    // Already failed: the repeat is idempotent, and the reason
                    // that settled the row is the one that stays.
                    return Ok(());
                }
                if !OutboxStatus::can_transition(record.entry.status, OutboxStatus::Failed) {
                    return Err(StoreError::Conflict);
                }
                record.last_failure = Some(reason);
                record.entry.status = OutboxStatus::Failed;
                record.entry.completed_at = Some(now);
                record.claim = None;
                Ok(())
            }
        }
    }

    pub(super) fn mark_outbox_outcome_unknown(
        &mut self,
        outbox_id: &OutboxId,
        remote_ref: Option<String>,
    ) -> Result<(), StoreError> {
        let record = self.outbox.get_mut(outbox_id).ok_or(StoreError::NotFound)?;
        if record.entry.status == OutboxStatus::OutcomeUnknown {
            if record.remote_ref.is_none() {
                record.remote_ref = remote_ref;
            }
            return Ok(());
        }
        if !OutboxStatus::can_transition(record.entry.status, OutboxStatus::OutcomeUnknown) {
            return Err(StoreError::Conflict);
        }
        record.entry.status = OutboxStatus::OutcomeUnknown;
        record.remote_ref = remote_ref;
        record.claim = None;
        Ok(())
    }

    pub(super) fn reschedule_outbox(
        &mut self,
        outbox_id: &OutboxId,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let record = self.outbox.get_mut(outbox_id).ok_or(StoreError::NotFound)?;
        if record.entry.status.is_terminal() {
            return Err(StoreError::Conflict);
        }
        record.entry.status = OutboxStatus::Pending;
        record.entry.next_attempt_at = Some(next_attempt_at);
        record.claim = None;
        Ok(())
    }

    pub(super) fn release_expired_claims(
        &mut self,
        claimed_before: DateTime<Utc>,
    ) -> Vec<OutboxId> {
        let mut released = Vec::new();
        for record in self.outbox.values_mut() {
            let stale = record.entry.status == OutboxStatus::Dispatching
                && record
                    .claim
                    .as_ref()
                    .is_some_and(|claim| claim.claimed_at < claimed_before);
            if stale {
                record.entry.status = OutboxStatus::Pending;
                record.entry.next_attempt_at = None;
                record.claim = None;
                released.push(record.entry.outbox_id);
            }
        }
        released.sort_unstable();
        released
    }
}

// ---------------------------------------------------------------------------
// Replay records
// ---------------------------------------------------------------------------

impl Inner {
    pub(super) fn put_replay(&mut self, record: ReplayRecord) {
        self.snapshot_replay(&record.account_id, &record.turn_id);
        self.replays
            .entry(record.account_id.clone())
            .or_default()
            .insert(record.turn_id, record);
    }

    pub(super) fn get_replay(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<ReplayRecord, StoreError> {
        self.replays
            .get(account)
            .and_then(|shard| shard.get(turn_id))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    pub(super) fn replays_for_conversation(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Vec<ReplayRecord> {
        let mut found: Vec<ReplayRecord> = self
            .replays
            .get(account)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|record| record.conversation_id == *conversation)
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            a.recorded_at
                .cmp(&b.recorded_at)
                .then_with(|| a.turn_id.cmp(&b.turn_id))
        });
        let skip = found.len().saturating_sub(limit);
        found.split_off(skip)
    }
}

// ---------------------------------------------------------------------------
// The commit bundle
// ---------------------------------------------------------------------------

impl Inner {
    /// Applies every item of `bundle` in the normative order of
    /// [`crate::commit`], all or nothing.
    ///
    /// # How the "nothing" is achieved
    ///
    /// The items are applied to the live state, and every mutation they make
    /// first records how to undo itself in [`Inner::undo`]. Any error — an
    /// illegal item, or a failure armed at one of the three chaos points a
    /// bundle passes through — replays those records in reverse and leaves the
    /// state exactly as the bundle found it.
    ///
    /// This used to be done by applying the bundle to a clone of the whole
    /// state and swapping the clone in on success, which is easier to believe
    /// but costs a deep copy of every conversation, card, journal entry and
    /// committed event *per commit*. That is quadratic in the number of commits
    /// a process makes: measured on this store, 4 000 single-event commits took
    /// 5.4 s in release mode, with the per-commit cost growing linearly with the
    /// history — 170 µs at the 500th commit, 1.35 ms at the 4 000th. Since this
    /// store backs the test kit, the examples and the evaluation harness, that
    /// cost shows up as slow suites, so the state is mutated in place and the
    /// reversal is recorded instead. The undo journal costs one clone of each
    /// entity a bundle actually touches, which is what the bundle was going to
    /// write anyway: the same 4 000 commits now take 6 ms, flat at about 1.5 µs
    /// each however long the history gets.
    ///
    /// The two are equivalent for every outcome a caller can observe, and the
    /// one case where they differ is not observable: a *panic* mid-bundle would
    /// leave the state half-applied instead of untouched — but a panic here
    /// happens while the store's mutex guard is held, which poisons it, and
    /// every later call answers [`StoreError::Corrupt`] under either design.
    pub(super) fn apply_bundle(
        &mut self,
        account: &AccountId,
        bundle: CommitBundle,
        now: DateTime<Utc>,
        fault: FaultProbe<'_>,
    ) -> Result<CommitReceipt, StoreError> {
        bundle.validate(account)?;
        self.undo = Some(Vec::new());
        let outcome = self.apply_bundle_items(account, bundle, now, fault);
        match outcome {
            Ok(receipt) => {
                self.undo = None;
                Ok(receipt)
            }
            Err(error) => {
                self.rollback();
                Err(error)
            }
        }
    }

    /// The items themselves. Never called with the undo journal disarmed.
    fn apply_bundle_items(
        &mut self,
        account: &AccountId,
        bundle: CommitBundle,
        now: DateTime<Utc>,
        fault: FaultProbe<'_>,
    ) -> Result<CommitReceipt, StoreError> {
        let mut event_ids = Vec::new();
        let mut inserted_interactions = Vec::new();
        let mut invalidated_interactions = Vec::new();

        for completion in bundle.journal_completions {
            self.journal_complete(account, &completion.command_id, completion.outcome, now)?;
        }
        if let Some(error) = fault(FailurePoint::AfterJournalInsertBeforeCommit) {
            return Err(error);
        }

        for batch in bundle.events {
            event_ids.extend(self.append_events(batch)?);
        }
        if let Some(error) = fault(FailurePoint::AfterCommitBeforeEventReadback) {
            return Err(error);
        }

        for finish in bundle.interaction_finishes {
            self.finish_resolution(account, &finish.interaction_id, finish.outcome)?;
        }

        for invalidation in bundle.interaction_invalidations {
            invalidated_interactions.extend(self.invalidate_for_case(
                account,
                &invalidation.case_key,
                invalidation.new_revision,
                invalidation.reason,
                now,
            )?);
        }

        for insert in bundle.interaction_inserts {
            let id = insert.interaction.id;
            invalidated_interactions.extend(self.insert_interaction(
                insert.interaction,
                insert.replace_blocking,
                now,
            )?);
            inserted_interactions.push(id);
        }
        if let Some(error) = fault(FailurePoint::AfterInteractionPersistence) {
            return Err(error);
        }

        for entry in bundle.outbox_entries {
            self.enqueue_outbox(entry)?;
        }

        if let Some(record) = bundle.replay_record {
            self.put_replay(record);
        }

        if let Some(update) = bundle.turn_phase {
            self.set_turn_phase(account, &update.turn_id, update.phase, now)?;
        }

        Ok(CommitReceipt {
            event_ids,
            inserted_interactions,
            invalidated_interactions,
            committed_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use turnframe_core::case::CaseRef;
    use turnframe_core::ids::UserId;
    use turnframe_core::interaction::{
        InteractionKind, InteractionOption, InteractionPayload, InteractionSpec,
        StoredInteractionAction,
    };
    use turnframe_core::locale::Locale;
    use turnframe_core::turn::{ActorContext, TurnInput};

    use super::*;

    fn card_spec() -> InteractionSpec {
        InteractionSpec::new(
            "undo",
            CaseRef::new("undo", "case-1", CaseRevision(1)),
            InteractionKind::SingleSelect,
            InteractionPayload::new("undo card").with_option(InteractionOption::new(
                "ack",
                "Got it",
                StoredInteractionAction::Dismiss,
            )),
        )
    }

    fn account() -> AccountId {
        AccountId::from("undo")
    }

    fn epoch() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH
    }

    /// A state holding one conversation and one turn, ready for a phase move.
    fn state_with_turn(turn: TurnId) -> (Inner, ConversationId) {
        let mut inner = Inner::default();
        let conversation = ConversationId::new();
        inner
            .create_conversation(ConversationRecord::new(conversation, account(), epoch()))
            .expect("the conversation is created");
        inner
            .append_user_turn(
                StoredUserTurn::new(
                    TurnInput {
                        turn_id: turn,
                        conversation_id: conversation,
                        actor: ActorContext::new(account(), UserId::from("u")),
                        text: Some("hello".to_owned()),
                        interaction_response: None,
                        attachments: Vec::new(),
                        origin: None,
                        locale: Locale::from("en"),
                        effort: None,
                    },
                    epoch(),
                ),
                epoch(),
            )
            .expect("the turn is appended");
        (inner, conversation)
    }

    /// The phase marker is the last item of a bundle, so no *bundle* can fail
    /// after moving it — but the reversal exists so that adding an item after
    /// it stays safe, and an untested reversal is a reversal that does not
    /// work. This drives the journal directly to prove it does.
    #[test]
    fn the_phase_marker_reversal_restores_both_columns_it_moves() {
        let turn = TurnId::new();
        let (mut inner, _) = state_with_turn(turn);
        let later = epoch() + chrono::TimeDelta::seconds(30);

        inner.undo = Some(Vec::new());
        inner
            .set_turn_phase(&account(), &turn, TurnPhase::Committed, later)
            .expect("the phase moves");
        let moved = inner.turn_phase(&account(), &turn).expect("a marker");
        assert_eq!(moved.phase, TurnPhase::Committed);
        assert_eq!(moved.updated_at, later);

        inner.rollback();
        let restored = inner.turn_phase(&account(), &turn).expect("a marker");
        assert_eq!(restored.phase, TurnPhase::Received);
        assert_eq!(
            restored.updated_at,
            epoch(),
            "the timestamp moves back with the phase"
        );
        assert!(inner.undo.is_none(), "a rollback disarms the journal");
    }

    /// Reversals are replayed newest first, so the *oldest* record of an entity
    /// is the one that lands: a bundle that writes the same record twice still
    /// rolls back to what it found.
    #[test]
    fn touching_one_record_twice_restores_what_the_bundle_found() {
        let mut inner = Inner::default();
        let turn = TurnId::new();
        let conversation = ConversationId::new();
        let original = ReplayRecord::received(turn, conversation, account(), epoch());
        inner.put_replay(original.clone());

        inner.undo = Some(Vec::new());
        let mut second = original.clone();
        second.phase = TurnPhase::Interpreted;
        inner.put_replay(second);
        let mut third = original.clone();
        third.phase = TurnPhase::Committed;
        inner.put_replay(third);
        assert_eq!(
            inner.get_replay(&account(), &turn).expect("a record").phase,
            TurnPhase::Committed
        );

        inner.rollback();
        assert_eq!(
            inner.get_replay(&account(), &turn).expect("a record"),
            original,
            "two writes to one record still roll back to the value before both"
        );
    }

    /// A reversal must not leave a shard the bundle created behind. Nothing can
    /// read an empty shard, but a state that differs from the one the bundle
    /// found is a state whose next failure is harder to reason about.
    #[test]
    fn a_reversal_leaves_no_shard_the_bundle_created() {
        let mut inner = Inner::default();
        let turn = TurnId::new();
        let conversation = ConversationId::new();
        let interaction = Interaction::from_spec(
            card_spec(),
            InteractionId::new(),
            account(),
            conversation,
            turn,
            epoch(),
        )
        .expect("a valid fixture card");

        inner.undo = Some(Vec::new());
        inner
            .insert_interaction(interaction.clone(), false, epoch())
            .expect("the card is inserted");
        inner
            .append_events(EventBatch::new(
                account(),
                interaction.case_ref.key(),
                CommandId::new(),
                CaseRevision(1),
                vec![turnframe_core::event::CommittedEvent {
                    event_id: EventId::new(),
                    event_type: "undo.happened".to_owned(),
                    occurred_at: epoch(),
                    payload: serde_json::Value::Null,
                }],
            ))
            .expect("the events are appended");
        inner.put_replay(ReplayRecord::received(
            turn,
            conversation,
            account(),
            epoch(),
        ));

        inner.rollback();

        assert!(
            !inner.interactions.contains_key(&account()),
            "the interaction shard the bundle created must be gone"
        );
        assert!(
            !inner.event_index.contains_key(&account()),
            "the event index shard the bundle created must be gone"
        );
        assert!(
            !inner.replays.contains_key(&account()),
            "the replay shard the bundle created must be gone"
        );
        assert!(
            inner.events.is_empty(),
            "the event log must be back to empty"
        );
        assert_eq!(
            inner.next_sequence, 0,
            "a rolled-back append must not consume a sequence"
        );
    }
}
