//! The migration recipe of the adoption guide, executed.
//!
//! `docs/revision-migration.md` tells an adopter how to put a
//! `revision` column on tables that never had one, and names the hazard that
//! makes the exercise worth doing: while the old runtime is still writing the
//! same rows, a legacy write that leaves the revision alone lets a card bound
//! to revision N stay valid over a record that has moved. That is the defect
//! the revision binding exists to prevent, reintroduced by the migration.
//!
//! These tests do not re-type that SQL. They read the fenced `sql` blocks out
//! of the guide by their `-- turnframe-recipe: <step>` marker and run them
//! against a live PostgreSQL, so the statements a reader copies are the
//! statements that were proven. Editing the guide's SQL badly fails this file;
//! deleting a block from the guide fails it too.
//!
//! Like every other test in this crate they run for real when
//! `TURNFRAME_TEST_DATABASE_URL` names a database and skip with a printed note
//! when it does not. The one test that needs no database — that the guide still
//! carries every step of the recipe — always runs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod support;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The recipe, compiled in so that it cannot drift from the test that proves
/// it: the statements a reader copies are the statements executed below.
const GUIDE: &str = include_str!("../docs/revision-migration.md");

/// Every step the guide is required to carry, in the order it presents them.
const STEPS: &[&str] = &[
    "legacy-table",
    "add-revision",
    "check-and-bump",
    "legacy-write-forgotten",
    "legacy-write",
    "legacy-upsert",
    "audit-and-guard",
    "enforce",
    "auto-bump",
];

/// The SQLSTATE PostgreSQL reports for a `check_violation`, which is what the
/// guard raises once enforcement is on.
const CHECK_VIOLATION: &str = "23514";

/// The tenant every fixture row belongs to.
const ACCOUNT: &str = "acct-migration-recipe";

/// Returns the one fenced `sql` block of the guide whose first extra is the
/// marker for `step`.
///
/// Deliberately strict: zero blocks means the guide lost the step, and two
/// mean a reader would not know which one is normative.
fn recipe(step: &str) -> String {
    let marker = format!("-- turnframe-recipe: {step}");
    let blocks: Vec<&str> = GUIDE
        .split("```sql\n")
        .skip(1)
        .filter_map(|rest| rest.split("\n```").next())
        .filter(|block| block.lines().next().unwrap_or_default().trim_end() == marker)
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "docs/revision-migration.md must carry exactly one `{step}` block, found {}",
        blocks.len()
    );
    blocks[0].to_owned()
}

/// Opens a pool whose `search_path` is a schema of this test's own, creating
/// the schema first because a `search_path` naming a schema that does not exist
/// resolves to nothing.
async fn pool_on(url: &str, schema: &str) -> PgPool {
    let options: PgConnectOptions = url.parse().expect("the test database URL parses");
    let bootstrap = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .expect("the test database accepts a connection");
    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
        .execute(&bootstrap)
        .await
        .expect("the test schema can be created");
    bootstrap.close().await;
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.options([("search_path", schema)]))
        .await
        .expect("the test database accepts a connection on the test schema")
}

/// Empties the schema so a run starts where the last one did. Dropping the
/// table drops its triggers with it; the functions are replaced rather than
/// dropped, which is what `CREATE OR REPLACE` is for.
async fn reset(pool: &PgPool) {
    sqlx::raw_sql(
        "DROP TABLE IF EXISTS trip CASCADE; \
         DROP TABLE IF EXISTS tf_revision_bump_audit CASCADE;",
    )
    .execute(pool)
    .await
    .expect("the test schema can be emptied");
}

/// Inserts one trip at revision 1, the state the backfill leaves every
/// existing row in.
async fn seed(conn: &mut sqlx::PgConnection, id: &str, total: i64) {
    sqlx::query("INSERT INTO trip (id, account_id, total_cents, revision) VALUES ($1, $2, $3, 1)")
        .bind(id)
        .bind(ACCOUNT)
        .bind(total)
        .execute(&mut *conn)
        .await
        .expect("the fixture row is inserted");
}

/// Reads back what the row says about itself now.
async fn row(conn: &mut sqlx::PgConnection, id: &str) -> (i64, i64) {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT total_cents, revision FROM trip WHERE id = $1 AND account_id = $2",
    )
    .bind(id)
    .bind(ACCOUNT)
    .fetch_one(&mut *conn)
    .await
    .expect("the fixture row is readable")
}

