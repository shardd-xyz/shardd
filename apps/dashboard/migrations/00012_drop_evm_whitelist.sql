-- Drop whitelist table and column — whitelist enforcement is deferred
-- to a future mesh-based mechanism. This migration cleans up the schema
-- without modifying the already-applied 00011.
DROP TABLE IF EXISTS bucket_evm_whitelist;
ALTER TABLE developer_buckets DROP COLUMN IF EXISTS evm_whitelist_enabled;
