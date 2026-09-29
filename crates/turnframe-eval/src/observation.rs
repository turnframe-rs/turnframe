//! What one execution of an item actually did.
//!
//! An [`Observation`] is read back from the same places an operator would look
//! after an incident — the replay record, the command journal, the event
//! ledger, the interaction store, the phase marker and the persisted turn — and
//! nowhere else. Nothing here is inferred from the model's words, which is why
//! a judge can never be asked whether an effect happened: by the time a judge
//! sees anything, the effects have already been read from storage.
//!
//! The observation is also what makes sampling meaningful. Its
//! [`signature`](Observation::signature) is a canonical rendering of the
//! deterministic facts of one run, so two samples that behaved identically
//! collapse to one string and two that did not, do not.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use turnframe_core::case::CaseKey;
use turnframe_core::flow::WorkflowRegistry;
use turnframe_core::ids::{AccountId, CaseId, InteractionId, TurnId};
use turnframe_core::interaction::{InteractionKind, InteractionStatus};
use turnframe_core::replay::{DiscardedAnswer, ProviderAttemptOutcome, ReplayRecord, TurnPhase};
use turnframe_core::response::{AssistantTurn, ResponseBlock};
use turnframe_core::target::TargetResolution;
use turnframe_core::understanding::UnderstoodAct;
use turnframe_runtime::resume::CARD_UNIT;
use turnframe_store::conversation::ConversationReader;
use turnframe_store::events::{EventCursor, EventJournalReader};
use turnframe_store::interaction::InteractionReader;
use turnframe_store::journal::CommandJournalReader;
use turnframe_store::replay::ReplayReader;
use turnframe_store::stores::Stores;

use crate::corpus::{BlockKind, CaseSeed};

/// How many events one page of the journal carries while the ledger is being
/// paged.
///
/// This is a page size, not a bound: [`Observation::collect`] keeps asking
/// until the journal says it has caught up, so the only thing this number
/// changes is how many round trips that takes.
const EVENT_PAGE_SIZE: usize = 512;

/// A case's state on both sides of the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedState {
    /// What the item seeded.
    pub before: serde_json::Value,
    /// What the store holds now, or `None` for a case that is no longer there.
    pub after: Option<serde_json::Value>,
}

/// One act the message was understood to ask for, as the replay record holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedAct {
    /// Snake-case kind, `apply_operation` or `start_workflow`.
    pub kind: String,
    /// The operation, when the variant names one.
    pub operation: Option<String>,
    /// What the reduction decided about it — `rejected`, `no_change`,
    /// `awaiting_confirmation`, and the rest of
    /// [`PlannedActResult::name`](turnframe_core::reduce::PlannedActResult::name).
    /// `None` for a turn whose understanding never reached the reduction.
    pub outcome: Option<String>,
}

impl ObservedAct {
    fn of(act: &UnderstoodAct, outcome: Option<&String>) -> Self {
        Self {
            kind: act.kind_name().to_owned(),
            operation: act.operation().map(|key| key.as_str().to_owned()),
            outcome: outcome.cloned(),
        }
    }
}

/// How one act's target resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedResolution {
    /// Position of the act among the message's understood acts.
    pub act_index: usize,
    /// Snake-case resolution name, e.g. `exact`.
    pub resolution: String,
    /// The case, for the resolutions that name one.
    pub case_id: Option<CaseId>,
}

impl ObservedResolution {
    fn of(act_index: usize, resolution: &TargetResolution) -> Self {
        let (name, case_id) = match resolution {
            TargetResolution::Exact { case_ref } => ("exact", Some(case_ref.case_id.clone())),
            TargetResolution::Ambiguous { .. } => ("ambiguous", None),
            TargetResolution::Missing => ("missing", None),
            TargetResolution::Unauthorized => ("unauthorized", None),
            TargetResolution::Stale { case_ref, .. } => ("stale", Some(case_ref.case_id.clone())),
            _ => ("other", None),
        };
        Self {
            act_index,
            resolution: name.to_owned(),
            case_id,
        }
    }
}

