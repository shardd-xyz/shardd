ALTER TABLE developer_buckets ADD COLUMN evm_enabled BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE developer_buckets ADD COLUMN evm_paused BOOLEAN NOT NULL DEFAULT false;
