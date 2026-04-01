-- Materialized view for fast balance bootstrap on node startup.
-- Refreshed periodically by BatchWriter after flush cycles.

CREATE MATERIALIZED VIEW IF NOT EXISTS balance_summary AS
SELECT bucket, account, SUM(amount)::bigint AS balance
FROM events
GROUP BY bucket, account;

CREATE UNIQUE INDEX IF NOT EXISTS balance_summary_bucket_account
ON balance_summary (bucket, account);
