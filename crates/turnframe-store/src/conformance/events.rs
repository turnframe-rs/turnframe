//! Checks for [`EventJournal`](crate::events::EventJournal).

use chrono::{DateTime, Utc};
use turnframe_core::ids::{CaseRevision, CommandId, EventId};

use super::ConformanceFailure;
use super::fixtures::{
    PERSONAL_DATA, account, at, case_key, ensure, ensure_code, ensure_eq, ensure_error, ensure_ok,
    epoch, event_batch, event_batch_for, event_batch_with_personal_data, other_account,
    other_case_key, other_redaction_authority, redaction_authority,
};
use crate::error::{StoreError, codes};
use crate::events::{EventCursor, StoredEvent};
use crate::stores::Stores;

/// The ledger appends in order, reads back in that order, and refuses a batch
/// whole (spec §17.1, ADR-012).
///
/// Receipts are only allowed to cite committed events, so a reader that
/// silently dropped or reordered one would let the assistant claim something
/// that did not happen. This check pins append order, the `since` cursor, the
/// by-identifier read used for receipt verification, and the atomicity of a
/// batch that contains a duplicate.
pub async fn check_event_append_ordering_and_get_by_ids(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_event_append_ordering_and_get_by_ids";
    let events = stores.events();
    let account = account();
    let command = CommandId::new();

    let first_ids = vec![EventId::new(), EventId::new()];
    let appended = ensure_ok(
        CHECK,
        "appending the first batch",
        events
            .append(event_batch(&account, command, 1, &first_ids, epoch()))
            .await,
    )?;
    ensure_eq(
        CHECK,
        "append returns the identifiers in batch order",
        &appended,
        &first_ids,
    )?;

    let second_ids = vec![EventId::new()];
    ensure_ok(
        CHECK,
        "appending the second batch",
        events
            .append(event_batch(
                &account,
                CommandId::new(),
                2,
                &second_ids,
                at(1),
            ))
            .await,
    )?;

    let history = ensure_ok(
        CHECK,
        "listing the whole history",
        events
            .list_since(&account, &case_key(), CaseRevision::ZERO, 10)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "history in append order",
        &history.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        &vec![first_ids[0], first_ids[1], second_ids[0]],
    )?;
    ensure(
        CHECK,
        history
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence),
        "the store-assigned sequence must strictly increase in append order",
    )?;
    ensure_eq(
        CHECK,
        "the revision stamped on an event",
        &history[2].case_revision,
        &CaseRevision(2),
    )?;

    let since = ensure_ok(
        CHECK,
        "listing from revision 1",
        events
            .list_since(&account, &case_key(), CaseRevision(1), 10)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "list_since is exclusive on the cursor",
        &since.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        &second_ids,
    )?;

    let limited = ensure_ok(
        CHECK,
        "listing with a limit",
        events
            .list_since(&account, &case_key(), CaseRevision::ZERO, 2)
            .await,
    )?;
    ensure_eq(CHECK, "the limit is honoured", &limited.len(), &2)?;

    // Receipt verification reads by identifier; the answer must follow the
    // request and quietly omit anything that is not this tenant's.
    let requested = vec![second_ids[0], EventId::new(), first_ids[0]];
    let by_ids = ensure_ok(
        CHECK,
        "reading events by identifier",
        events.get_by_ids(&account, &requested).await,
    )?;
    ensure_eq(
        CHECK,
        "events come back in the order requested, unknown ones omitted",
        &by_ids.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        &vec![second_ids[0], first_ids[0]],
    )?;
    let foreign = ensure_ok(
        CHECK,
        "reading this tenant's events as another tenant",
        events.get_by_ids(&other_account(), &requested).await,
    )?;
    ensure(
        CHECK,
        foreign.is_empty(),
        "another tenant must not read these events by identifier",
    )?;

    let count = ensure_ok(
        CHECK,
        "counting the events of the case",
        events.count(&account, &case_key()).await,
    )?;
    ensure_eq(CHECK, "events on the case", &count, &3)?;

    // A batch is all or nothing: one duplicate rejects it whole.
    let fresh = EventId::new();
    ensure_error(
        CHECK,
        "appending a batch containing an event that already exists",
        events
            .append(event_batch(
                &account,
                command,
                3,
                &[fresh, first_ids[0]],
                at(2),
            ))
            .await,
        &StoreError::Conflict,
    )?;
    let after_conflict = ensure_ok(
        CHECK,
        "counting after the rejected batch",
        events.count(&account, &case_key()).await,
    )?;
    ensure_eq(
        CHECK,
        "a rejected batch must leave nothing behind",
        &after_conflict,
        &3,
    )?;
    let orphan = ensure_ok(
        CHECK,
        "reading the event of the rejected batch",
        events.get_by_ids(&account, &[fresh]).await,
    )?;
    ensure(
        CHECK,
        orphan.is_empty(),
        "no event of a rejected batch may be visible",
    )?;

    ensure_code(
        CHECK,
        "appending an empty batch",
        events
            .append(event_batch(&account, command, 4, &[], at(3)))
            .await,
        codes::INVALID_RECORD,
    )
}

