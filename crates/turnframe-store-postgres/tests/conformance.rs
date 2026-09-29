//! The store conformance suite, run against a live PostgreSQL.
//!
//! This is the central proof of the crate. `turnframe-store` states the
//! persistence contract as executable checks that talk to a set of stores
//! through the public traits only; if they pass here, the runtime's guarantees
//! hold on this adapter, and no amount of reading the SQL below would say more.
//!
//! The suite asks for an **empty** set of stores once per check. This binary
//! owns a schema of its own and empties it between checks, so a check never
//! sees the writes of the one before it — which matters more here than for the
//! in-memory store, because two of the checks sweep across every tenant
//! (interaction expiry and the recovery listing) and would otherwise see rows a
//! neighbouring check left behind.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod support;

use turnframe_store::conformance;
use turnframe_store::stores::Stores;

/// The schema this binary owns.
const SCHEMA: &str = "tf_test_conformance";

/// How many checks the suite carried when this test was written. The assertion
/// is a lower bound, so the suite may grow without touching this file, but a
/// suite that silently emptied out cannot pass as "everything green".
const CHECKS_AT_LEAST: usize = 19;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_whole_store_conformance_suite_passes_against_postgres() {
    let Some(url) = support::database_url() else {
        support::skipped("the_whole_store_conformance_suite_passes_against_postgres");
        return;
    };
    let store = support::sole_owner_of(&url, SCHEMA).await;

    // `run_all` builds the stores synchronously, once per check, so the reset
    // has to bridge back into the runtime. `block_in_place` is what makes that
    // legal on a multi-threaded runtime.
    let factory = {
        let store = store.clone();
        move || -> Stores {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(support::truncate_all(&store));
            });
            store.stores().expect("every role is supplied")
        }
    };

    let report = conformance::run_all(&factory).await;
    println!("{report}");
    assert!(report.passed(), "{report}");
    assert!(
        report.outcomes.len() >= CHECKS_AT_LEAST,
        "the suite ran {} checks, expected at least {CHECKS_AT_LEAST}",
        report.outcomes.len()
    );
}

#[tokio::test]
async fn migrations_are_idempotent() {
    let Some(url) = support::database_url() else {
        support::skipped("migrations_are_idempotent");
        return;
    };
    // The schema this binary already migrated: applying the same migrations to
    // it again must be a no-op rather than an error, because every deployment
    // runs `migrate()` on every start-up.
    let store = support::second_pool(&url, SCHEMA).await;
    store.migrate().await.expect("re-applying migrations");
    store.migrate().await.expect("re-applying migrations twice");
}
