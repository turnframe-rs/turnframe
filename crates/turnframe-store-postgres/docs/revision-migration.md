<!--
This recipe lives in the crate rather than in a guide because it is PostgreSQL
specific and because a test compiles it in: `tests/revision_migration_recipe.rs`
extracts each fenced block by its `-- turnframe-recipe:` marker and executes it
against a live database. The statements a reader copies are therefore the
statements that were proven, and the two cannot drift.
-->

# Adopting over a database that has no revision column

Turnframe requires every mutable case to carry a monotonic revision, and every
command to check that revision in the same statement that writes the row. An
application built on Turnframe from the start gets this for free. An
application being migrated does not, and the transition has a hazard that is
easy to describe and easy to miss.

While the old runtime is still live, both runtimes write the same rows. If an
old write changes a trip and leaves the revision where it was, a card that
was bound to revision N stays valid over a record that has moved. The user then
confirms a rebooking against a preview of a trip that no longer exists, and the
confirmation is honoured, because as far as the check-and-bump statement can
tell nothing happened in between. That is precisely the defect the revision
binding exists to prevent, reintroduced by the migration itself. It never shows
up in the Turnframe test suite, because the Turnframe write path is correct; it
shows up only in the window where the two runtimes overlap, which is exactly
the period nobody wants to be surprised in.

The recipe below closes it. The SQL is dependency-free (no extension, no ORM,
nothing but PostgreSQL), and it is not a transcription: the integration test
`crates/turnframe-store-postgres/tests/revision_migration_recipe.rs` reads
these blocks out of this file and executes them against a live database, so the
statements printed here are the statements that were proven. Adapt the table
and column names; keep the shapes.

### 3.1 The table as the legacy application left it

```sql
-- turnframe-recipe: legacy-table
CREATE TABLE trip (
    id          TEXT   PRIMARY KEY,
    account_id  TEXT   NOT NULL,
    total_cents BIGINT NOT NULL
);
```

Two things are worth noticing before anything is added. There is no revision,
and there is no way to tell from the row whether it changed since a card was
rendered. Everything below exists to make that second question answerable.

### 3.2 Add the column

```sql
-- turnframe-recipe: add-revision
ALTER TABLE trip ADD COLUMN revision BIGINT NOT NULL DEFAULT 1;
ALTER TABLE trip ADD CONSTRAINT trip_revision_positive CHECK (revision > 0);
```

`BIGINT NOT NULL DEFAULT 1` is deliberate. From PostgreSQL 11 onwards a new
column with a constant default is recorded in the catalogue instead of being
written into every row, so this is fast on a large table and needs no separate
backfill statement: every existing row reads as revision 1 immediately. Keep
the default rather than dropping it afterwards, because legacy inserts do not
know the column exists and must still produce a valid row. The check constraint
is there so that a write which computes a revision arithmetically can never
park a row at zero, which is the value `CaseRevision::ZERO` reserves for "this
case does not exist yet".

### 3.3 The statement Turnframe writes

```sql
-- turnframe-recipe: check-and-bump
UPDATE trip
   SET total_cents = $3,
       revision = revision + 1
 WHERE id = $1
   AND account_id = $2
   AND revision = $4;
```

This is the only shape the executor uses. The check and the write are one
statement, so no other transaction can slip between them, and the revision the
card was bound to is the revision the write demands.

Zero rows affected is not a failure to retry. It means the case moved between
the load and the write, and the executor turns it into
`ExecutionError::RevisionConflict`, which the runtime turns into a stale card
rather than into an overwrite. The account predicate is not decoration either:
every lookup is account-scoped, and a stale revision and another tenant's row
have to be indistinguishable from the outside.

### 3.4 Make every legacy write bump it

A legacy write has no card to honour, so it does not gain a revision predicate.
It only has to stop pretending that nothing happened. Each site changes from
this:

```sql
-- turnframe-recipe: legacy-write-forgotten
UPDATE trip
   SET total_cents = $3
 WHERE id = $1
   AND account_id = $2;
```

to this:

```sql
-- turnframe-recipe: legacy-write
UPDATE trip
   SET total_cents = $3,
       revision = revision + 1
 WHERE id = $1
   AND account_id = $2;
```

The common case is that these writes are concentrated. The first adopter's own
instance is sixteen `UPDATE` sites in a single file for one workflow and one
upsert for the other, and at that scale this is a mechanical edit that a
reviewer can check by eye. The upsert needs the same treatment, with the table
name qualifying the column because `EXCLUDED` holds the row that was proposed
rather than the row that is stored:

```sql
-- turnframe-recipe: legacy-upsert
INSERT INTO trip (id, account_id, total_cents, revision)
VALUES ($1, $2, $3, 1)
ON CONFLICT (id) DO UPDATE
   SET total_cents = EXCLUDED.total_cents,
       revision = trip.revision + 1;
```

Do not bump in a second statement. A separate `UPDATE trip SET revision =
revision + 1` after the data write leaves a window in which the row carries new
data and an old revision, and a card validated inside that window is exactly
the bug this whole section is about.

### 3.5 Prove that no write path skips it

Grep finds the sites you already know about. It does not find the report job
that writes through a view, the back-office screen in another service, or the
statement built by an ORM whose text never appears in your repository. The
proof has to come from the database, because the database is the one place
every write must pass through.