/// The sequence cursor pages the stream exactly once, whatever else is being
/// appended while a consumer walks it (spec §17.1, §22.2).
///
/// This is the read a projector, an exporter or any consumer with an offset of
/// its own depends on, and the two ways to get it wrong are opposite: a store
/// that treats the cursor as inclusive re-delivers an event the consumer has
/// already acted on, and a store that resumes from anything but the last event
/// *handed out* — the highest sequence it holds, say, or a revision — silently
/// drops the events that arrived while the page was in flight. The check makes
/// the journal grow between pages, in the middle of a revision and under
/// another tenant's traffic, and demands that the union of the pages be every
/// event of the account, in commit order, with no repeats.
pub async fn check_event_stream_cursor_pages_exactly_once(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_event_stream_cursor_pages_exactly_once";
    let events = stores.events();
    let account = account();

    // One command, one revision, three events: paging two at a time has to cut
    // this revision in half, which is exactly what `list_since` cannot resume
    // from and what this read exists for.
    let first: Vec<EventId> = (0..3).map(|_| EventId::new()).collect();
    ensure_ok(
        CHECK,
        "appending three events at one revision",
        events
            .append(event_batch(&account, CommandId::new(), 1, &first, epoch()))
            .await,
    )?;

    let page = ensure_ok(
        CHECK,
        "reading the first page",
        events.read_from(&account, EventCursor::START, 2).await,
    )?;
    ensure_eq(
        CHECK,
        "the first page honours the limit",
        &page.events.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        &first[..2].to_vec(),
    )?;
    ensure(
        CHECK,
        page.next_cursor == EventCursor::after(page.events[1].sequence),
        "the page must resume after the last event it carried",
    )?;

    // The journal grows while the consumer is between pages: a second case of
    // the same account, and a second tenant whose events must stay invisible
    // without consuming anyone's limit.
    let second: Vec<EventId> = (0..2).map(|_| EventId::new()).collect();
    ensure_ok(
        CHECK,
        "appending to another case of the same account",
        events
            .append(event_batch_for(
                &account,
                &other_case_key(),
                CommandId::new(),
                1,
                &second,
                at(1),
            ))
            .await,
    )?;
    let foreign = vec![EventId::new()];
    ensure_ok(
        CHECK,
        "appending an event of another tenant",
        events
            .append(event_batch(
                &other_account(),
                CommandId::new(),
                1,
                &foreign,
                at(2),
            ))
            .await,
    )?;
    let third: Vec<EventId> = vec![EventId::new()];
    ensure_ok(
        CHECK,
        "appending one more event of the first case",
        events
            .append(event_batch(&account, CommandId::new(), 2, &third, at(3)))
            .await,
    )?;

    // Walk to the end from the cursor the consumer was holding all along.
    let mut cursor = page.next_cursor;
    let mut seen: Vec<EventId> = page.events.iter().map(|e| e.event_id).collect();
    let mut sequences: Vec<u64> = page.events.iter().map(|e| e.sequence).collect();
    for _ in 0..10 {
        let next = ensure_ok(
            CHECK,
            "reading the next page",
            events.read_from(&account, cursor, 2).await,
        )?;
        if next.is_empty() {
            ensure(
                CHECK,
                next.next_cursor == cursor,
                "an empty page must leave the cursor where it was",
            )?;
            break;
        }
        ensure(
            CHECK,
            next.next_cursor > cursor,
            "a non-empty page must move the cursor forward",
        )?;
        cursor = next.next_cursor;
        seen.extend(next.events.iter().map(|e| e.event_id));
        sequences.extend(next.events.iter().map(|e| e.sequence));
    }

    let expected: Vec<EventId> = first
        .iter()
        .chain(second.iter())
        .chain(third.iter())
        .copied()
        .collect();
    ensure_eq(
        CHECK,
        "every event of the account, once each, in commit order",
        &seen,
        &expected,
    )?;
    ensure(
        CHECK,
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "the pages must not repeat or reorder a sequence",
    )?;
    ensure(
        CHECK,
        !seen.contains(&foreign[0]),
        "another tenant's event must never appear in this account's stream",
    )?;

    // Caught up, then an append: the consumer sees only what is new.
    let caught_up = ensure_ok(
        CHECK,
        "reading after the last event",
        events.read_from(&account, cursor, 10).await,
    )?;
    ensure(
        CHECK,
        caught_up.is_empty() && caught_up.next_cursor == cursor,
        "a consumer that has caught up gets an empty page and keeps its cursor",
    )?;
    let late = vec![EventId::new()];
    ensure_ok(
        CHECK,
        "appending after the consumer caught up",
        events
            .append(event_batch(&account, CommandId::new(), 3, &late, at(4)))
            .await,
    )?;
    let resumed = ensure_ok(
        CHECK,
        "reading again from the stored cursor",
        events.read_from(&account, cursor, 10).await,
    )?;
    ensure_eq(
        CHECK,
        "only the event appended after the consumer caught up",
        &resumed
            .events
            .iter()
            .map(|e| e.event_id)
            .collect::<Vec<_>>(),
        &late,
    )?;

    // The same walk from START must produce the same stream: a cursor read is a
    // function of the journal, not of when the consumer started.
    let mut replayed: Vec<EventId> = Vec::new();
    let mut from_start = EventCursor::START;
    for _ in 0..10 {
        let page = ensure_ok(
            CHECK,
            "replaying the whole stream one event at a time",
            events.read_from(&account, from_start, 1).await,
        )?;
        if page.is_empty() {
            break;
        }
        from_start = page.next_cursor;
        replayed.extend(page.events.iter().map(|e| e.event_id));
    }
    let mut all = expected;
    all.extend(late);
    ensure_eq(
        CHECK,
        "a full replay sees the same events in the same order",
        &replayed,
        &all,
    )?;

    // A foreign cursor is not a way into another tenant's stream.
    let foreign_view = ensure_ok(
        CHECK,
        "reading this account's stream as another tenant",
        events
            .read_from(&other_account(), EventCursor::START, 10)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "the other tenant sees only its own event",
        &foreign_view
            .events
            .iter()
            .map(|e| e.event_id)
            .collect::<Vec<_>>(),
        &foreign,
    )
}