/// How many rows the audit trigger has recorded for `id`.
async fn audited(conn: &mut sqlx::PgConnection, id: &str) -> Vec<(i64, i64, String)> {
    sqlx::query_as::<_, (i64, i64, String)>(
        "SELECT old_revision, new_revision, statement FROM tf_revision_bump_audit \
         WHERE table_name = 'trip' AND row_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .expect("the audit table is readable")
}

/// The SQLSTATE of a failed statement, or the empty string when the failure was
/// not the database's.
fn sqlstate(error: &sqlx::Error) -> String {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(|code| code.into_owned())
        .unwrap_or_default()
}

/// The guide still carries every step, and the two statements that carry the
/// whole argument still say what they have to say.
///
/// This one needs no database: a run with `TURNFRAME_TEST_DATABASE_URL` unset
/// still proves that the recipe has not been quietly hollowed out.
#[test]
fn the_guide_carries_every_step_of_the_recipe() {
    for step in STEPS {
        let block = recipe(step);
        assert!(
            block.lines().count() > 1,
            "the `{step}` block of the guide is only its marker"
        );
    }

    let check_and_bump = recipe("check-and-bump");
    assert!(
        check_and_bump.contains("revision = revision + 1"),
        "the check-and-bump statement must move the revision"
    );
    assert!(
        check_and_bump.contains("AND revision = $4"),
        "the check-and-bump statement must check the revision it was bound to"
    );
    assert!(
        check_and_bump.contains("AND account_id = $2"),
        "the check-and-bump statement must stay account-scoped"
    );

    let forgotten = recipe("legacy-write-forgotten");
    assert!(
        !forgotten.contains("revision"),
        "the `before` example must be the write that forgets the revision"
    );
    assert!(
        recipe("legacy-write").contains("revision = revision + 1"),
        "the `after` example must be the same write with the bump"
    );
    assert!(
        recipe("legacy-upsert").contains("revision = trip.revision + 1"),
        "the upsert must qualify the column, because EXCLUDED holds the proposed row"
    );
}

/// The hazard, then the detection, then the enforcement, on one table.
///
/// The order is the order an adopter lives it: first the legacy write that
/// forgets the bump and leaves a card looking fresh over a record that moved,
/// then the same write under the audit trigger, which changes nothing except
/// that the offending site now names itself, then the same write under
/// enforcement, which fails instead of succeeding quietly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_legacy_update_that_forgets_the_bump_is_detectable() {
    let Some(url) = support::database_url() else {
        support::skipped("a_legacy_update_that_forgets_the_bump_is_detectable");
        return;
    };
    let pool = pool_on(&url, "tf_test_revision_recipe").await;
    reset(&pool).await;
    // One connection for the whole test: the enforcement switch is a session
    // setting, and a pool would hand the next statement to a connection that
    // never saw it.
    let mut conn = pool.acquire().await.expect("a connection is available");

    let legacy_table = recipe("legacy-table");
    let add_revision = recipe("add-revision");
    sqlx::raw_sql(&legacy_table)
        .execute(&mut *conn)
        .await
        .expect("the legacy table is created");
    sqlx::raw_sql(&add_revision)
        .execute(&mut *conn)
        .await
        .expect("the revision column is added");

    let check_and_bump = recipe("check-and-bump");
    let forgotten = recipe("legacy-write-forgotten");
    let fixed = recipe("legacy-write");

    // 1. Unguarded, the hazard is real. A legacy write changes the row and
    //    leaves the revision where it was, so the card bound to revision 1 —
    //    which was rendered over the total the user actually saw — still
    //    executes over a record that has since moved.
    seed(&mut conn, "trip-unguarded", 1_000).await;
    sqlx::query(&forgotten)
        .bind("trip-unguarded")
        .bind(ACCOUNT)
        .bind(7_000_i64)
        .execute(&mut *conn)
        .await
        .expect("the legacy write succeeds, which is the problem");
    assert_eq!(
        row(&mut conn, "trip-unguarded").await,
        (7_000, 1),
        "the legacy write moved the record and left the revision behind"
    );
    let stale_card_still_executes = sqlx::query(&check_and_bump)
        .bind("trip-unguarded")
        .bind(ACCOUNT)
        .bind(1_200_i64)
        .bind(1_i64)
        .execute(&mut *conn)
        .await
        .expect("the check-and-bump statement runs")
        .rows_affected();
    assert_eq!(
        stale_card_still_executes, 1,
        "without the guard a card bound to revision 1 writes over a record that moved"
    );

    // 2. Install the guard in audit mode. Behaviour is unchanged; the only
    //    difference is that the offending statement now leaves its name behind.
    let audit_and_guard = recipe("audit-and-guard");
    sqlx::raw_sql(&audit_and_guard)
        .execute(&mut *conn)
        .await
        .expect("the audit table, the guard function and the trigger are installed");

    seed(&mut conn, "trip-audited", 1_000).await;
    sqlx::query(&forgotten)
        .bind("trip-audited")
        .bind(ACCOUNT)
        .bind(7_000_i64)
        .execute(&mut *conn)
        .await
        .expect("in audit mode the legacy write still succeeds");
    assert_eq!(
        row(&mut conn, "trip-audited").await,
        (7_000, 1),
        "audit mode observes, it does not correct"
    );
    let recorded = audited(&mut conn, "trip-audited").await;
    assert_eq!(
        recorded.len(),
        1,
        "the skipped bump was recorded exactly once"
    );
    assert_eq!(
        (recorded[0].0, recorded[0].1),
        (1, 1),
        "the audit row says the revision did not move"
    );
    assert!(
        recorded[0].2.contains("UPDATE trip"),
        "the audit row names the offending statement, not just its existence: {}",
        recorded[0].2
    );

    // A write that changes nothing does not fire the guard, because it did not
    // move the record and cannot have invalidated a card.
    sqlx::query(&forgotten)
        .bind("trip-audited")
        .bind(ACCOUNT)
        .bind(7_000_i64)
        .execute(&mut *conn)
        .await
        .expect("a write that changes nothing succeeds");
    assert_eq!(
        audited(&mut conn, "trip-audited").await.len(),
        1,
        "a write that changed no column is not an offence"
    );

    // 3. Switch enforcement on. The same statement now fails loudly.
    let enforce = recipe("enforce");
    sqlx::raw_sql(&enforce)
        .execute(&mut *conn)
        .await
        .expect("enforcement is switched on for this session");

    seed(&mut conn, "trip-enforced", 1_000).await;
    let refused = sqlx::query(&forgotten)
        .bind("trip-enforced")
        .bind(ACCOUNT)
        .bind(7_000_i64)
        .execute(&mut *conn)
        .await
        .expect_err("under enforcement the write that forgets the bump is refused");
    assert_eq!(
        sqlstate(&refused),
        CHECK_VIOLATION,
        "the refusal is a check violation, not a connection accident: {refused}"
    );
    assert_eq!(
        row(&mut conn, "trip-enforced").await,
        (1_000, 1),
        "the refused write changed nothing"
    );

    // 4. The same site, fixed. It passes enforcement, and the card bound to the
    //    revision it moved past can no longer execute — which is the whole
    //    point of the exercise.
    seed(&mut conn, "trip-fixed", 1_000).await;
    sqlx::query(&fixed)
        .bind("trip-fixed")
        .bind(ACCOUNT)
        .bind(7_000_i64)
        .execute(&mut *conn)
        .await
        .expect("the fixed legacy write passes enforcement");
    assert_eq!(
        row(&mut conn, "trip-fixed").await,
        (7_000, 2),
        "the fixed legacy write moved the revision with the data"
    );
    let stale_card = sqlx::query(&check_and_bump)
        .bind("trip-fixed")
        .bind(ACCOUNT)
        .bind(1_200_i64)
        .bind(1_i64)
        .execute(&mut *conn)
        .await
        .expect("the check-and-bump statement runs")
        .rows_affected();
    assert_eq!(
        stale_card, 0,
        "the card bound to revision 1 no longer matches, so it is stale instead of destructive"
    );

    // 5. The upsert of the guide bumps on its update branch, under enforcement.
    let upsert = recipe("legacy-upsert");
    for total in [1_000_i64, 7_000_i64] {
        sqlx::query(&upsert)
            .bind("trip-upserted")
            .bind(ACCOUNT)
            .bind(total)
            .execute(&mut *conn)
            .await
            .expect("the upsert passes enforcement on both branches");
    }
    assert_eq!(
        row(&mut conn, "trip-upserted").await,
        (7_000, 2),
        "the update branch of the upsert bumped the revision"
    );
}

