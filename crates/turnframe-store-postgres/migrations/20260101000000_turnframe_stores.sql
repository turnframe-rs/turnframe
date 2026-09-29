-- Turnframe persistence schema (spec §22.2), tf_ prefix.
--
-- Every statement is IF NOT EXISTS, so applying this file to a database that
-- already carries the schema is a no-op. sqlx additionally records the version
-- in `_sqlx_migrations` and skips it on the next run.
--
-- Rollback story: there are no down-migrations. This file only creates objects,
-- it never alters or drops one, so rolling the application back to an earlier
-- release leaves the schema usable as it is. To remove it entirely, drop the
-- dedicated schema (`DROP SCHEMA <schema> CASCADE`) or, when the tables live in
-- a shared schema, drop them in reverse dependency order:
--
--   DROP TABLE IF EXISTS tf_replay, tf_outbox, tf_domain_event,
--                        tf_command_journal, tf_interaction,
--                        tf_turn_phase, tf_turn, tf_conversation;
--   DELETE FROM _sqlx_migrations WHERE version = 20260101000000;
--
-- Every table but tf_outbox is account-scoped and account_id leads every
-- primary key and every index, so a query that forgets the tenant cannot use an
-- index. tf_outbox is the deliberate exception the persistence contract names:
-- it is a system-owned dispatch queue addressed by outbox_id, never by user
-- input, and it carries command_id to link a row back to the account-scoped
-- journal. Every timestamp is timestamptz, every payload is jsonb, and every
-- revision is checked non-negative.

-- ---------------------------------------------------------------------------
-- Conversations, turns and the crash-recovery phase marker (spec §22.3, §23.1)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS tf_conversation (
    account_id      text        NOT NULL,
    conversation_id uuid        NOT NULL,
    created_at      timestamptz NOT NULL,
    metadata        jsonb       NOT NULL DEFAULT 'null'::jsonb,
    PRIMARY KEY (account_id, conversation_id)
);

