//! The event journal: the append-only claim ledger (spec §17.1, ADR-012).
//!
//! Append-only in the sense that carries the guarantee: no event is inserted
//! between two others, removed or moved, and the only write touching a stored
//! one is [`EventJournalWriter::redact_payload`], which empties a payload
//! without disturbing identity, type, sequence or timestamps. A batch is
//! appended atomically, a duplicate id rejects the whole batch, and the store
//! assigns every event a [`StoredEvent::sequence`] that strictly increases
//! across the whole store. Payloads cross type-erased; every lookup is
//! account-scoped.
//!
//! There are **two reads and choosing wrong is a correctness bug**:
//! [`EventJournalReader::list_since`] pages by case revision, which is not a
//! position, and answers "what happened to this case since the revision I
//! hold"; [`EventJournalReader::read_from`] pages by the store-assigned
//! sequence, which is, and is the read for any consumer that must see every
//! event exactly once.
//!
//! Why erasure is a redaction rather than a delete, and how to design payloads
//! that need it less, is in
//! [`docs/persistence.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/persistence.md).
//!

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use turnframe_core::case::CaseKey;
use turnframe_core::event::{Commit, CommittedEvent, EventRedaction, ReceiptEvent, RedactedEvent};
use turnframe_core::ids::{AccountId, CaseRevision, CommandId, EventId, RedactionAuthority};

use crate::error::StoreError;

/// A committed event as persisted (spec §22.2, `tf_domain_events`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent {
    /// Store-assigned position in the journal, strictly increasing in append
    /// order across the whole store — not per case and not per account.
    ///
    /// It is a position, not a count: nothing promises it has no gaps, only
    /// that a later append gets a higher one. [`EventCursor`] pages by it; see
    /// the module documentation for why the revision cannot.
    pub sequence: u64,
    /// Identifier in the ledger.
    pub event_id: EventId,
    /// Owning tenant.
    pub account_id: AccountId,
    /// The case.
    pub case_key: CaseKey,
    /// Revision of the case produced by the commit that emitted the event.
    pub case_revision: CaseRevision,
    /// Command whose execution produced it.
    pub command_id: CommandId,
    /// Stable event type label.
    pub event_type: String,
    /// Type-erased payload, or JSON `null` once it has been erased.
    ///
    /// [`Self::redaction`] is what says which of the two it is; a domain whose
    /// event type legitimately serializes to `null` is not misread.
    pub payload: serde_json::Value,
    /// When it was committed.
    pub occurred_at: DateTime<Utc>,
    /// Present once the payload has been erased, saying when and on whose
    /// authority — never what was removed.
    ///
    /// It defaults to absent, so a record written before this field existed
    /// reads back as an event whose payload is intact, which it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<EventRedaction>,
}

impl StoredEvent {
    /// The core view of the event.
    ///
    /// The payload comes back as it is stored, so an erased event yields a
    /// `null` payload here. Use [`Self::to_receipt_event`] wherever the caller
    /// has to tell the two apart, which is everywhere a receipt is rendered.
    #[must_use]
    pub fn to_committed(&self) -> CommittedEvent<serde_json::Value> {
        CommittedEvent {
            event_id: self.event_id,
            event_type: self.event_type.clone(),
            occurred_at: self.occurred_at,
            payload: self.payload.clone(),
        }
    }

    /// Returns `true` when the payload has been erased.
    #[must_use]
    pub fn is_redacted(&self) -> bool {
        self.redaction.is_some()
    }

    /// The event in the form a receipt renderer takes, which distinguishes a
    /// payload that is there from one that was erased.
    #[must_use]
    pub fn to_receipt_event(&self) -> ReceiptEvent<serde_json::Value> {
        match &self.redaction {
            None => ReceiptEvent::Committed(self.to_committed()),
            Some(redaction) => ReceiptEvent::Redacted(RedactedEvent {
                event_id: self.event_id,
                event_type: self.event_type.clone(),
                occurred_at: self.occurred_at,
                redaction: redaction.clone(),
            }),
        }
    }
}

/// The events one command committed, with the revision they produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventBatch {
    /// Owning tenant.
    pub account_id: AccountId,
    /// The case.
    pub case_key: CaseKey,
    /// The command that produced the events.
    pub command_id: CommandId,
    /// Revision after the commit.
    pub revision: CaseRevision,
    /// The events, in commit order.
    pub events: Vec<CommittedEvent<serde_json::Value>>,
}