/// One persisted card, as the interaction store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedInteraction {
    /// Identifier.
    pub id: InteractionId,
    /// The case it belongs to.
    pub case: CaseKey,
    /// Its shape.
    pub kind: InteractionKind,
    /// Its lifecycle status.
    pub status: InteractionStatus,
    /// Whether it owns unqualified answers for the case.
    pub blocking: bool,
    /// The turn that created it.
    pub created_by_turn: TurnId,
}

/// Everything one execution of an item left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The turn that was run.
    pub turn_id: TurnId,
    /// The orchestrator's error code, when the turn failed outright.
    pub error_code: Option<String>,
    /// The acts the message was understood to ask for, in order. Empty for a turn
    /// with no text, whose acts come from the card it answers.
    pub acts: Vec<ObservedAct>,
    /// How each of those acts' targets resolved.
    pub target_resolutions: Vec<ObservedResolution>,
    /// The whole understanding of the message, for scoring each task that made it.
    pub understanding: Option<turnframe_core::understanding::Understanding>,
    /// Command types journaled this turn, in admission order.
    pub commands: Vec<String>,
    /// Event types committed this turn, in append order.
    pub events: Vec<String>,
    /// Whether [`events`](Self::events) was cut short by a configured bound.
    ///
    /// It is `false` for every observation collected with
    /// [`collect`](Self::collect), which pages the journal to the end. It can
    /// only be `true` when a run set
    /// [`ExecutionConfig::max_observed_events`](crate::config::ExecutionConfig::max_observed_events),
    /// and when it is, every assertion that reads the event list fails: an
    /// expectation checked against half a ledger is not a measurement, and a
    /// forbidden event hiding in the half nobody read would otherwise report a
    /// green safety row.
    pub events_truncated: bool,
    /// The revision each seeded case ended the turn at.
    pub revisions: Vec<(CaseKey, u64)>,
    /// What each seeded case held before the turn and after it.
    ///
    /// Both sides, because the two questions a corpus asks about state are
    /// different and one of them has no answer without the before: «this field
    /// ends at Lisbon» is about the after, and «this field did not move» is
    /// about the pair.
    pub states: Vec<(CaseKey, ObservedState)>,
    /// Cards known for the seeded cases and cards this turn created.
    pub interactions: Vec<ObservedInteraction>,
    /// Response block kinds, in order.
    pub blocks: Vec<BlockKind>,
    /// Phase the turn finished in.
    pub phase: Option<TurnPhase>,
    /// How many provider attempts failed or fell back.
    pub provider_failures: usize,
    /// Every answer a model produced this turn that the runtime threw away
    /// whole, in the order it discarded them.
    ///
    /// # Why a measurement wants this
    ///
    /// It is the only channel for a turn that did nothing *and had nothing to
    /// show for it*. Every other field here reports an effect, and the failure
    /// this answers — the assistant closing a turn without proposing anything
    /// or asking anything — leaves no effect by definition, so an empty
    /// `commands` and an empty `events` look exactly like a turn that correctly
    /// had nothing to do. This list separates the two: a turn whose plan was
    /// refused for citing words the user never wrote is a defect, and a turn
    /// that quietly agreed there was nothing to do is not.
    ///
    /// Deliberately **not** part of [`signature`](Self::signature) — see there.
    pub discarded_answers: Vec<DiscardedAnswer>,
    /// How many cards this turn created.
    pub cards_created: usize,
    /// The model-authored text of the turn, which is all a judge ever sees.
    pub answer: String,
}

