//! Shared plumbing for the tests that need a live PostgreSQL.
//!
//! Every test here is written to do one of two things: run for real when
//! `TURNFRAME_TEST_DATABASE_URL` names a database, and skip with a printed note
//! when it does not, so `cargo test` passes on a machine with no PostgreSQL and
//! proves something on a machine with one.
//!
//! Isolation has two layers. A schema per subject keeps the conformance run, the
//! account-scoped race tests and the outbox sweep out of each other's way.
//! Inside the shared schema, every test writes as an account of its own and
//! reads only its own tenant, so they run in parallel — which is a standing
//! check that the account scoping this adapter promises actually holds.
//!
//! Every test opens its own pool rather than sharing one. `#[tokio::test]`
//! builds a runtime per test and drops it when the test ends; a `sqlx` pool
//! outliving the runtime it was created on stops being able to hand out
//! connections, which surfaces as an acquire timeout in whichever test is still
//! running. One pool per test, created inside that test's runtime, is the only
//! arrangement that does not depend on which test finishes first.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use turnframe_core::case::CaseRef;
use turnframe_core::command::{CommandOrigin, IdempotencyKey};
use turnframe_core::event::{CommittedEvent, OutboxEntry, OutboxStatus};
use turnframe_core::ids::{
    AccountId, CaseRevision, CommandId, ConversationId, EventId, InteractionId, OutboxId, TurnId,
    UserId,
};
use turnframe_core::interaction::{
    Interaction, InteractionKind, InteractionOption, InteractionPayload, InteractionSpec,
    StoredInteractionAction,
};
use turnframe_core::locale::Locale;
use turnframe_core::turn::{ActorContext, TurnInput};
use turnframe_store::conversation::StoredUserTurn;
use turnframe_store::events::EventBatch;
use turnframe_store::journal::{CommandJournalEntry, CommandJournalStatus};
use turnframe_store_postgres::{PgStoreConfig, PgStores};
use uuid::Uuid;

/// The variable the continuous integration workflow sets, and the one a
/// developer sets to run these tests locally.
pub const DATABASE_URL_VARIABLE: &str = "TURNFRAME_TEST_DATABASE_URL";

/// Every table this schema owns, in the order a truncation names them.
const TABLES: &str = "tf_conversation, tf_turn, tf_turn_phase, tf_interaction, \
     tf_command_journal, tf_domain_event, tf_outbox, tf_replay";

/// Loads the repository's `.env` once per test binary, before any test reads a
/// variable or opens a connection. A variable already set in the shell wins.
fn load_dotenv() {
    static LOADED: std::sync::Once = std::sync::Once::new();
    LOADED.call_once(|| {
        let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    });
}

/// The database to test against, when there is one.
pub fn database_url() -> Option<String> {
    load_dotenv();
    std::env::var(DATABASE_URL_VARIABLE)
        .ok()
        .filter(|url| !url.trim().is_empty())
}

/// Says out loud that a test did not run, so a green suite is never mistaken
/// for a suite that proved something.
pub fn skipped(test: &str) {
    println!("SKIPPED {test}: {DATABASE_URL_VARIABLE} is not set, so no database was available");
}

/// Opens a migrated store in a schema this test is the only user of, and empties
/// it so the run starts where the last one did.
///
/// The schema name is fixed rather than random, so repeated runs reuse one
/// namespace instead of leaving a new one behind every time.
pub async fn sole_owner_of(url: &str, schema: &str) -> PgStores {
    let store = migrated(url, schema).await;
    truncate_all(&store).await;
    store
}

/// Opens a migrated store in a schema shared with the other tests of this
/// binary, which are kept apart by writing as different accounts.
///
/// It does not empty the schema: another test is very likely using it right now.
pub async fn co_tenant_of(url: &str, schema: &str) -> PgStores {
    migrated(url, schema).await
}

/// Opens a store and applies the migrations, which is safe to do from several
/// tests at once.
async fn migrated(url: &str, schema: &str) -> PgStores {
    let store = open(url, schema).await;
    store.migrate().await.expect("migrations apply");
    store
}

/// Opens a second, independent pool on a schema another store already migrated.
///
/// Race tests need two connections that know nothing about each other; this is
/// how they get one each.
pub async fn second_pool(url: &str, schema: &str) -> PgStores {
    open(url, schema).await
}

