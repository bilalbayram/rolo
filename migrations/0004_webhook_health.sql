-- Secret rotation: after rotating, deliveries are signed with both the new and
-- the previous secret until `previous_secret_expires_at`.
ALTER TABLE webhooks ADD COLUMN previous_secret TEXT;
ALTER TABLE webhooks ADD COLUMN previous_secret_expires_at TEXT;

-- Endpoint health, reset by any successful delivery. A webhook is disabled
-- automatically once enough deliveries have exhausted their retries and it has
-- been failing for long enough (see delivery::DisablePolicy).
ALTER TABLE webhooks ADD COLUMN failing_since TEXT;
ALTER TABLE webhooks ADD COLUMN consecutive_failed_deliveries INTEGER NOT NULL DEFAULT 0;
ALTER TABLE webhooks ADD COLUMN disabled_reason TEXT;
