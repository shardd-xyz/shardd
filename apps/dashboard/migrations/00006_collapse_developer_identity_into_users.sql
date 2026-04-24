ALTER TABLE developer_api_keys
    ADD COLUMN user_id UUID;

UPDATE developer_api_keys AS api_keys
SET user_id = accounts.owner_user_id
FROM developer_accounts AS accounts
WHERE accounts.id = api_keys.developer_account_id;

ALTER TABLE developer_api_keys
    ALTER COLUMN user_id SET NOT NULL,
    ADD CONSTRAINT developer_api_keys_user_id_fkey
        FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

CREATE INDEX idx_developer_api_keys_user_id ON developer_api_keys(user_id);
CREATE INDEX idx_developer_api_keys_active_user ON developer_api_keys(user_id) WHERE revoked_at IS NULL;

ALTER TABLE developer_auth_audit_log
    ADD COLUMN target_user_id UUID REFERENCES users(id) ON DELETE SET NULL;

UPDATE developer_auth_audit_log AS audit
SET target_user_id = accounts.owner_user_id
FROM developer_accounts AS accounts
WHERE accounts.id = audit.developer_account_id;

UPDATE developer_auth_audit_log
SET metadata = metadata - 'developer_public_id'
WHERE metadata ? 'developer_public_id';

CREATE INDEX idx_developer_auth_audit_target_user_id ON developer_auth_audit_log(target_user_id);

DROP INDEX IF EXISTS idx_developer_api_keys_account_id;
DROP INDEX IF EXISTS idx_developer_api_keys_active;
DROP INDEX IF EXISTS idx_developer_auth_audit_account_id;

ALTER TABLE developer_api_keys
    DROP CONSTRAINT IF EXISTS developer_api_keys_developer_account_id_fkey,
    DROP COLUMN developer_account_id;

ALTER TABLE developer_auth_audit_log
    DROP CONSTRAINT IF EXISTS developer_auth_audit_log_developer_account_id_fkey,
    DROP COLUMN developer_account_id;

DROP TABLE developer_accounts;
DROP FUNCTION IF EXISTS set_developer_accounts_updated_at();
