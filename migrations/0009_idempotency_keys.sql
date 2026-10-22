-- Responses to REST POSTs sent with an `Idempotency-Key` header, scoped per API
-- key and kept for 24 hours, so a retry replays the stored response instead of
-- repeating the write. Expired rows are deleted lazily.
CREATE TABLE idempotency_keys (
    api_key_id   TEXT NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    key          TEXT NOT NULL,
    -- Method, path and query, e.g. `POST /v1/contacts`.
    request      TEXT NOT NULL,
    -- Hex SHA-256 of the request body.
    body_hash    TEXT NOT NULL,
    -- NULL while the first request with this key is still running.
    status       INTEGER,
    content_type TEXT,
    body         BLOB,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (api_key_id, key)
);
CREATE INDEX idempotency_keys_created_at ON idempotency_keys (created_at);
