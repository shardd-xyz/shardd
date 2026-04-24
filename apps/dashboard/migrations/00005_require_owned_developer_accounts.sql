DELETE FROM developer_accounts
WHERE owner_user_id IS NULL;

DROP INDEX IF EXISTS idx_developer_accounts_is_frozen;

ALTER TABLE developer_accounts
    ALTER COLUMN owner_user_id SET NOT NULL,
    DROP COLUMN is_frozen;
