//! The event journal read as a stream: paging by sequence cursor while other
//! tasks append.
//!
//! The conformance suite pins the same rule deterministically, by growing the
//! journal between two page reads. This file does it the noisy way — a reader
//! task and two appender tasks on a multi-threaded runtime — because the point
//! of the cursor is that a consumer never has to know whether it is racing
//! anybody.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use chrono::{DateTime, Utc};
use turnframe_core::case::CaseKey;
use turnframe_core::event::CommittedEvent;
use turnframe_core::ids::{AccountId, CaseRevision, CommandId, EventId};
use turnframe_store::events::{EventBatch, EventCursor, EventJournal};
use turnframe_store::stores::Stores;

/// Events per append, cycling, so revisions of different sizes are cut by the
/// page limit in different places.
const BATCH_SIZES: [usize; 3] = [1, 3, 2];
const APPENDS: usize = 120;
const PAGE: usize = 7;

fn batch(account: &AccountId, case: &str, revision: u64, ids: &[EventId]) -> EventBatch {
    EventBatch::new(
        account.clone(),
        CaseKey::new("streaming", case),
        CommandId::new(),
        CaseRevision(revision),
        ids.iter()
            .map(|id| CommittedEvent {
                event_id: *id,
                event_type: "streaming.happened".to_owned(),
                occurred_at: DateTime::<Utc>::UNIX_EPOCH,
                payload: serde_json::json!({ "id": id.to_string() }),
            })
            .collect(),
    )
}

/// Appends `APPENDS` batches and returns the identifiers in append order.
async fn append_all(
    journal: Arc<dyn EventJournal>,
    account: AccountId,
    case: &'static str,
) -> Vec<EventId> {
    let mut appended = Vec::new();
    for round in 0..APPENDS {
        let ids: Vec<EventId> = (0..BATCH_SIZES[round % BATCH_SIZES.len()])
            .map(|_| EventId::new())
            .collect();
        journal
            .append(batch(&account, case, round as u64 + 1, &ids))
            .await
            .unwrap();
        appended.extend(ids);
        // Give the reader a chance to observe a half-written stream.
        tokio::task::yield_now().await;
    }
    appended
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_paging_by_cursor_sees_every_event_once_while_others_append() {
    let stores = Stores::in_memory();
    let journal = stores.events().clone();
    let account = AccountId::from("streaming-account");
    let noisy_neighbour = AccountId::from("streaming-other-account");

    let writer = tokio::spawn(append_all(journal.clone(), account.clone(), "case-a"));
    // A second tenant writing into the same journal the whole time: its events
    // share the sequence space and must never reach this reader, nor eat into
    // its page limit.
    let neighbour = tokio::spawn(append_all(
        journal.clone(),
        noisy_neighbour.clone(),
        "case-b",
    ));

    let reader = {
        let journal = journal.clone();
        let account = account.clone();
        tokio::spawn(async move {
            let mut cursor = EventCursor::START;
            let mut seen: Vec<EventId> = Vec::new();
            let mut sequences: Vec<u64> = Vec::new();
            let expected = APPENDS / BATCH_SIZES.len() * BATCH_SIZES.iter().sum::<usize>();
            // Bounded so a broken store fails the test instead of hanging.
            for _ in 0..100_000 {
                let page = journal.read_from(&account, cursor, PAGE).await.unwrap();
                assert!(page.events.len() <= PAGE, "a page must honour the limit");
                if page.is_empty() {
                    assert_eq!(
                        page.next_cursor, cursor,
                        "an empty page must leave the cursor where it was"
                    );
                    if seen.len() >= expected {
                        break;
                    }
                    tokio::task::yield_now().await;
                    continue;
                }
                assert!(
                    page.next_cursor > cursor,
                    "a non-empty page must move the cursor forward"
                );
                cursor = page.next_cursor;
                seen.extend(page.events.iter().map(|event| event.event_id));
                sequences.extend(page.events.iter().map(|event| event.sequence));
            }
            (seen, sequences)
        })
    };

    let written = writer.await.unwrap();
    let neighbour_written = neighbour.await.unwrap();
    let (seen, sequences) = reader.await.unwrap();

    assert_eq!(
        seen, written,
        "the reader must see exactly the events of its account, once each, in append order"
    );
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "sequences must strictly increase across pages"
    );
    for id in &neighbour_written {
        assert!(
            !seen.contains(id),
            "another tenant's event leaked into the stream"
        );
    }

    // The neighbour's own consumer sees its own stream, equally intact.
    let mut cursor = EventCursor::START;
    let mut neighbour_seen = Vec::new();
    loop {
        let page = journal
            .read_from(&noisy_neighbour, cursor, PAGE)
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        cursor = page.next_cursor;
        neighbour_seen.extend(page.events.iter().map(|event| event.event_id));
    }
    assert_eq!(neighbour_seen, neighbour_written);
}

#[tokio::test]
async fn a_cursor_survives_being_stored_and_picked_up_later() {
    let stores = Stores::in_memory();
    let journal = stores.events();
    let account = AccountId::from("resuming-account");

    let first: Vec<EventId> = (0..3).map(|_| EventId::new()).collect();
    journal
        .append(batch(&account, "case-a", 1, &first))
        .await
        .unwrap();

    let page = journal
        .read_from(&account, EventCursor::START, 2)
        .await
        .unwrap();
    // What a consumer would persist next to whatever it built from the page.
    let stored: u64 = serde_json::from_str(&serde_json::to_string(&page.next_cursor).unwrap())
        .expect("a cursor is a plain number on the wire");

    let second: Vec<EventId> = (0..2).map(|_| EventId::new()).collect();
    journal
        .append(batch(&account, "case-b", 1, &second))
        .await
        .unwrap();

    let resumed = journal
        .read_from(&account, EventCursor::after(stored), 10)
        .await
        .unwrap();
    let expected: Vec<EventId> = first[2..].iter().chain(second.iter()).copied().collect();
    assert_eq!(
        resumed
            .events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        expected,
        "a stored cursor resumes in the middle of a revision and across cases"
    );
}
