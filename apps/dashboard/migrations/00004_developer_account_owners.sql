ALTER TABLE developer_accounts
ADD COLUMN owner_user_id UUID REFERENCES users(id) ON DELETE CASCADE;

CREATE UNIQUE INDEX uq_developer_accounts_owner_user_id
ON developer_accounts(owner_user_id)
WHERE owner_user_id IS NOT NULL;