impl Observation {
    /// Adds the cases every turn of the conversation wrote, read back now, beside the
    /// seeded ones. A case the conversation created was seeded as nothing.
    pub async fn with_conversation_cases(
        mut self,
        stores: &Stores,
        workflows: &WorkflowRegistry,
        account: &AccountId,
        earlier: &[TurnId],
    ) -> Self {
        let mut keys: Vec<CaseKey> = Vec::new();
        for turn in earlier.iter().copied().chain(std::iter::once(self.turn_id)) {
            let entries = CommandJournalReader::for_turn(stores.journal().as_ref(), account, &turn)
                .await
                .unwrap_or_default();
            for entry in entries {
                let key = entry.case_ref.key();
                let seen =
                    keys.contains(&key) || self.states.iter().any(|(known, _)| *known == key);
                if !seen {
                    keys.push(key);
                }
            }
        }
        for key in keys {
            let Some(registered) = workflows.get(&key.workflow) else {
                continue;
            };
            if let Ok(loaded) = registered.executor.load(account, &key.case_id).await {
                self.states.push((
                    key,
                    ObservedState {
                        before: serde_json::Value::Null,
                        after: loaded.value,
                    },
                ));
            }
        }
        self
    }

    /// Reads back everything one turn left in the stores, paging the event
    /// journal to the end.
    ///
    /// `outcome` is the turn as the orchestrator returned it, or the stable
    /// error code of the failure. A failed turn is still observed: what it
    /// journaled and committed before it failed is exactly what a safety
    /// assertion is about.
    ///
    /// The ledger is read through the journal's **sequence cursor**, one page
    /// after another, until the journal reports it has caught up. A case with
    /// nine thousand prior events is therefore observed exactly like a case
    /// with nine, which is the property a seeded corpus depends on: an item
    /// whose fixture carries a long history must still see the events its own
    /// turn committed, and must still see a forbidden one.
    pub async fn collect(
        stores: &Stores,
        workflows: &WorkflowRegistry,
        account: &AccountId,
        turn_id: TurnId,
        cases: &[CaseSeed],
        outcome: Result<&AssistantTurn, String>,
    ) -> Self {
        Self::collect_bounded(stores, workflows, account, turn_id, cases, outcome, None).await
    }

    /// [`collect`](Self::collect), with an optional cap on how many of the
    /// turn's events are recorded.
    ///
    /// `max_events` is `None` for the complete ledger, which is what every
    /// reproducible run wants. A `Some(limit)` is a deliberate ceiling for a
    /// corpus run against a real endpoint where one item could commit an
    /// unbounded number of events, and it is **never silent**: reaching it sets
    /// [`events_truncated`](Self::events_truncated), and
    /// [`check`](crate::assertions::check) turns that into a failure of every
    /// expectation that reads the event list. A bound that made an assertion
    /// pass would be worse than no assertion at all.
    #[allow(clippy::too_many_arguments)]
    pub async fn collect_bounded(
        stores: &Stores,
        workflows: &WorkflowRegistry,
        account: &AccountId,
        turn_id: TurnId,
        cases: &[CaseSeed],
        outcome: Result<&AssistantTurn, String>,
        max_events: Option<usize>,
    ) -> Self {
        let (turn, error_code) = match outcome {
            Ok(turn) => (Some(turn), None),
            Err(code) => (None, Some(code)),
        };
        let record = ReplayReader::get(stores.replay().as_ref(), account, &turn_id)
            .await
            .ok();
        let entries = CommandJournalReader::for_turn(stores.journal().as_ref(), account, &turn_id)
            .await
            .unwrap_or_default();

        let mut case_keys: Vec<CaseKey> = cases
            .iter()
            .map(|seed| CaseKey::new(seed.workflow.clone(), seed.case_id.clone()))
            .collect();
        for entry in &entries {
            let key = entry.case_ref.key();
            if !case_keys.contains(&key) {
                case_keys.push(key);
            }
        }
        let command_ids: BTreeSet<_> = entries.iter().map(|entry| entry.command_id).collect();
        let ledger = events_of(stores, account, &case_keys, &command_ids, max_events).await;

        Self {
            turn_id,
            error_code,
            acts: acts_of(record.as_ref()),
            target_resolutions: resolutions_of(record.as_ref()),
            understanding: record
                .as_ref()
                .and_then(|record| record.understanding.clone()),
            commands: entries
                .iter()
                .map(|entry| entry.command_type.clone())
                .collect(),
            events: ledger.events,
            events_truncated: ledger.truncated,
            revisions: revisions_of(workflows, account, &case_keys).await,
            states: states_of(workflows, account, cases).await,
            interactions: interactions_of(stores, account, turn_id, &case_keys, record.as_ref())
                .await,
            blocks: turn
                .map(|turn| turn.blocks.iter().map(BlockKind::of).collect())
                .unwrap_or_default(),
            phase: ConversationReader::turn_phase(
                stores.conversations().as_ref(),
                account,
                &turn_id,
            )
            .await
            .ok()
            .map(|marker| marker.phase),
            provider_failures: provider_failures_of(record.as_ref()),
            discarded_answers: record
                .as_ref()
                .map(ReplayRecord::discarded_answers)
                .unwrap_or_default(),
            cards_created: record
                .as_ref()
                .map_or(0, |record| record.interactions_created.len()),
            answer: turn.map(narration).unwrap_or_default(),
        }
    }

