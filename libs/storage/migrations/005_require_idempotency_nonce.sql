-- Make idempotency_nonce mandatory on every event.
--
-- Motivation: up to now the column was nullable and callers could omit
-- it, which silently disabled dedupe on retries. We now treat retry
-- safety as an invariant of the system: every event has a nonce, either
-- one picked deliberately by the caller (Stripe invoice id, bucket
-- name, admin audit id) or a UUID generated at the write boundary.
--
-- Existing rows predate this invariant. For rows where nonce is NULL we
-- fill in a random UUID so the NOT NULL constraint can be satisfied
-- without data loss. The backfilled values are distinct and harmless:
-- nothing dedupes against them.

UPDATE events
    SET idempotency_nonce = gen_random_uuid()::text
    WHERE idempotency_nonce IS NULL;

ALTER TABLE events
    ALTER COLUMN idempotency_nonce SET NOT NULL;

-- The partial index on nonce is now redundantly partial — every row
-- matches. Replace with a full index on the same columns for the same
-- idempotency lookup path.
DROP INDEX IF EXISTS idx_events_idempotency;
CREATE INDEX idx_events_idempotency
    ON events (idempotency_nonce, bucket, account, amount);
