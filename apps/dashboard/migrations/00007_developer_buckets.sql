-- Explicit bucket registry. Until this migration, buckets existed only as
-- string columns on event rows in the mesh; a developer could implicitly
-- create one by writing an event. After this migration, the dashboard owns
-- bucket lifecycle and the mesh rejects writes to buckets that aren't here.

CREATE TABLE developer_buckets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (user_id, name)
);

CREATE INDEX idx_developer_buckets_user ON developer_buckets(user_id);