/// The statement Turnframe writes refuses a revision that has moved on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_and_bump_refuses_a_stale_revision() {
    let Some(url) = support::database_url() else {
        support::skipped("check_and_bump_refuses_a_stale_revision");
        return;
    };
    let pool = pool_on(&url, "tf_test_revision_stale").await;
    reset(&pool).await;
    let mut conn = pool.acquire().await.expect("a connection is available");

    let legacy_table = recipe("legacy-table");
    let add_revision = recipe("add-revision");
    sqlx::raw_sql(&legacy_table)
        .execute(&mut *conn)
        .await
        .expect("the legacy table is created");
    sqlx::raw_sql(&add_revision)
        .execute(&mut *conn)
        .await
        .expect("the revision column is added");
    seed(&mut conn, "trip-1", 1_000).await;

    let check_and_bump = recipe("check-and-bump");
    let first = sqlx::query(&check_and_bump)
        .bind("trip-1")
        .bind(ACCOUNT)
        .bind(2_000_i64)
        .bind(1_i64)
        .execute(&mut *conn)
        .await
        .expect("the first write runs")
        .rows_affected();
    assert_eq!(
        first, 1,
        "the write at the bound revision is the one that wins"
    );
    assert_eq!(row(&mut conn, "trip-1").await, (2_000, 2));

    let second = sqlx::query(&check_and_bump)
        .bind("trip-1")
        .bind(ACCOUNT)
        .bind(9_999_i64)
        .bind(1_i64)
        .execute(&mut *conn)
        .await
        .expect("the second write runs")
        .rows_affected();
    assert_eq!(
        second, 0,
        "a second card bound to the same revision matches nothing"
    );
    assert_eq!(
        row(&mut conn, "trip-1").await,
        (2_000, 2),
        "zero rows affected means the loser wrote nothing at all"
    );

    // Another tenant's row is not reachable at any revision, so a stale card
    // and a card for someone else's case fail the same way.
    let other_tenant = sqlx::query(&check_and_bump)
        .bind("trip-1")
        .bind("acct-somebody-else")
        .bind(9_999_i64)
        .bind(2_i64)
        .execute(&mut *conn)
        .await
        .expect("the cross-tenant write runs")
        .rows_affected();
    assert_eq!(other_tenant, 0, "the account predicate is load-bearing");
}

