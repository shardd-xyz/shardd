ALTER TABLE billing_plans ADD COLUMN IF NOT EXISTS stripe_annual_price_id TEXT;
ALTER TABLE billing_plans ADD COLUMN IF NOT EXISTS annual_price_cents INTEGER NOT NULL DEFAULT 0;