impl EventBatch {
    /// Builds a batch from an already erased list of events.
    #[must_use]
    pub fn new(
        account_id: AccountId,
        case_key: CaseKey,
        command_id: CommandId,
        revision: CaseRevision,
        events: Vec<CommittedEvent<serde_json::Value>>,
    ) -> Self {
        Self {
            account_id,
            case_key,
            command_id,
            revision,
            events,
        }
    }

    /// Builds a batch from a typed [`Commit`], erasing every event payload.
    ///
    /// # Errors
    /// * `Serialization` when an event payload cannot be rendered as JSON.
    pub fn from_commit<S, E: Serialize>(
        account_id: AccountId,
        case_key: CaseKey,
        command_id: CommandId,
        commit: &Commit<S, E>,
    ) -> Result<Self, StoreError> {
        let mut events = Vec::with_capacity(commit.events.len());
        for event in &commit.events {
            let payload =
                serde_json::to_value(&event.payload).map_err(|_| StoreError::Serialization)?;
            events.push(CommittedEvent {
                event_id: event.event_id,
                event_type: event.event_type.clone(),
                occurred_at: event.occurred_at,
                payload,
            });
        }
        Ok(Self::new(
            account_id,
            case_key,
            command_id,
            commit.new_revision,
            events,
        ))
    }

    /// Identifiers of the events, in order.
    #[must_use]
    pub fn event_ids(&self) -> Vec<EventId> {
        self.events.iter().map(|e| e.event_id).collect()
    }

    /// Returns `true` when the batch carries no event.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// The events one command committed, read back from the ledger, in the form a
/// receipt renderer takes.
///
/// It is the read-side counterpart of [`EventBatch`], and the difference is the
/// whole point: a batch is what an append carries, so every event in it has a
/// payload, while a read may hand back an event whose payload was erased. Build
/// these with [`group_for_receipts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerReceiptGroup {
    /// The case the events belong to.
    pub case_key: CaseKey,
    /// The command that produced them.
    pub command_id: CommandId,
    /// Revision the commit produced.
    pub revision: CaseRevision,
    /// The events, in append order, erasures carried through.
    pub events: Vec<ReceiptEvent<serde_json::Value>>,
}

/// Groups a ledger read by the command that produced it, keeping append order,
/// and carrying every erasure through.
///
/// This is how a caller regenerating a response from the journal — the readback
/// ADR-012 point 6 requires — turns stored events into something a domain can
/// render receipts from. Going through [`StoredEvent::to_committed`] instead
/// would hand the domain a redacted event dressed as an intact one, with JSON
/// `null` where its payload used to be, and the type-erasure boundary would
/// then fail to deserialize it: a lost response where an honest receipt was
/// available.
///
/// Order is the order of `events`, and a group appears where its first event
/// did, so a regenerated response has the block order the original had.
#[must_use]
pub fn group_for_receipts(events: &[StoredEvent]) -> Vec<LedgerReceiptGroup> {
    let mut groups: Vec<LedgerReceiptGroup> = Vec::new();
    for event in events {
        let existing = groups.iter().position(|group| {
            group.case_key == event.case_key && group.command_id == event.command_id
        });
        match existing.and_then(|index| groups.get_mut(index)) {
            Some(group) => group.events.push(event.to_receipt_event()),
            None => groups.push(LedgerReceiptGroup {
                case_key: event.case_key.clone(),
                command_id: event.command_id,
                revision: event.case_revision,
                events: vec![event.to_receipt_event()],
            }),
        }
    }
    groups
}

/// A position in the journal's total order, for
/// [`EventJournalReader::read_from`].
///
/// It is exclusive: a read `after` a cursor returns events strictly beyond it,
/// so handing back the cursor of the page just consumed is exactly "continue
/// where I stopped". [`EventCursor::START`] is before every event.
///
/// A cursor is a store-assigned sequence, so it is only meaningful against the
/// store that issued it. Persist it next to whatever the consumer built from
/// the events and the consumer resumes exactly, across restarts.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct EventCursor(pub u64);

impl EventCursor {
    /// Before the first event of the journal.
    pub const START: Self = Self(0);

    /// The cursor that resumes strictly after the event at `sequence`.
    #[must_use]
    pub const fn after(sequence: u64) -> Self {
        Self(sequence)
    }

    /// The raw sequence this cursor sits on.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for EventCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One page of [`EventJournalReader::read_from`].
///
/// `next_cursor` is where the following call must resume. It is the sequence of
/// the last event in `events`, or the cursor that was asked for when the page
/// is empty, so a consumer can store it unconditionally and never has to
/// reason about the empty case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPage {
    /// The events, in sequence order, at most the requested limit.
    pub events: Vec<StoredEvent>,
    /// Where to resume. Never goes backwards.
    pub next_cursor: EventCursor,
}