/// A limit given to [`list_since`](crate::events::EventJournalReader::list_since) is
/// a cap on the answer, not a page boundary.
///
/// The revision cursor cannot express "continue after the third event of
/// revision 1", so a consumer that paged with it would either re-read the whole
/// revision or lose the rest of it. The check pins that this is what the method
/// does — the caller is not being short-changed by an implementation, it is
/// reading the wrong method — and that the cursor read recovers exactly the
/// events the revision cursor could not name.
pub async fn check_revision_read_truncates_inside_a_revision(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_revision_read_truncates_inside_a_revision";
    let events = stores.events();
    let account = account();

    let ids: Vec<EventId> = (0..3).map(|_| EventId::new()).collect();
    ensure_ok(
        CHECK,
        "appending three events at one revision",
        events
            .append(event_batch(&account, CommandId::new(), 1, &ids, epoch()))
            .await,
    )?;

    let truncated = ensure_ok(
        CHECK,
        "listing the case history with a limit of two",
        events
            .list_since(&account, &case_key(), CaseRevision::ZERO, 2)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "the limit cuts the revision in half",
        &truncated.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        &ids[..2].to_vec(),
    )?;
    let resumed_by_revision = ensure_ok(
        CHECK,
        "trying to continue by revision",
        events
            .list_since(&account, &case_key(), CaseRevision(1), 2)
            .await,
    )?;
    ensure(
        CHECK,
        resumed_by_revision.is_empty(),
        "continuing past revision 1 skips the rest of revision 1: that is why \
         the revision is not a page boundary",
    )?;

    let by_sequence = ensure_ok(
        CHECK,
        "continuing by sequence instead",
        events
            .read_from(
                &account,
                EventCursor::after(truncated[1].sequence),
                usize::MAX,
            )
            .await,
    )?;
    ensure_eq(
        CHECK,
        "the sequence cursor recovers the event the revision cursor lost",
        &by_sequence
            .events
            .iter()
            .map(|e| e.event_id)
            .collect::<Vec<_>>(),
        &ids[2..].to_vec(),
    )
}