    /// The revision a case ended the turn at, when it was observed.
    #[must_use]
    pub fn revision_of(&self, case: &CaseKey) -> Option<u64> {
        self.revisions
            .iter()
            .find(|(key, _)| key == case)
            .map(|(_, revision)| *revision)
    }

    /// The cards of one case, oldest first.
    #[must_use]
    pub fn interactions_of(&self, case: &CaseKey) -> Vec<&ObservedInteraction> {
        self.interactions
            .iter()
            .filter(|card| &card.case == case)
            .collect()
    }

    /// A canonical rendering of the deterministic facts of this run.
    ///
    /// Two samples with the same signature behaved the same way; two with
    /// different signatures did not. It deliberately excludes the model's
    /// wording, block identifiers and timestamps, because a run that phrases
    /// the same receipt differently is not a different behaviour — and counting
    /// it as one would make every item look flaky.
    #[must_use]
    pub fn signature(&self) -> String {
        let mut out = String::new();
        if let Some(code) = &self.error_code {
            let _ = writeln!(out, "error={code}");
        }
        for act in &self.acts {
            let _ = writeln!(
                out,
                "act={} op={}",
                act.kind,
                act.operation.as_deref().unwrap_or("-")
            );
        }
        for resolution in &self.target_resolutions {
            let _ = writeln!(
                out,
                "target[{}]={} case={}",
                resolution.act_index,
                resolution.resolution,
                resolution.case_id.as_ref().map_or("-", CaseId::as_str)
            );
        }
        let _ = writeln!(out, "commands={}", self.commands.join(","));
        let _ = writeln!(out, "events={}", self.events.join(","));
        if self.events_truncated {
            // Only written when it happened, so an ordinary signature is
            // unchanged — and two runs that differ only in whether the ledger
            // was cut never collapse into one behaviour.
            let _ = writeln!(out, "events_truncated=true");
        }
        for (case, revision) in &self.revisions {
            let _ = writeln!(out, "rev {}/{}={revision}", case.workflow, case.case_id);
        }
        for card in &self.interactions {
            let _ = writeln!(
                out,
                "card {}/{} {:?} {:?}",
                card.case.workflow, card.case.case_id, card.kind, card.status
            );
        }
        let blocks: Vec<&str> = self.blocks.iter().map(|kind| kind.as_str()).collect();
        let _ = writeln!(out, "blocks={}", blocks.join(","));
        let _ = writeln!(out, "phase={:?}", self.phase);
        // `discarded_answers` is deliberately absent. The signature answers
        // "did these two runs behave the same", and a repair round that the
        // runtime recovered from changed how the turn got to its answer, not
        // what the answer was. Counting it here would report an item as having
        // three distinct behaviours because one sample needed a second round to
        // reach the same effects — which reads as instability in the turn and
        // is instability in the road to it. It is measured on its own axis
        // instead; see `ItemReport::samples_with_discards`.
        out
    }