/// The variant for an application whose writes are not concentrated: the
/// database does the bump, and the two triggers compose in name order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn when_writes_are_scattered_the_trigger_does_the_bump() {
    let Some(url) = support::database_url() else {
        support::skipped("when_writes_are_scattered_the_trigger_does_the_bump");
        return;
    };
    let pool = pool_on(&url, "tf_test_revision_autobump").await;
    reset(&pool).await;
    let mut conn = pool.acquire().await.expect("a connection is available");

    for step in [
        "legacy-table",
        "add-revision",
        "audit-and-guard",
        "auto-bump",
    ] {
        let sql = recipe(step);
        sqlx::raw_sql(&sql)
            .execute(&mut *conn)
            .await
            .unwrap_or_else(|error| panic!("the `{step}` block applies: {error}"));
    }
    let enforce = recipe("enforce");
    sqlx::raw_sql(&enforce)
        .execute(&mut *conn)
        .await
        .expect("enforcement is switched on for this session");

    // The write that forgets the bump now passes under enforcement, because the
    // auto-bump trigger sorts before the guard and has already moved the
    // revision by the time the guard looks.
    seed(&mut conn, "trip-scattered", 1_000).await;
    let forgotten = recipe("legacy-write-forgotten");
    sqlx::query(&forgotten)
        .bind("trip-scattered")
        .bind(ACCOUNT)
        .bind(7_000_i64)
        .execute(&mut *conn)
        .await
        .expect("with the auto-bump installed the forgetful write is repaired, not refused");
    assert_eq!(
        row(&mut conn, "trip-scattered").await,
        (7_000, 2),
        "the trigger supplied the bump the statement omitted"
    );

    // The price of that repair: with the auto-bump in place nothing is ever
    // recorded, so the sloppy call sites stay anonymous.
    assert!(
        audited(&mut conn, "trip-scattered").await.is_empty(),
        "the auto-bump trades discovery for safety, and this is where that shows"
    );

    // A statement that bumps explicitly is left alone rather than bumped twice,
    // which is what lets the two arrangements coexist during a migration.
    let fixed = recipe("legacy-write");
    sqlx::query(&fixed)
        .bind("trip-scattered")
        .bind(ACCOUNT)
        .bind(8_000_i64)
        .execute(&mut *conn)
        .await
        .expect("an explicit bump passes");
    assert_eq!(
        row(&mut conn, "trip-scattered").await,
        (8_000, 3),
        "the trigger left the explicit bump alone instead of adding a second one"
    );
}