impl EventPage {
    /// A page holding `events`, resuming after the last of them or at `asked`
    /// when there are none.
    #[must_use]
    pub fn new(events: Vec<StoredEvent>, asked: EventCursor) -> Self {
        let next_cursor = events
            .last()
            .map_or(asked, |event| EventCursor::after(event.sequence));
        Self {
            events,
            next_cursor,
        }
    }

    /// Returns `true` when the page carried no event, which is how a consumer
    /// learns it has caught up.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// The read half of the append-only claim ledger (spec §22.1).
///
/// The ledger is the only thing an operational receipt may cite, so reading it
/// is how a caller checks a claim; appending to it is a separate trait.
#[async_trait]
pub trait EventJournalReader: Send + Sync {
    /// Events of one case with `case_revision > since`, in append order, at
    /// most `limit`. `since = CaseRevision::ZERO` lists the whole history.
    ///
    /// This is the *history of a case*, and `limit` is a cap on the answer, not
    /// a page boundary: a revision that produced several events can be cut in
    /// half by it and the caller has no cursor to resume from, because the next
    /// call can only name a revision again. To consume a stream exactly once,
    /// use [`read_from`](EventJournalReader::read_from) instead; the module
    /// documentation lays the two side by side.
    ///
    /// # Errors
    /// * [`StoreError`] when the history could not be read.
    async fn list_since(
        &self,
        account: &AccountId,
        case_key: &CaseKey,
        since: CaseRevision,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError>;

    /// The account's next events after `after` in the journal's total order,
    /// across every case, at most `limit`, as an [`EventPage`].
    ///
    /// This is the exact-paging read. Start at [`EventCursor::START`], hand the
    /// page's `next_cursor` back on the next call, and every event of the
    /// account is delivered **once**, in commit order, however the journal grows
    /// in between: appends land at higher sequences than any cursor already
    /// issued, so they are seen by a later page and never re-order an earlier
    /// one. An empty page means the consumer has caught up; the cursor stays
    /// where it was and the call can simply be repeated later.
    ///
    /// `limit` bounds the page and nothing else. Events of other tenants
    /// between two of this account's events are skipped without consuming it,
    /// so a page is short only when the account has no more events, never
    /// because a neighbour was noisy. A `limit` of zero is legal and returns an
    /// empty page at the same cursor — which a consumer looping on
    /// [`EventPage::is_empty`] would read as "caught up", so do not pass one.
    ///
    /// # Errors
    /// * Whatever the backend raises; a page is never partial.
    async fn read_from(
        &self,
        account: &AccountId,
        after: EventCursor,
        limit: usize,
    ) -> Result<EventPage, StoreError>;

    /// The events with the given identifiers that exist for `account`, in the
    /// order requested; unknown or foreign identifiers are omitted. Receipt
    /// verification reads events back through this method (ADR-012 point 6).
    ///
    /// # Errors
    /// * [`StoreError`] when the events could not be read.
    async fn get_by_ids(
        &self,
        account: &AccountId,
        ids: &[EventId],
    ) -> Result<Vec<StoredEvent>, StoreError>;

    /// Number of events of a case.
    ///
    /// # Errors
    /// * [`StoreError`] when the count could not be read.
    async fn count(&self, account: &AccountId, case_key: &CaseKey) -> Result<u64, StoreError>;
}

/// The write half of the append-only claim ledger (spec §22.1).
#[async_trait]
pub trait EventJournalWriter: Send + Sync {
    /// Appends a batch atomically and returns the event identifiers in order.
    ///
    /// # Errors
    /// * `Other(INVALID_RECORD)` when the batch is empty.
    /// * `Conflict` when any `event_id` already exists for the account; nothing
    ///   of the batch is written.
    async fn append(&self, batch: EventBatch) -> Result<Vec<EventId>, StoreError>;