    /// The stable codes of the answers this turn lost, in order and with
    /// repeats.
    ///
    /// The reason strings are for a person reading one turn; these are what a
    /// run groups by.
    #[must_use]
    pub fn discard_codes(&self) -> Vec<&str> {
        self.discarded_answers
            .iter()
            .map(|discarded| discarded.code.as_str())
            .collect()
    }
}

fn acts_of(record: Option<&ReplayRecord>) -> Vec<ObservedAct> {
    let Some(record) = record else {
        return Vec::new();
    };
    // One outcome per recorded act; a missing one means the reduction never ran.
    message_acts(record)
        .map(|(at, act)| ObservedAct::of(act, record.act_outcomes.get(at)))
        .collect()
}

fn resolutions_of(record: Option<&ReplayRecord>) -> Vec<ObservedResolution> {
    let Some(record) = record else {
        return Vec::new();
    };
    let acts: Vec<&UnderstoodAct> = message_acts(record).map(|(_, act)| act).collect();
    record
        .target_resolutions
        .iter()
        .filter_map(|entry| {
            let at = acts.iter().position(|act| act.id == entry.act)?;
            Some(ObservedResolution::of(at, &entry.resolution))
        })
        .collect()
}

/// The message's own acts, each with its position among every recorded act. The acts a
/// card answer stands for are recorded too, in the card's unit, and are not the message's.
fn message_acts(record: &ReplayRecord) -> impl Iterator<Item = (usize, &UnderstoodAct)> {
    record
        .understanding
        .iter()
        .flat_map(|understanding| understanding.acts.iter().enumerate())
        .filter(|(_, act)| act.id.unit != CARD_UNIT)
}

fn provider_failures_of(record: Option<&ReplayRecord>) -> usize {
    record.map_or(0, |record| {
        record
            .provider_attempts
            .iter()
            .filter(|attempt| {
                matches!(
                    attempt.outcome,
                    ProviderAttemptOutcome::Failed { .. } | ProviderAttemptOutcome::FellBack { .. }
                )
            })
            .count()
    })
}

/// The ledger as one observation saw it.
struct Ledger {
    /// Event types committed by this turn, in append order.
    events: Vec<String>,
    /// Whether a configured bound cut the list short.
    truncated: bool,
}

/// The events this turn committed, across every case it touched, in append
/// order.
///
/// The filter on command identifiers is what makes this "the turn's events" and
/// not "the case's history": an item that starts from a seeded state with prior
/// events must not see them in its own assertion.
///
/// # Why the cursor and not the revision
///
/// [`EventJournalReader::list_since`] pages by case revision, and a revision is not a
/// position: one command commits several events at the same revision, so a
/// `limit` that lands inside a revision cannot be resumed from. Reading a case
/// that way means picking a number and hoping no history is longer than it —
/// which is exactly the silent bound this crate is not allowed to have.
/// [`EventJournalReader::read_from`] pages by the store-assigned sequence, which *is*
/// a position, so the loop below simply keeps going until the journal reports
/// an empty page. The sequence is also the journal's total order across cases,
/// so the events come out in append order with nothing left to sort.
async fn events_of(
    stores: &Stores,
    account: &AccountId,
    cases: &[CaseKey],
    command_ids: &BTreeSet<turnframe_core::ids::CommandId>,
    max_events: Option<usize>,
) -> Ledger {
    let mut events = Vec::new();
    let mut truncated = false;
    let mut cursor = EventCursor::START;
    'paging: loop {
        let Ok(page) = EventJournalReader::read_from(
            stores.events().as_ref(),
            account,
            cursor,
            EVENT_PAGE_SIZE,
        )
        .await
        else {
            break;
        };
        if page.is_empty() {
            break;
        }
        cursor = page.next_cursor;
        for event in page.events {
            if !command_ids.contains(&event.command_id) || !cases.contains(&event.case_key) {
                continue;
            }
            if max_events.is_some_and(|limit| events.len() >= limit) {
                truncated = true;
                break 'paging;
            }
            events.push(event.event_type);
        }
    }
    Ledger { events, truncated }
}