-- The user turn as received and the assistant turn exactly as returned. Both
-- are whole jsonb documents: a reload deserializes the very value that was
-- persisted, so the ordered response blocks come back identical and no card is
-- ever reconstructed from free text (spec §22.3).
CREATE TABLE IF NOT EXISTS tf_turn (
    account_id      text        NOT NULL,
    turn_id         uuid        NOT NULL,
    conversation_id uuid        NOT NULL,
    received_at     timestamptz NOT NULL,
    user_turn       jsonb       NOT NULL,
    assistant_turn  jsonb,
    PRIMARY KEY (account_id, turn_id),
    CONSTRAINT tf_turn_conversation_fk
        FOREIGN KEY (account_id, conversation_id)
        REFERENCES tf_conversation (account_id, conversation_id)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS tf_turn_by_conversation
    ON tf_turn (account_id, conversation_id, received_at, turn_id);

-- One row per turn, rewritten as the turn advances. It is separate from tf_turn
-- because the turn itself is immutable once written while the marker moves on
-- every step, and because recovery reads only this table.
CREATE TABLE IF NOT EXISTS tf_turn_phase (
    account_id      text        NOT NULL,
    turn_id         uuid        NOT NULL,
    conversation_id uuid        NOT NULL,
    phase           text        NOT NULL,
    updated_at      timestamptz NOT NULL,
    PRIMARY KEY (account_id, turn_id),
    CONSTRAINT tf_turn_phase_turn_fk
        FOREIGN KEY (account_id, turn_id)
        REFERENCES tf_turn (account_id, turn_id)
        ON DELETE CASCADE
);

-- The recovery sweep looks only at turns that have not finished, so the index
-- carries only those rows and shrinks back to nothing as turns are delivered.
CREATE INDEX IF NOT EXISTS tf_turn_phase_unfinished
    ON tf_turn_phase (account_id, turn_id)
    WHERE phase NOT IN ('delivered', 'failed');

-- ---------------------------------------------------------------------------
-- Interactions (spec §15.5, §15.6, §22.2)
-- ---------------------------------------------------------------------------

-- `interaction` holds the whole core record and is never rewritten. The columns
-- beside it are either extracted keys used by an index, or the lifecycle state
-- the store owns; a read deserializes the document and overlays the lifecycle
-- columns, so those columns are the single source of truth for what moves.
CREATE TABLE IF NOT EXISTS tf_interaction (
    account_id           text        NOT NULL,
    interaction_id       uuid        NOT NULL,
    conversation_id      uuid        NOT NULL,
    workflow_key         text        NOT NULL,
    case_id              text        NOT NULL,
    case_revision        bigint      NOT NULL CHECK (case_revision >= 0),
    kind                 text        NOT NULL,
    blocking             boolean     NOT NULL,
    revision_independent boolean     NOT NULL,
    payload_hash         text        NOT NULL,
    interaction          jsonb       NOT NULL,
    status               text        NOT NULL,
    created_at           timestamptz NOT NULL,
    expires_at           timestamptz,
    resolved_at          timestamptz,
    resolved_option_id   text,
    resolved_by_turn     uuid,
    resolution_event_ids uuid[]      NOT NULL DEFAULT '{}',
    failure_code         text,
    invalidation         jsonb,
    PRIMARY KEY (account_id, interaction_id)
);

-- I5: at most one open blocking card per case, enforced by the database rather
-- than by a read-then-write in the adapter. A second concurrent insert loses on
-- the index, not on a race.
CREATE UNIQUE INDEX IF NOT EXISTS tf_one_open_blocking_interaction_per_case
    ON tf_interaction (account_id, workflow_key, case_id)
    WHERE blocking AND status IN ('active', 'resolving');

CREATE INDEX IF NOT EXISTS tf_interaction_open_by_conversation
    ON tf_interaction (account_id, conversation_id, created_at, interaction_id)
    WHERE status IN ('active', 'resolving');

CREATE INDEX IF NOT EXISTS tf_interaction_by_case
    ON tf_interaction (account_id, workflow_key, case_id, created_at, interaction_id);

-- The expiry sweep crosses tenants and must not scan resolved history.
CREATE INDEX IF NOT EXISTS tf_interaction_due
    ON tf_interaction (expires_at)
    WHERE status = 'active' AND expires_at IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Command journal (spec §16.2, §22.2, I14)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS tf_command_journal (
    account_id        text        NOT NULL,
    command_id        uuid        NOT NULL,
    idempotency_key   text        NOT NULL,
    turn_id           uuid        NOT NULL,
    workflow_key      text        NOT NULL,
    case_id           text        NOT NULL,
    expected_revision bigint      NOT NULL CHECK (expected_revision >= 0),
    command_type      text        NOT NULL,
    command_payload   jsonb       NOT NULL,
    origin            jsonb       NOT NULL,
    status            text        NOT NULL,
    result            jsonb,
    created_at        timestamptz NOT NULL,
    completed_at      timestamptz,
    PRIMARY KEY (account_id, command_id),
    -- The rule that stops a retried request from charging a traveler twice.
    CONSTRAINT tf_command_journal_idempotency_key UNIQUE (account_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS tf_command_journal_by_turn
    ON tf_command_journal (account_id, turn_id, created_at, command_id);

-- ---------------------------------------------------------------------------
-- Domain events: the append-only claim ledger (spec §17.1, §22.2)
-- ---------------------------------------------------------------------------

-- `sequence` is the store-assigned position that makes readback order equal
-- append order. It is global rather than per account so that one identity
-- generator orders the whole ledger.
CREATE TABLE IF NOT EXISTS tf_domain_event (
    sequence      bigint      GENERATED ALWAYS AS IDENTITY,
    account_id    text        NOT NULL,
    event_id      uuid        NOT NULL,
    workflow_key  text        NOT NULL,
    case_id       text        NOT NULL,
    case_revision bigint      NOT NULL CHECK (case_revision >= 0),
    command_id    uuid        NOT NULL,
    event_type    text        NOT NULL,
    payload       jsonb       NOT NULL,
    occurred_at   timestamptz NOT NULL,
    PRIMARY KEY (account_id, event_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS tf_domain_event_sequence
    ON tf_domain_event (sequence);

CREATE INDEX IF NOT EXISTS tf_domain_event_by_case
    ON tf_domain_event (account_id, workflow_key, case_id, sequence);

-- ---------------------------------------------------------------------------
-- Outbox (spec §16.4, §22.2)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS tf_outbox (
    outbox_id       uuid        PRIMARY KEY,
    command_id      uuid        NOT NULL,
    destination     text        NOT NULL,
    payload         jsonb       NOT NULL,
    idempotency_key text        NOT NULL,
    status          text        NOT NULL,
    attempt_count   integer     NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at timestamptz,
    created_at      timestamptz NOT NULL,
    completed_at    timestamptz,
    claim_worker_id text,
    claim_taken_at  timestamptz,
    last_failure    text,
    remote_ref      text,
    -- A given external action exists at most once per destination.
    CONSTRAINT tf_outbox_destination_idempotency_key UNIQUE (destination, idempotency_key)
);

-- The claim query reads exactly this index and takes its rows FOR UPDATE
-- SKIP LOCKED, so two dispatchers never see the same row.
CREATE INDEX IF NOT EXISTS tf_outbox_due
    ON tf_outbox (created_at, outbox_id)
    WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS tf_outbox_by_command
    ON tf_outbox (command_id, created_at, outbox_id);

CREATE INDEX IF NOT EXISTS tf_outbox_claims
    ON tf_outbox (claim_taken_at)
    WHERE status = 'dispatching';

-- ---------------------------------------------------------------------------
-- Replay records (spec §23.1, I20)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS tf_replay (
    account_id      text        NOT NULL,
    turn_id         uuid        NOT NULL,
    conversation_id uuid        NOT NULL,
    phase           text        NOT NULL,
    recorded_at     timestamptz NOT NULL,
    record          jsonb       NOT NULL,
    PRIMARY KEY (account_id, turn_id)
);

CREATE INDEX IF NOT EXISTS tf_replay_by_conversation
    ON tf_replay (account_id, conversation_id, recorded_at, turn_id);