    /// Erases one event's payload in place, and records that it happened.
    ///
    /// This is the ledger's only answer to an erasure obligation, and the whole
    /// contract is in what it must **not** disturb (see the module
    /// documentation). After it returns:
    ///
    /// * the event is still there, with the same
    ///   [`event_id`](StoredEvent::event_id), the same
    ///   [`event_type`](StoredEvent::event_type), the same
    ///   [`sequence`](StoredEvent::sequence), the same
    ///   [`case_revision`](StoredEvent::case_revision),
    ///   [`command_id`](StoredEvent::command_id) and
    ///   [`occurred_at`](StoredEvent::occurred_at);
    /// * it is still returned by [`list_since`](EventJournalReader::list_since),
    ///   [`read_from`](EventJournalReader::read_from) and
    ///   [`get_by_ids`](EventJournalReader::get_by_ids), in the same position,
    ///   and still counted by [`count`](EventJournalReader::count);
    /// * its [`payload`](StoredEvent::payload) is JSON `null` and its
    ///   [`redaction`](StoredEvent::redaction) is present, so
    ///   [`StoredEvent::to_receipt_event`] hands the domain a
    ///   [`ReceiptEvent::Redacted`](turnframe_core::event::ReceiptEvent);
    /// * no other event is touched.
    ///
    /// **Implementing it as a delete is a contract violation**, not an
    /// optimisation: it breaks every claim that rests on the event and silently
    /// skips a position for every consumer paging by cursor.
    ///
    /// The record of the erasure is stored **on the event itself** rather than
    /// in a log beside it, so one write makes both the erasure and its audit
    /// trail, and a redacted payload without a record of who removed it is not
    /// a state this store can be in. It carries the instant and the
    /// [`RedactionAuthority`] and never what was removed. The store stamps the
    /// instant from its own clock, because an erasure is timed by the system
    /// that performed it.
    ///
    /// Repeating the call is a no-op that returns the **first** record: an
    /// erasure request that is retried must not rewrite the history of who
    /// erased what, and there is nothing left to erase the second time.
    ///
    /// # Errors
    /// * `NotFound` when no event of `account` has that identifier. An event of
    ///   another tenant is `NotFound` too, indistinguishable from absent
    ///   (spec §25.4).
    async fn redact_payload(
        &self,
        account: &AccountId,
        event_id: &EventId,
        authority: &RedactionAuthority,
    ) -> Result<EventRedaction, StoreError>;
}

/// The append-only claim ledger (spec §22.1): both halves.
///
/// There is nothing to implement here: write [`EventJournalReader`] and
/// [`EventJournalWriter`] and the blanket implementation below supplies this
/// trait.
pub trait EventJournal: EventJournalReader + EventJournalWriter {}

impl<T: EventJournalReader + EventJournalWriter + ?Sized> EventJournal for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_from_commit_erases_payloads() {
        #[derive(Serialize)]
        struct Ev {
            n: u32,
        }
        let commit: Commit<(), Ev> = Commit {
            state: None,
            new_revision: CaseRevision(3),
            events: vec![CommittedEvent {
                event_id: EventId::nil(),
                event_type: "t".into(),
                occurred_at: DateTime::<Utc>::UNIX_EPOCH,
                payload: Ev { n: 7 },
            }],
            idempotency_replay: false,
        };
        let batch = EventBatch::from_commit(
            AccountId::from("a"),
            CaseKey::new("w", "c"),
            CommandId::nil(),
            &commit,
        )
        .unwrap();
        assert_eq!(batch.revision, CaseRevision(3));
        assert_eq!(batch.events[0].payload, serde_json::json!({"n": 7}));
        assert_eq!(batch.event_ids(), vec![EventId::nil()]);
        assert!(!batch.is_empty());
    }

    #[test]
    fn cursor_is_exclusive_and_pages_carry_where_to_resume() {
        let event = |sequence: u64| StoredEvent {
            sequence,
            event_id: EventId::new(),
            account_id: AccountId::from("a"),
            case_key: CaseKey::new("w", "c"),
            case_revision: CaseRevision(1),
            command_id: CommandId::nil(),
            event_type: "t".into(),
            payload: serde_json::Value::Null,
            occurred_at: DateTime::<Utc>::UNIX_EPOCH,
            redaction: None,
        };

        assert_eq!(EventCursor::START, EventCursor(0));
        assert_eq!(EventCursor::default(), EventCursor::START);
        assert_eq!(EventCursor::after(7).value(), 7);
        assert_eq!(EventCursor::after(7).to_string(), "7");

        let page = EventPage::new(vec![event(4), event(5)], EventCursor::after(3));
        assert!(!page.is_empty());
        assert_eq!(
            page.next_cursor,
            EventCursor::after(5),
            "resume after the last event of the page"
        );

        let caught_up = EventPage::new(Vec::new(), page.next_cursor);
        assert!(caught_up.is_empty());
        assert_eq!(
            caught_up.next_cursor, page.next_cursor,
            "an empty page must not move the cursor"
        );

        let json = serde_json::to_string(&page).unwrap();
        assert!(json.contains("\"next_cursor\":5"), "{json}");
        assert_eq!(serde_json::from_str::<EventPage>(&json).unwrap(), page);
    }

