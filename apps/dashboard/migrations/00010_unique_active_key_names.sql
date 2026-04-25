-- Active developer API keys must have a unique name per user. The
-- dashboard's keys list otherwise renders two indistinguishable rows
-- when, e.g., `shardd auth login` mints a fresh key with the same
-- `cli/<hostname>` name as an older one (or the user accidentally
-- types the same name twice in the create-key wizard).
--
-- Revoked rows are excluded from the constraint so a user can issue
-- a new "production-worker" key after revoking an old one with the
-- same name. The active set is what matters in the UI and for auth.
--
-- Existing collisions (one user with multiple active keys sharing
-- a name) are pre-renamed by appending the first 8 chars of the
-- key's UUID. The original is preserved untouched so devs can
-- recognise their keys; only the older copies grow the suffix.

WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (
               PARTITION BY user_id, name
               ORDER BY created_at ASC, id ASC
           ) AS rn
    FROM developer_api_keys
    WHERE revoked_at IS NULL
)
UPDATE developer_api_keys k
SET name = k.name || ' #' || substring(k.id::text from 1 for 8)
FROM ranked r
WHERE k.id = r.id AND r.rn > 1;

CREATE UNIQUE INDEX IF NOT EXISTS developer_api_keys_user_active_name_unique
    ON developer_api_keys (user_id, name)
    WHERE revoked_at IS NULL;