```sql
-- turnframe-recipe: audit-and-guard
CREATE TABLE IF NOT EXISTS tf_revision_bump_audit (
    id           BIGSERIAL   PRIMARY KEY,
    table_name   TEXT        NOT NULL,
    row_id       TEXT        NOT NULL,
    old_revision BIGINT      NOT NULL,
    new_revision BIGINT      NOT NULL,
    observed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    statement    TEXT        NOT NULL
);

CREATE OR REPLACE FUNCTION tf_require_revision_bump() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    enforcing BOOLEAN := coalesce(
        current_setting('turnframe.enforce_revision_bump', TRUE), 'off'
    ) IN ('on', 'true', '1');
BEGIN
    IF NEW.revision > OLD.revision THEN
        RETURN NEW;
    END IF;
    IF enforcing THEN
        RAISE EXCEPTION 'write to %.% left revision at %',
            TG_TABLE_SCHEMA, TG_TABLE_NAME, OLD.revision
            USING ERRCODE = 'check_violation';
    END IF;
    INSERT INTO tf_revision_bump_audit
        (table_name, row_id, old_revision, new_revision, statement)
    VALUES
        (TG_TABLE_NAME,
         to_jsonb(OLD) ->> TG_ARGV[0],
         OLD.revision,
         NEW.revision,
         left(current_query(), 500));
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS trip_require_revision_bump ON trip;
CREATE TRIGGER trip_require_revision_bump
    BEFORE UPDATE ON trip
    FOR EACH ROW
    WHEN (OLD.* IS DISTINCT FROM NEW.*)
    EXECUTE FUNCTION tf_require_revision_bump('id');
```

Installed like this the guard changes no behaviour: a write that forgot the
bump still succeeds, and the only difference is that it now leaves its own name
behind. `current_query()` records the statement text, so the audit table tells
you which site to fix rather than that some site exists. The trigger argument
names the primary key column, so the same function serves every table you
migrate.

Two clauses deserve a note. `WHEN (OLD.* IS DISTINCT FROM NEW.*)` means an
update that changed no column never fires the guard, which is right: a write
that moved nothing did not invalidate anything. And a transaction that rolls
back leaves no audit row, which is right for the same reason: a rolled-back
write did not move the record, so no card became unsafe.

Leave the guard in audit mode for a full business cycle, not for an afternoon.
The write paths that skip a revision are, in practice, the ones that run
monthly.

### 3.6 Enforce

```sql
-- turnframe-recipe: enforce
SELECT set_config('turnframe.enforce_revision_bump', 'on', FALSE);
```

Per session is how this is rehearsed and how the test exercises it. In
production, once the audit table has stayed empty for a full cycle, set it once
on the database with `ALTER DATABASE app SET turnframe.enforce_revision_bump =
'on';` and let it apply to every new connection. From then on a write path that
forgets the bump fails with SQLSTATE 23514 instead of quietly retiring the
guarantee. That is the right trade: a rejected legacy write is an incident with
a stack trace, while an accepted one is an incident with a confused user
and no evidence.

### 3.7 When the writes are not concentrated

If the writes are spread over dozens of call sites, generated by an ORM, or
partly inside stored procedures, editing each one is not a plan. Let the
database do the bump:

```sql
-- turnframe-recipe: auto-bump
CREATE OR REPLACE FUNCTION tf_bump_revision() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.revision = OLD.revision THEN
        NEW.revision := OLD.revision + 1;
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS trip_bump_revision ON trip;
CREATE TRIGGER trip_bump_revision
    BEFORE UPDATE ON trip
    FOR EACH ROW
    WHEN (OLD.* IS DISTINCT FROM NEW.*)
    EXECUTE FUNCTION tf_bump_revision();
```

Name it so that it sorts before the guard. Triggers on the same event fire in
name order, so `trip_bump_revision` runs first and
`trip_require_revision_bump` then sees a revision that has already moved and
lets the write through. Reverse the two names and the guard rejects exactly the
writes the auto-bump was about to repair.

Three consequences are worth saying out loud. The auto-bump guarantees the bump
but not the check, so the statement in 3.3 still carries its own `AND revision =
$n`; the trigger leaves an explicit bump alone, which is why the two compose.
It hides the sloppy sites instead of naming them, because with it installed the
audit table stays empty whether or not any write path was ever fixed. And it
bumps on writes with no semantic meaning (a denormalized counter, a
last-seen timestamp), which invalidates more cards than strictly necessary.
That last one is a cost in re-confirmations, not in correctness. Prefer the
audit-then-enforce route while the writes are still few enough to fix by hand,
and use the auto-bump when they are not, knowing that you traded discovery for
safety.

### 3.8 The cards that predate the column

Every existing row now reads as revision 1, a number that meant nothing an hour
ago. Any interaction created before the backfill is bound to a revision drawn
from a different numbering, and some of those will compare equal by accident.
At cutover, invalidate every open interaction on the migrated workflow with
`InvalidationReason::Administrative { code }` rather than letting it resolve.
A card the user has to click again is a small annoyance; a card that matches by
coincidence is the incident this section exists to prevent.
