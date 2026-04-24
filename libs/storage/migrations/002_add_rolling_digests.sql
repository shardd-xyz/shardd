-- Rolling prefix digests per §8.3.
-- Per-origin-epoch incremental SHA-256 hash for O(1) convergence verification.
-- Additive-only migration — safe to rollback by dropping this table.

CREATE TABLE IF NOT EXISTS rolling_digests (
    origin_node_id TEXT NOT NULL,
    origin_epoch   INTEGER NOT NULL,
    head           BIGINT NOT NULL,
    digest         BYTEA NOT NULL,
    PRIMARY KEY (origin_node_id, origin_epoch)
);
