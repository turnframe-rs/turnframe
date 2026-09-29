//! Property and concurrency tests for the one rule that must never bend:
//! an idempotency key is admitted exactly once (I14, spec §16.2, §27.2).
//!
//! Everything downstream leans on it. A second `Fresh` for the same key means a
//! command runs twice, which means an trip is sent twice; there is no later
//! layer that can undo that. So the rule is tested two ways: over random
//! interleavings of admissions of the same keys, and under real contention from
//! many tasks racing on one key at once.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use proptest::prelude::*;
use turnframe_core::case::CaseRef;
use turnframe_core::command::{CommandOrigin, IdempotencyKey};
use turnframe_core::ids::{AccountId, CaseRevision, CommandId, EventId, TurnId};
use turnframe_store::journal::{
    CommandJournalEntry, CommandJournalReader, CommandJournalStatus, CommandJournalWriter,
    JournalAdmission, JournalOutcome,
};
use turnframe_store::memory::MemoryStores;

fn entry(
    account: &AccountId,
    turn: TurnId,
    command_id: CommandId,
    key: &str,
    created_at: DateTime<Utc>,
) -> CommandJournalEntry {
    CommandJournalEntry {
        command_id,
        account_id: account.clone(),
        idempotency_key: IdempotencyKey::new(key),
        turn_id: turn,
        case_ref: CaseRef::new("proptest", "case-1", CaseRevision(1)),
        command_type: "proptest.noop".to_owned(),
        command_payload: serde_json::json!({ "key": key }),
        origin: CommandOrigin::InternalPolicy {
            policy_key: "proptest".to_owned(),
        },
        status: CommandJournalStatus::Pending,
        result: None,
        created_at,
        completed_at: None,
    }
}

/// One step of a generated interleaving.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Admit a command under key `key`.
    Begin { key: u8 },
    /// Settle the command admitted first under key `key`, if there is one.
    Complete { key: u8 },
    /// Move the command admitted first under key `key` to `Executing`.
    Execute { key: u8 },
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    let step = prop_oneof![
        (0_u8..4).prop_map(|key| Step::Begin { key }),
        (0_u8..4).prop_map(|key| Step::Complete { key }),
        (0_u8..4).prop_map(|key| Step::Execute { key }),
    ];
    prop::collection::vec(step, 1..40)
}

proptest! {
    /// However the calls interleave, every key is admitted exactly once and
    /// every later call replays that first admission.
    #[test]
    fn a_key_is_admitted_once_under_any_interleaving(steps in steps()) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async move {
            let store = MemoryStores::new();
            let account = AccountId::from("proptest-account");
            let turn = TurnId::new();
            let start = DateTime::<Utc>::UNIX_EPOCH;

            // key -> the command id of its one and only Fresh admission.
            let mut admitted: BTreeMap<u8, CommandId> = BTreeMap::new();
            let mut fresh_count: BTreeMap<u8, usize> = BTreeMap::new();

            for (tick, step) in steps.iter().enumerate() {
                let now = start + TimeDelta::milliseconds(tick as i64);
                match *step {
                    Step::Begin { key } => {
                        let command_id = CommandId::new();
                        let admission = store
                            .begin(entry(&account, turn, command_id, &format!("key-{key}"), now))
                            .await
                            .expect("begin never fails on a healthy store");
                        match admission {
                            JournalAdmission::Fresh => {
                                *fresh_count.entry(key).or_default() += 1;
                                admitted.entry(key).or_insert(command_id);
                            }
                            JournalAdmission::Replay(replayed) => {
                                let original = admitted.get(&key).copied();
                                prop_assert_eq!(
                                    Some(replayed.command_id),
                                    original,
                                    "a replay must return the originally admitted command"
                                );
                            }
                        }
                    }
                    Step::Execute { key } => {
                        if let Some(command_id) = admitted.get(&key) {
                            // May legitimately conflict once the entry is settled.
                            let _ = store.mark_executing(&account, command_id).await;
                        }
                    }
                    Step::Complete { key } => {
                        if let Some(command_id) = admitted.get(&key) {
                            let _ = store
                                .complete(
                                    &account,
                                    command_id,
                                    JournalOutcome::Committed {
                                        new_revision: CaseRevision(2),
                                        event_ids: vec![EventId::nil()],
                                    },
                                )
                                .await;
                        }
                    }
                }
            }

            for (key, count) in fresh_count {
                prop_assert_eq!(count, 1, "key-{} was admitted Fresh {} times", key, count);
            }
            for (key, command_id) in admitted {
                let entry = store
                    .get(&account, &command_id)
                    .await
                    .expect("the admitted entry is readable");
                prop_assert_eq!(
                    entry.idempotency_key,
                    IdempotencyKey::new(format!("key-{key}")),
                    "the persisted entry must keep its key"
                );
            }
            Ok(())
        })?;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_admissions_of_one_key_produce_exactly_one_fresh() {
    let store = Arc::new(MemoryStores::new());
    let account = AccountId::from("race-account");
    let turn = TurnId::new();
    let now = DateTime::<Utc>::UNIX_EPOCH;

    let mut tasks = Vec::new();
    for _ in 0..32 {
        let store = store.clone();
        let account = account.clone();
        tasks.push(tokio::spawn(async move {
            store
                .begin(entry(&account, turn, CommandId::new(), "contended", now))
                .await
        }));
    }

    let mut fresh = 0_usize;
    let mut replayed_ids = Vec::new();
    for task in tasks {
        match task.await.expect("the task must not panic") {
            Ok(JournalAdmission::Fresh) => fresh += 1,
            Ok(JournalAdmission::Replay(entry)) => replayed_ids.push(entry.command_id),
            Err(error) => panic!("begin must not fail on a healthy store: {error:?}"),
        }
    }

    assert_eq!(fresh, 1, "exactly one caller may be admitted");
    assert_eq!(replayed_ids.len(), 31, "every other caller must replay");
    assert!(
        replayed_ids.windows(2).all(|pair| pair[0] == pair[1]),
        "every replay must name the same command"
    );
}