/// Erasure redacts a payload **in place**: nothing appears, disappears or
/// changes position (spec §17.1, ADR-012).
///
/// This is the check that stops a store from meeting an erasure obligation by
/// deleting the row, which is the tempting implementation and the one that
/// breaks the property the whole architecture rests on. A receipt may only cite
/// committed events, and a consumer pages the journal by a sequence that must
/// never skip; a delete quietly invalidates every claim resting on that event
/// and drops a position no consumer will ever be handed again, and nothing
/// fails until somebody asks whether an assistant's statement was true.
///
/// So the check reads the whole stream before the erasure, redacts one event in
/// the middle of it, and demands the same events back in the same order with
/// the same sequences and the same everything-but-the-payload. It also plants a
/// recognisable string in the payload and looks for it afterwards, because a
/// store that moved the payload instead of removing it would otherwise pass.
pub async fn check_event_redaction_preserves_identity_and_order(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_event_redaction_preserves_identity_and_order";
    let events = stores.events();
    let account = account();

    // Two revisions of one case, so the redacted event has neighbours on both
    // sides, and one event of another tenant that must not be touched.
    let first: Vec<EventId> = (0..2).map(|_| EventId::new()).collect();
    ensure_ok(
        CHECK,
        "appending the first batch",
        events
            .append(event_batch_with_personal_data(
                &account,
                CommandId::new(),
                1,
                &first,
                epoch(),
            ))
            .await,
    )?;
    let second = vec![EventId::new()];
    ensure_ok(
        CHECK,
        "appending the second batch",
        events
            .append(event_batch_with_personal_data(
                &account,
                CommandId::new(),
                2,
                &second,
                at(1),
            ))
            .await,
    )?;
    let foreign = vec![EventId::new()];
    ensure_ok(
        CHECK,
        "appending an event of another tenant",
        events
            .append(event_batch_with_personal_data(
                &other_account(),
                CommandId::new(),
                1,
                &foreign,
                at(2),
            ))
            .await,
    )?;

    let before = ensure_ok(
        CHECK,
        "reading the whole stream before the erasure",
        events.read_from(&account, EventCursor::START, 10).await,
    )?;
    ensure_eq(
        CHECK,
        "the stream before the erasure",
        &before.events.len(),
        &3,
    )?;

    // The middle event: a delete would be invisible at either end.
    let target = first[1];
    let record = ensure_ok(
        CHECK,
        "redacting the payload of one event",
        events
            .redact_payload(&account, &target, &redaction_authority())
            .await,
    )?;
    ensure_eq(
        CHECK,
        "the erasure is recorded under the authority it was asked for",
        &record.authority,
        &redaction_authority(),
    )?;

    let after = ensure_ok(
        CHECK,
        "reading the whole stream after the erasure",
        events.read_from(&account, EventCursor::START, 10).await,
    )?;
    ensure_eq(
        CHECK,
        "an erasure must not add, remove or move an event",
        &after.events.iter().map(identity_of).collect::<Vec<_>>(),
        &before.events.iter().map(identity_of).collect::<Vec<_>>(),
    )?;
    ensure_eq(
        CHECK,
        "the cursor at the end of the stream must not move",
        &after.next_cursor,
        &before.next_cursor,
    )?;
    ensure_eq(
        CHECK,
        "the events of the case are still counted",
        &ensure_ok(
            CHECK,
            "counting after the erasure",
            events.count(&account, &case_key()).await,
        )?,
        &3,
    )?;

    // Exactly one event changed, and it changed in exactly one way.
    for (was, is) in before.events.iter().zip(after.events.iter()) {
        if is.event_id == target {
            ensure(
                CHECK,
                is.is_redacted(),
                "the redacted event must be marked as redacted",
            )?;
            ensure_eq(
                CHECK,
                "the payload of a redacted event",
                &is.payload,
                &serde_json::Value::Null,
            )?;
            ensure(
                CHECK,
                !serde_json::to_string(is)
                    .unwrap_or_default()
                    .contains(PERSONAL_DATA),
                "the erased value must not survive anywhere on the stored event, \
                 including in the record of the erasure",
            )?;
            ensure(
                CHECK,
                is.to_receipt_event().is_redacted(),
                "a redacted event must reach a receipt renderer as redacted",
            )?;
        } else {
            ensure_eq(CHECK, "an event nobody asked to erase", is, was)?;
            ensure(
                CHECK,
                !is.is_redacted() && !is.to_receipt_event().is_redacted(),
                "erasing one payload must not mark another event as redacted",
            )?;
        }
    }

    // The by-identifier read is how a receipt is verified: the redacted event
    // must still answer to its own id, or every claim resting on it dies.
    let by_ids = ensure_ok(
        CHECK,
        "reading the redacted event by identifier",
        events.get_by_ids(&account, &[target]).await,
    )?;
    ensure_eq(
        CHECK,
        "a redacted event is still readable by identifier",
        &by_ids.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        &vec![target],
    )?;
    let history = ensure_ok(
        CHECK,
        "listing the case history after the erasure",
        events
            .list_since(&account, &case_key(), CaseRevision::ZERO, 10)
            .await,
    )?;
    ensure_eq(
        CHECK,
        "the case history still carries every event, in order",
        &history.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        &vec![first[0], first[1], second[0]],
    )?;

    // A consumer paging one event at a time still gets each position once:
    // this is what a delete would break without any read looking wrong.
    let mut walked: Vec<EventId> = Vec::new();
    let mut cursor = EventCursor::START;
    for _ in 0..10 {
        let page = ensure_ok(
            CHECK,
            "paging the stream after the erasure",
            events.read_from(&account, cursor, 1).await,
        )?;
        if page.is_empty() {
            break;
        }
        cursor = page.next_cursor;
        walked.extend(page.events.iter().map(|e| e.event_id));
    }
    ensure_eq(
        CHECK,
        "every position of the journal is still delivered exactly once",
        &walked,
        &vec![first[0], first[1], second[0]],
    )?;

    // Another tenant's ledger is not collateral damage.
    let elsewhere = ensure_ok(
        CHECK,
        "reading the other tenant's stream",
        events
            .read_from(&other_account(), EventCursor::START, 10)
            .await,
    )?;
    ensure(
        CHECK,
        elsewhere.events.len() == 1 && !elsewhere.events[0].is_redacted(),
        "an erasure in one tenant must not touch another tenant's events",
    )
}