    /// A stored event with `payload`, redacted when `redaction` says so.
    fn stored(sequence: u64, case: &str, command: CommandId, redacted: bool) -> StoredEvent {
        StoredEvent {
            sequence,
            event_id: EventId::new(),
            account_id: AccountId::from("a"),
            case_key: CaseKey::new("w", case),
            case_revision: CaseRevision(1),
            command_id: command,
            event_type: "t".into(),
            payload: if redacted {
                serde_json::Value::Null
            } else {
                serde_json::json!({ "full_name": "Marta Bianchi" })
            },
            occurred_at: DateTime::<Utc>::UNIX_EPOCH,
            redaction: redacted.then(|| EventRedaction {
                redacted_at: DateTime::<Utc>::UNIX_EPOCH,
                authority: RedactionAuthority::from("erasure-request-1"),
            }),
        }
    }

    #[test]
    fn a_redacted_event_reaches_a_receipt_renderer_as_redacted() {
        let intact = stored(1, "c", CommandId::nil(), false);
        assert!(!intact.is_redacted());
        assert!(!intact.to_receipt_event().is_redacted());
        assert_eq!(
            intact.to_receipt_event().payload(),
            Some(&serde_json::json!({ "full_name": "Marta Bianchi" }))
        );

        let erased = stored(2, "c", CommandId::nil(), true);
        assert!(erased.is_redacted());
        let event = erased.to_receipt_event();
        assert!(event.is_redacted(), "the payload is gone and it says so");
        assert_eq!(event.event_id(), erased.event_id, "identity survives");
        assert_eq!(event.event_type(), "t", "the type survives");
        assert_eq!(
            event.occurred_at(),
            erased.occurred_at,
            "the instant survives"
        );
        assert_eq!(
            event.redaction().map(|r| r.authority.as_str()),
            Some("erasure-request-1")
        );

        // The record travels with the event through serialization, and an event
        // written before the field existed reads back as intact.
        let json = serde_json::to_string(&erased).expect("a stored event serializes");
        assert_eq!(
            serde_json::from_str::<StoredEvent>(&json).expect("and deserializes"),
            erased
        );
        let older = serde_json::json!({
            "sequence": 1,
            "event_id": EventId::nil(),
            "account_id": "a",
            "case_key": { "workflow": "w", "case_id": "c" },
            "case_revision": 1,
            "command_id": CommandId::nil(),
            "event_type": "t",
            "payload": {},
            "occurred_at": "1970-01-01T00:00:00Z",
        });
        let older: StoredEvent = serde_json::from_value(older).expect("a record without the field");
        assert!(!older.is_redacted(), "no record means nothing was erased");
    }

    #[test]
    fn grouping_a_ledger_read_keeps_order_and_carries_erasures() {
        let (first, second) = (CommandId::new(), CommandId::new());
        let events = vec![
            stored(1, "c1", first, false),
            stored(2, "c1", first, true),
            stored(3, "c2", second, false),
            // Back to the first command: the group it belongs to already
            // exists, and it must not open a second one.
            stored(4, "c1", first, false),
        ];
        let groups = group_for_receipts(&events);

        assert_eq!(groups.len(), 2, "one group per command");
        assert_eq!(groups[0].case_key, CaseKey::new("w", "c1"));
        assert_eq!(groups[0].command_id, first);
        assert_eq!(groups[0].events.len(), 3);
        assert_eq!(groups[1].case_key, CaseKey::new("w", "c2"));
        assert_eq!(groups[1].events.len(), 1);

        assert_eq!(
            groups[0]
                .events
                .iter()
                .map(ReceiptEvent::is_redacted)
                .collect::<Vec<_>>(),
            vec![false, true, false],
            "the erasure of the middle event survives the grouping"
        );
        assert_eq!(
            groups[0]
                .events
                .iter()
                .map(ReceiptEvent::event_id)
                .collect::<Vec<_>>(),
            vec![events[0].event_id, events[1].event_id, events[3].event_id],
            "append order inside a group"
        );
        assert!(group_for_receipts(&[]).is_empty());
    }

    #[test]
    fn stored_event_to_committed() {
        let stored = StoredEvent {
            sequence: 1,
            event_id: EventId::nil(),
            account_id: AccountId::from("a"),
            case_key: CaseKey::new("w", "c"),
            case_revision: CaseRevision(1),
            command_id: CommandId::nil(),
            event_type: "t".into(),
            payload: serde_json::Value::Null,
            occurred_at: DateTime::<Utc>::UNIX_EPOCH,
            redaction: None,
        };
        let committed = stored.to_committed();
        assert_eq!(committed.event_id, EventId::nil());
        assert_eq!(committed.event_type, "t");
    }
}
