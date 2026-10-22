-- Users own records. API keys may belong to a user: records created with that
-- key default to being owned by them, and deactivating the user disables the key.

CREATE TABLE users (
    id             TEXT PRIMARY KEY NOT NULL,
    name           TEXT NOT NULL,
    email          TEXT NOT NULL,
    deactivated_at TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);
CREATE UNIQUE INDEX users_email_key ON users (lower(email));

ALTER TABLE api_keys ADD COLUMN user_id TEXT REFERENCES users (id) ON DELETE SET NULL;

ALTER TABLE contacts ADD COLUMN owner_id TEXT REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE companies ADD COLUMN owner_id TEXT REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE deals ADD COLUMN owner_id TEXT REFERENCES users (id) ON DELETE SET NULL;
-- For tasks, the assignee.
ALTER TABLE activities ADD COLUMN owner_id TEXT REFERENCES users (id) ON DELETE SET NULL;

CREATE INDEX contacts_owner_id ON contacts (owner_id);
CREATE INDEX companies_owner_id ON companies (owner_id);
CREATE INDEX deals_owner_id ON deals (owner_id);
CREATE INDEX activities_owner_id ON activities (owner_id);
