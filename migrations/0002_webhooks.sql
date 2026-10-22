-- Webhook subscriptions. The delivery worker fans events out from the
-- `events` outbox into `webhook_deliveries`, then POSTs them with retries.

CREATE TABLE webhooks (
    id          TEXT PRIMARY KEY NOT NULL,
    url         TEXT NOT NULL,
    description TEXT,
    -- JSON array of event type prefixes (e.g. ["deal.", "contact.created"]); empty = all
    event_types TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(event_types) AND json_type(event_types) = 'array'),
    -- `whsec_...` signing secret (Standard Webhooks); kept in plaintext because signing needs it
    secret      TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    -- last events.seq fanned out to this webhook; new webhooks start at the current head
    cursor_seq  INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE webhook_deliveries (
    id               TEXT PRIMARY KEY NOT NULL,
    webhook_id       TEXT NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
    event_seq        INTEGER NOT NULL REFERENCES events (seq),
    status           TEXT NOT NULL CHECK (status IN ('pending', 'succeeded', 'failed')),
    attempts         INTEGER NOT NULL DEFAULT 0,
    next_attempt_at  TEXT,
    last_status_code INTEGER,
    last_error       TEXT,
    delivered_at     TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    UNIQUE (webhook_id, event_seq)
);
CREATE INDEX webhook_deliveries_due ON webhook_deliveries (next_attempt_at) WHERE status = 'pending';
CREATE INDEX webhook_deliveries_webhook ON webhook_deliveries (webhook_id, id);
