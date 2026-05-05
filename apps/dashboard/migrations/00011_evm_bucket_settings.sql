ALTER TABLE developer_buckets ADD COLUMN evm_enabled BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE developer_buckets ADD COLUMN evm_paused BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE developer_buckets ADD COLUMN evm_whitelist_enabled BOOLEAN NOT NULL DEFAULT false;

CREATE TABLE bucket_evm_whitelist (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bucket_id UUID NOT NULL REFERENCES developer_buckets(id) ON DELETE CASCADE,
    address TEXT NOT NULL,
    added_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (bucket_id, address)
);

CREATE INDEX idx_bucket_evm_whitelist_bucket
    ON bucket_evm_whitelist(bucket_id);
