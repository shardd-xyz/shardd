-- shardd v3 — event identity is now per-`(bucket, origin_node_id, origin_epoch, origin_seq)`.
--
-- This migration is destructive by design: the existing dev/test rows in
-- `events`, `rolling_digests`, and `balance_summary` are wiped because the
-- old (origin, epoch, seq) identity is no longer unique in the new
-- dedup-key space. Billing credits that lived in `__billing__<user>`
-- buckets must be re-seeded after nodes are brought back up (see
-- `scripts/reseed_credits.sh`).
--
-- Changes:
-- 1. `events.idx_events_dedup` now keys on (bucket, …) with bucket first.
-- 2. `rolling_digests` gains `bucket` in its primary key.
-- 3. New `bucket_seq_allocator` table tracks per-(bucket, node) epoch and
--    next_seq; replaces the old `node_meta.current_epoch`/`next_seq`.
-- 4. `node_meta` loses those columns — node identity is still persisted
--    there (node_id/host/port), but seq/epoch is per-bucket now.

DROP MATERIALIZED VIEW IF EXISTS balance_summary;
DROP TABLE IF EXISTS rolling_digests;
DROP TABLE IF EXISTS events;

CREATE TABLE events (
    event_id                TEXT PRIMARY KEY,
    bucket                  TEXT NOT NULL,
    origin_node_id          TEXT NOT NULL,
    origin_epoch            INTEGER NOT NULL,
    origin_seq              BIGINT NOT NULL,
    created_at_unix_ms      BIGINT NOT NULL,
    type                    TEXT NOT NULL DEFAULT 'standard',
    account                 TEXT NOT NULL,
    amount                  BIGINT NOT NULL,
    note                    TEXT,
    idempotency_nonce       TEXT,
    void_ref                TEXT,
    hold_amount             BIGINT NOT NULL DEFAULT 0,
    hold_expires_at_unix_ms BIGINT NOT NULL DEFAULT 0,
    inserted_at             TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE events ADD CONSTRAINT chk_events_note_length
    CHECK (note IS NULL OR char_length(note) <= 4096) NOT VALID;

-- Dedup key: (bucket, origin_node_id, origin_epoch, origin_seq) must be
-- globally unique. bucket-first for clustering events of the same bucket
-- physically close; helps range scans during per-bucket catch-up.
CREATE UNIQUE INDEX idx_events_dedup
    ON events (bucket, origin_node_id, origin_epoch, origin_seq);

-- Balance aggregation per (bucket, account).
CREATE INDEX idx_events_bucket_account
    ON events (bucket, account);

-- Time-ordered queries (§7.1 GET /events).
CREATE INDEX idx_events_created_at
    ON events (created_at_unix_ms);

-- Correction / hold-release lookups (§10.5, §11.3).
CREATE INDEX idx_events_void_ref
    ON events (void_ref) WHERE void_ref IS NOT NULL;

-- Idempotency conflict detection (§10.4) — unchanged key.
CREATE INDEX idx_events_idempotency
    ON events (idempotency_nonce, bucket, account, amount)
    WHERE idempotency_nonce IS NOT NULL;

-- Per-(bucket, node_id) epoch + seq allocator.
--
-- Rows are created lazily: a bucket only gets a row here the first time
-- the owning node writes to it. On node startup, every row matching the
-- local node_id is flagged `needs_bump = TRUE`; the next write to that
-- bucket atomically bumps the epoch and clears the flag.
--
-- current_epoch increases monotonically across restarts, but only for
-- buckets that the node actually writes to after a restart — empty
-- buckets don't accumulate empty epochs.
CREATE TABLE bucket_seq_allocator (
    bucket         TEXT NOT NULL,
    node_id        TEXT NOT NULL,
    current_epoch  INTEGER NOT NULL,
    next_seq       BIGINT NOT NULL,
    needs_bump     BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (bucket, node_id)
);

-- Rolling prefix digests per (bucket, origin, epoch) (§8.3).
CREATE TABLE rolling_digests (
    bucket         TEXT NOT NULL,
    origin_node_id TEXT NOT NULL,
    origin_epoch   INTEGER NOT NULL,
    head           BIGINT NOT NULL,
    digest         BYTEA NOT NULL,
    PRIMARY KEY (bucket, origin_node_id, origin_epoch)
);

-- node_meta loses the columns that are now per-bucket.
ALTER TABLE node_meta DROP COLUMN IF EXISTS current_epoch;
ALTER TABLE node_meta DROP COLUMN IF EXISTS next_seq;

-- Balance bootstrap view (§6.1, optional optimization).
CREATE MATERIALIZED VIEW balance_summary AS
SELECT bucket, account, SUM(amount)::BIGINT AS balance
FROM events
GROUP BY bucket, account;

CREATE UNIQUE INDEX idx_balance_summary_pk
    ON balance_summary (bucket, account);