/// Reads each seeded case back, keeping the state the item declared beside it.
async fn states_of(
    workflows: &WorkflowRegistry,
    account: &AccountId,
    cases: &[CaseSeed],
) -> Vec<(CaseKey, ObservedState)> {
    let mut found = Vec::new();
    for seed in cases {
        let key = CaseKey::new(seed.workflow.clone(), seed.case_id.clone());
        let Some(registered) = workflows.get(&seed.workflow) else {
            continue;
        };
        let after = match registered.executor.load(account, &seed.case_id).await {
            Ok(loaded) => loaded.value,
            // A case that cannot be read is not a case that did not move: the
            // difference matters to `unchanged`, so nothing is recorded and the
            // assertion says it could not look rather than passing quietly.
            Err(_) => continue,
        };
        found.push((
            key,
            ObservedState {
                before: seed.state.clone(),
                after,
            },
        ));
    }
    found
}

async fn revisions_of(
    workflows: &WorkflowRegistry,
    account: &AccountId,
    cases: &[CaseKey],
) -> Vec<(CaseKey, u64)> {
    let mut found = Vec::new();
    for case in cases {
        let Some(registered) = workflows.get(&case.workflow) else {
            continue;
        };
        if let Ok(loaded) = registered.executor.load(account, &case.case_id).await {
            found.push((case.clone(), loaded.revision.0));
        }
    }
    found
}

/// The cards worth observing: the ones this turn created, the ones it
/// *answered*, and whatever is still open on the seeded cases.
///
/// The answered card is the one that would otherwise vanish from the report. A
/// turn that resolves a confirmation leaves it `Resolved`, which means it is no
/// longer open and was not created this turn — so an item asserting "the card
/// ended up resolved" would see nothing at all. It is read back from the turn's
/// own persisted input.
async fn interactions_of(
    stores: &Stores,
    account: &AccountId,
    turn_id: TurnId,
    cases: &[CaseKey],
    record: Option<&ReplayRecord>,
) -> Vec<ObservedInteraction> {
    let mut ids: Vec<InteractionId> = Vec::new();
    for id in record
        .iter()
        .flat_map(|record| &record.interactions_created)
    {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    if let Ok(stored) =
        ConversationReader::load_turn(stores.conversations().as_ref(), account, &turn_id).await
        && let Some(answered) = stored.user.input.interaction_response.as_ref()
        && !ids.contains(&answered.interaction_id)
    {
        ids.push(answered.interaction_id);
    }
    for case in cases {
        let open =
            InteractionReader::list_open_for_case(stores.interactions().as_ref(), account, case)
                .await
                .unwrap_or_default();
        for card in open {
            if !ids.contains(&card.id) {
                ids.push(card.id);
            }
        }
    }

    let mut found = Vec::new();
    for id in ids {
        let Ok(record) = InteractionReader::get(stores.interactions().as_ref(), account, &id).await
        else {
            continue;
        };
        let card = record.interaction;
        found.push((
            card.created_at,
            ObservedInteraction {
                id: card.id,
                case: card.case_ref.key(),
                kind: card.kind,
                status: card.status,
                blocking: card.blocking,
                created_by_turn: card.created_by_turn,
            },
        ));
    }
    found.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.id.cmp(&right.1.id)));
    found.into_iter().map(|(_, card)| card).collect()
}

/// The model-authored text of a turn — answers and transitions, nothing else.
///
/// Receipts, notices and cards are server-authored: their wording is the
/// library's, not the model's, so grading it would measure Turnframe rather
/// than the model under test.
fn narration(turn: &AssistantTurn) -> String {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer.text.as_str()),
            ResponseBlock::Transition(transition) => Some(transition.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}
