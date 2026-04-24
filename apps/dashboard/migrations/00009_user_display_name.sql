-- Adds an optional display_name for user profiles so devs can show
-- something friendlier than their email in UIs down the line. Nullable on
-- purpose: existing users stay as-is, the profile page gets a new field.
ALTER TABLE users ADD COLUMN IF NOT EXISTS display_name TEXT;
