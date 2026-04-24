CREATE TABLE billing_notifications (
    user_id UUID NOT NULL,
    threshold TEXT NOT NULL,  -- '20pct', '10pct', '5pct', 'zero'
    sent_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, threshold)
);
