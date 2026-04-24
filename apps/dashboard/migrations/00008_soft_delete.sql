-- Soft-delete semantics for users + archive flag for buckets.
--
-- Buckets can no longer be hard-deleted: the "Archive" action just sets
-- archived_at. Event history on the mesh is untouched (archiving is
-- reversible and does not create gaps in per-node seq space).
--
-- Users likewise are soft-deleted: their row remains so mesh event history
-- (which references user_id via the internal bucket naming convention)
-- stays resolvable, and all their API keys are revoked at the same time.

ALTER TABLE developer_buckets
    ADD COLUMN archived_at TIMESTAMPTZ;

CREATE INDEX idx_developer_buckets_active
    ON developer_buckets(user_id)
    WHERE archived_at IS NULL;

ALTER TABLE users
    ADD COLUMN deleted_at TIMESTAMPTZ;