/// Opens a pool with the small bounds a test needs.
async fn open(url: &str, schema: &str) -> PgStores {
    let config = PgStoreConfig::new()
        .max_connections(8)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(30))
        .schema(schema)
        .expect("the schema name is a plain identifier");
    PgStores::connect_with(url, &config)
        .await
        .expect("the test database accepts a connection")
}

/// Empties every table, so a run starts from the same place as the last one.
pub async fn truncate_all(store: &PgStores) {
    let statement = format!("TRUNCATE {TABLES} RESTART IDENTITY CASCADE");
    sqlx::query(&statement)
        .execute(store.pool())
        .await
        .expect("the test schema can be emptied");
}

/// A tenant no other test writes as.
pub fn unique_account(test: &str) -> AccountId {
    AccountId::from(format!("{test}-{}", Uuid::new_v4().simple()))
}

/// The instant fixtures start from.
pub fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

/// `epoch()` plus whole seconds, so ordering assertions are not coin flips.
pub fn at(seconds: i64) -> DateTime<Utc> {
    epoch() + TimeDelta::seconds(seconds)
}

/// A case of the test's own, at `revision`.
pub fn case(case_id: &str, revision: u64) -> CaseRef {
    CaseRef::new("turnframe-postgres-test", case_id, CaseRevision(revision))
}

/// A blocking, revision-bound card with one dismissable option.
pub fn card(account: &AccountId, case_ref: CaseRef, created_at: DateTime<Utc>) -> Interaction {
    let payload = InteractionPayload::new("a card").with_option(InteractionOption::new(
        "ack",
        "Got it",
        StoredInteractionAction::Dismiss,
    ));
    let spec = InteractionSpec::new("card", case_ref, InteractionKind::SingleSelect, payload);
    Interaction::from_spec(
        spec,
        InteractionId::new(),
        account.clone(),
        ConversationId::new(),
        TurnId::new(),
        created_at,
    )
    .expect("the fixture card is answerable")
}

/// A user turn carrying plain text.
pub fn user_turn(
    account: &AccountId,
    conversation: ConversationId,
    turn: TurnId,
    received_at: DateTime<Utc>,
) -> StoredUserTurn {
    StoredUserTurn::new(
        TurnInput {
            turn_id: turn,
            conversation_id: conversation,
            actor: ActorContext::new(account.clone(), UserId::from("test-user")),
            text: Some("a turn".to_owned()),
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: Locale::from("en"),
            effort: None,
        },
        received_at,
    )
}

/// A `Pending` journal entry.
pub fn journal_entry(
    account: &AccountId,
    turn: TurnId,
    command_id: CommandId,
    key: &str,
) -> CommandJournalEntry {
    CommandJournalEntry {
        command_id,
        account_id: account.clone(),
        idempotency_key: IdempotencyKey::new(key),
        turn_id: turn,
        case_ref: case("case-1", 1),
        command_type: "turnframe.postgres.test".to_owned(),
        command_payload: serde_json::json!({ "key": key }),
        origin: CommandOrigin::InternalPolicy {
            policy_key: "test".to_owned(),
        },
        status: CommandJournalStatus::Pending,
        result: None,
        created_at: epoch(),
        completed_at: None,
    }
}

/// A batch of one event on `case_id`.
pub fn event_batch(
    account: &AccountId,
    case_id: &str,
    command_id: CommandId,
    revision: u64,
    ids: &[EventId],
) -> EventBatch {
    EventBatch::new(
        account.clone(),
        case(case_id, 0).key(),
        command_id,
        CaseRevision(revision),
        ids.iter()
            .map(|id| CommittedEvent {
                event_id: *id,
                event_type: "turnframe.postgres.test.happened".to_owned(),
                occurred_at: epoch(),
                payload: serde_json::json!({}),
            })
            .collect(),
    )
}

/// A `Pending` outbox row for `destination`.
pub fn outbox_entry(
    outbox_id: OutboxId,
    command_id: CommandId,
    destination: &str,
    key: &str,
    created_at: DateTime<Utc>,
) -> OutboxEntry {
    OutboxEntry {
        outbox_id,
        command_id,
        destination: destination.to_owned(),
        payload: serde_json::json!({ "key": key }),
        idempotency_key: IdempotencyKey::new(key),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        next_attempt_at: None,
        created_at,
        completed_at: None,
    }
}
