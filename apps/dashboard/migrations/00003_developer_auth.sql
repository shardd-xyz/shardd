CREATE TABLE developer_accounts (
    id UUID PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    is_frozen BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_developer_accounts_public_id ON developer_accounts(public_id);
CREATE INDEX idx_developer_accounts_is_frozen ON developer_accounts(is_frozen) WHERE is_frozen = TRUE;

CREATE OR REPLACE FUNCTION set_developer_accounts_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_developer_accounts_set_updated_at
BEFORE UPDATE ON developer_accounts
FOR EACH ROW
EXECUTE FUNCTION set_developer_accounts_updated_at();

CREATE TABLE developer_api_keys (
    id UUID PRIMARY KEY,
    developer_account_id UUID NOT NULL REFERENCES developer_accounts(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    key_prefix TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ
);

CREATE INDEX idx_developer_api_keys_account_id ON developer_api_keys(developer_account_id);
CREATE INDEX idx_developer_api_keys_created_at ON developer_api_keys(created_at DESC);
CREATE INDEX idx_developer_api_keys_active ON developer_api_keys(developer_account_id) WHERE revoked_at IS NULL;

CREATE OR REPLACE FUNCTION set_developer_api_keys_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_developer_api_keys_set_updated_at
BEFORE UPDATE ON developer_api_keys
FOR EACH ROW
EXECUTE FUNCTION set_developer_api_keys_updated_at();

CREATE TABLE developer_api_key_scopes (
    id UUID PRIMARY KEY,
    api_key_id UUID NOT NULL REFERENCES developer_api_keys(id) ON DELETE CASCADE,
    resource_type TEXT NOT NULL,
    match_type TEXT NOT NULL,
    resource_value TEXT,
    can_read BOOLEAN NOT NULL DEFAULT FALSE,
    can_write BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_developer_api_key_scopes_key_id ON developer_api_key_scopes(api_key_id);

CREATE TABLE developer_auth_audit_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    admin_id UUID REFERENCES users(id) ON DELETE SET NULL,
    admin_email TEXT NOT NULL,
    action TEXT NOT NULL,
    developer_account_id UUID REFERENCES developer_accounts(id) ON DELETE SET NULL,
    api_key_id UUID REFERENCES developer_api_keys(id) ON DELETE SET NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_developer_auth_audit_created_at ON developer_auth_audit_log(created_at DESC);
CREATE INDEX idx_developer_auth_audit_account_id ON developer_auth_audit_log(developer_account_id);
CREATE INDEX idx_developer_auth_audit_api_key_id ON developer_auth_audit_log(api_key_id);
