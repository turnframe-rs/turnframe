-- Erasing a payload from the append-only claim ledger (spec §17.1, ADR-012).
--
-- The ledger may not lose an event: a receipt cites committed events, and a
-- consumer pages the journal by a sequence that must never skip. But a payload
-- carrying personal data has to be erasable on request, so the event stays and
-- the payload is emptied in place. These two columns are the record of that
-- act, and they live on the redacted row itself rather than in a log beside it:
-- one statement then performs the erasure and writes its own audit trail, and a
-- redacted payload with no record of who removed it is not a state this schema
-- can hold.
--
-- `redaction_authority` names an erasure ticket, a retention policy key or an
-- operator, never the data that was removed. Nothing here records what was
-- erased, which is the point.
--
-- Both columns are nullable because an event that was never redacted has no
-- record, and the check constraint keeps them from disagreeing.
--
-- Rollback story: as in the first migration, there is no down-migration. To
-- remove these columns from a database that carries them:
--
--   ALTER TABLE tf_domain_event
--     DROP COLUMN IF EXISTS redacted_at,
--     DROP COLUMN IF EXISTS redaction_authority;
--   DELETE FROM _sqlx_migrations WHERE version = 20260102000000;
--
-- Dropping them does not bring an erased payload back.

ALTER TABLE tf_domain_event
    ADD COLUMN IF NOT EXISTS redacted_at          timestamptz,
    ADD COLUMN IF NOT EXISTS redaction_authority  text;

-- Either the event has a whole erasure record or it has none: a row that
-- carried one half would leave an operator unable to say when, or under what
-- authority, a payload disappeared.
DO $$
BEGIN
    ALTER TABLE tf_domain_event
        ADD CONSTRAINT tf_domain_event_redaction_complete
        CHECK ((redacted_at IS NULL) = (redaction_authority IS NULL));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END;
$$;