/// The erasure is itself an auditable act, it is idempotent, and it cannot be
/// aimed across tenants (spec §17.1, §25.4).
///
/// The record lives on the event it redacted rather than in a log beside it, so
/// this check reads it back off the event and demands it be exactly what the
/// call reported. Repeating the request must return the **first** record: an
/// erasure retried by an operator, a queue or a support tool must not rewrite
/// the history of who erased what, and there is nothing left to erase the
/// second time. And an identifier that belongs to another tenant is `NotFound`,
/// indistinguishable from one that never existed — a redaction is a write, and
/// a write is not a way to probe for the existence of another tenant's data.
pub async fn check_event_redaction_is_audited_and_idempotent(
    stores: &Stores,
) -> Result<(), ConformanceFailure> {
    const CHECK: &str = "check_event_redaction_is_audited_and_idempotent";
    let events = stores.events();
    let account = account();

    let ids: Vec<EventId> = (0..2).map(|_| EventId::new()).collect();
    ensure_ok(
        CHECK,
        "appending two events",
        events
            .append(event_batch_with_personal_data(
                &account,
                CommandId::new(),
                1,
                &ids,
                epoch(),
            ))
            .await,
    )?;

    let record = ensure_ok(
        CHECK,
        "redacting a payload",
        events
            .redact_payload(&account, &ids[0], &redaction_authority())
            .await,
    )?;

    let stored = ensure_ok(
        CHECK,
        "reading the redacted event back",
        events.get_by_ids(&account, &[ids[0]]).await,
    )?;
    let stored = stored
        .first()
        .ok_or_else(|| ConformanceFailure::new(CHECK, "the redacted event is gone"))?;
    ensure_eq(
        CHECK,
        "the erasure the store reported is the erasure it persisted",
        &stored.redaction,
        &Some(record.clone()),
    )?;

    // Retrying the request: same record, and nothing else moves.
    let repeated = ensure_ok(
        CHECK,
        "redacting the same payload again under another authority",
        events
            .redact_payload(&account, &ids[0], &other_redaction_authority())
            .await,
    )?;
    ensure_eq(
        CHECK,
        "a repeated erasure keeps the record of the first one",
        &repeated,
        &record,
    )?;
    let after_repeat = ensure_ok(
        CHECK,
        "reading the event after the repeated erasure",
        events.get_by_ids(&account, &[ids[0]]).await,
    )?;
    ensure_eq(
        CHECK,
        "a repeated erasure rewrites nothing",
        &after_repeat.first(),
        &Some(stored),
    )?;
    ensure_eq(
        CHECK,
        "the events of the case after two erasure requests",
        &ensure_ok(
            CHECK,
            "counting the events of the case",
            events.count(&account, &case_key()).await,
        )?,
        &2,
    )?;

    // An event that does not exist, and one that belongs elsewhere, answer the
    // same way.
    ensure_error(
        CHECK,
        "redacting an event that does not exist",
        events
            .redact_payload(&account, &EventId::new(), &redaction_authority())
            .await,
        &StoreError::NotFound,
    )?;
    ensure_error(
        CHECK,
        "redacting this tenant's event as another tenant",
        events
            .redact_payload(&other_account(), &ids[1], &redaction_authority())
            .await,
        &StoreError::NotFound,
    )?;
    let untouched = ensure_ok(
        CHECK,
        "reading the event another tenant tried to redact",
        events.get_by_ids(&account, &[ids[1]]).await,
    )?;
    ensure(
        CHECK,
        untouched
            .first()
            .is_some_and(|event| !event.is_redacted() && event.payload.get("full_name").is_some()),
        "a refused erasure must leave the event exactly as it was",
    )
}

/// Everything an erasure must not disturb, as one comparable value.
fn identity_of(event: &StoredEvent) -> (u64, EventId, CaseRevision, &str, DateTime<Utc>) {
    (
        event.sequence,
        event.event_id,
        event.case_revision,
        event.event_type.as_str(),
        event.occurred_at,
    )
}
