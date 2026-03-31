CREATE TABLE events (
    event_id           TEXT PRIMARY KEY,
    origin_node_id     TEXT NOT NULL,
    origin_seq         BIGINT NOT NULL,
    created_at_unix_ms BIGINT NOT NULL,
    bucket             TEXT NOT NULL,
    account            TEXT NOT NULL,
    amount             BIGINT NOT NULL,
    note               TEXT,
    inserted_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_events_origin_seq
    ON events (origin_node_id, origin_seq);
CREATE INDEX idx_events_bucket_account
    ON events (bucket, account);
CREATE INDEX idx_events_created_at
    ON events (created_at_unix_ms);

CREATE TABLE node_meta (
    node_id   TEXT PRIMARY KEY,
    host      TEXT NOT NULL DEFAULT '127.0.0.1',
    port      INTEGER NOT NULL DEFAULT 0,
    next_seq  BIGINT NOT NULL DEFAULT 1
);

CREATE TABLE peers (
    addr       TEXT PRIMARY KEY,
    added_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
