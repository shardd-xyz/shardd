-- shardd v2 schema per protocol.md v1.7 §6.1

-- Events: append-only event log (§2.1)
CREATE TABLE events (
    event_id                TEXT PRIMARY KEY,
    origin_node_id          TEXT NOT NULL,
    origin_epoch            INTEGER NOT NULL DEFAULT 1,
    origin_seq              BIGINT NOT NULL,
    created_at_unix_ms      BIGINT NOT NULL,
    type                    TEXT NOT NULL DEFAULT 'standard',
    bucket                  TEXT NOT NULL,
    account                 TEXT NOT NULL,
    amount                  BIGINT NOT NULL,
    note                    TEXT,
    idempotency_nonce       TEXT,
    void_ref                TEXT,
    hold_amount             BIGINT NOT NULL DEFAULT 0,
    hold_expires_at_unix_ms BIGINT NOT NULL DEFAULT 0,
    inserted_at             TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Dedup key and durable replay-safe presence check (§3.2, §6.2)
CREATE UNIQUE INDEX idx_events_dedup
    ON events (origin_node_id, origin_epoch, origin_seq);

-- Balance aggregation (§5.2)
CREATE INDEX idx_events_bucket_account
    ON events (bucket, account);

-- Time-ordered queries (§7.1 GET /events)
CREATE INDEX idx_events_created_at
    ON events (created_at_unix_ms);

-- Correction and hold-release lookups (§10.5, §11.3)
CREATE INDEX idx_events_void_ref
    ON events (void_ref) WHERE void_ref IS NOT NULL;

-- Idempotency conflict detection (§10.4)
CREATE INDEX idx_events_idempotency
    ON events (idempotency_nonce, bucket, account, amount)
    WHERE idempotency_nonce IS NOT NULL;

-- Node identity (§6.1, §13.1)
CREATE TABLE node_meta (
    node_id         TEXT PRIMARY KEY,
    host            TEXT NOT NULL DEFAULT '127.0.0.1',
    port            INTEGER NOT NULL DEFAULT 0,
    current_epoch   INTEGER NOT NULL DEFAULT 1,
    next_seq        BIGINT NOT NULL DEFAULT 1
);

-- Permanent node registry (§14.1) — rows are NEVER deleted
CREATE TABLE node_registry (
    node_id                 TEXT PRIMARY KEY,
    addr                    TEXT NOT NULL,
    first_seen_at_unix_ms   BIGINT NOT NULL,
    last_seen_at_unix_ms    BIGINT NOT NULL,
    status                  TEXT NOT NULL DEFAULT 'active'
);

-- Balance summary materialized view (§6.1, OPTIONAL optimization)
-- Used for fast balance bootstrap on startup
CREATE MATERIALIZED VIEW balance_summary AS
SELECT bucket, account, SUM(amount)::BIGINT AS balance
FROM events
GROUP BY bucket, account;

CREATE UNIQUE INDEX idx_balance_summary_pk
    ON balance_summary (bucket, account);
