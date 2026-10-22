-- Core CRM schema. IDs are prefixed ULIDs (e.g. `con_01jc...`), so ordering by
-- id is ordering by creation time. Timestamps are RFC 3339 text in UTC.
-- `custom` holds user-defined fields as a JSON object.

CREATE TABLE companies (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    domain      TEXT,
    custom      TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(custom) AND json_type(custom) = 'object'),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX companies_domain_key ON companies (lower(domain)) WHERE domain IS NOT NULL;

CREATE TABLE contacts (
    id          TEXT PRIMARY KEY NOT NULL,
    first_name  TEXT,
    last_name   TEXT,
    email       TEXT,
    phone       TEXT,
    title       TEXT,
    company_id  TEXT REFERENCES companies (id) ON DELETE SET NULL,
    custom      TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(custom) AND json_type(custom) = 'object'),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX contacts_email_key ON contacts (lower(email)) WHERE email IS NOT NULL;
CREATE INDEX contacts_company_id ON contacts (company_id);

CREATE TABLE pipelines (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    position    INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE stages (
    id          TEXT PRIMARY KEY NOT NULL,
    pipeline_id TEXT NOT NULL REFERENCES pipelines (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    position    INTEGER NOT NULL,
    -- open: deal in progress; won / lost: terminal, sets deals.closed_at
    kind        TEXT NOT NULL CHECK (kind IN ('open', 'won', 'lost')),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX stages_pipeline_id ON stages (pipeline_id, position);

CREATE TABLE deals (
    id                  TEXT PRIMARY KEY NOT NULL,
    title               TEXT NOT NULL,
    -- minor currency units (cents) to avoid floating point
    amount              INTEGER,
    currency            TEXT,
    stage_id            TEXT NOT NULL REFERENCES stages (id) ON DELETE RESTRICT,
    company_id          TEXT REFERENCES companies (id) ON DELETE SET NULL,
    contact_id          TEXT REFERENCES contacts (id) ON DELETE SET NULL,
    expected_close_date TEXT,
    closed_at           TEXT,
    custom              TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(custom) AND json_type(custom) = 'object'),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
CREATE INDEX deals_stage_id ON deals (stage_id);
CREATE INDEX deals_company_id ON deals (company_id);
CREATE INDEX deals_contact_id ON deals (contact_id);

-- Activities outlive the records they reference (links are set to NULL on
-- delete) so a deal's history survives deleting one of its contacts.
CREATE TABLE activities (
    id           TEXT PRIMARY KEY NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('note', 'call', 'email', 'meeting', 'task')),
    subject      TEXT,
    body         TEXT,
    contact_id   TEXT REFERENCES contacts (id) ON DELETE SET NULL,
    company_id   TEXT REFERENCES companies (id) ON DELETE SET NULL,
    deal_id      TEXT REFERENCES deals (id) ON DELETE SET NULL,
    occurred_at  TEXT NOT NULL,
    due_at       TEXT,
    completed_at TEXT,
    custom       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(custom) AND json_type(custom) = 'object'),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX activities_contact_id ON activities (contact_id);
CREATE INDEX activities_company_id ON activities (company_id);
CREATE INDEX activities_deal_id ON activities (deal_id);

CREATE TABLE api_keys (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL,
    -- first characters of the token, shown in listings to identify a key
    prefix       TEXT NOT NULL,
    -- sha256 of the full token; the token itself is never stored
    token_hash   TEXT NOT NULL UNIQUE,
    scope        TEXT NOT NULL CHECK (scope IN ('read', 'write')),
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at   TEXT
);

-- Transactional outbox: every mutation appends an event in the same
-- transaction. Consumers poll /v1/events; webhook delivery reads from here too.
CREATE TABLE events (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    type        TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    data        TEXT NOT NULL CHECK (json_valid(data)),
    occurred_at TEXT NOT NULL
);

-- Default sales pipeline so a fresh install is usable immediately.
INSERT INTO pipelines (id, name, position, created_at, updated_at)
VALUES ('pip_sales', 'Sales', 0, strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'), strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'));

INSERT INTO stages (id, pipeline_id, name, position, kind, created_at, updated_at)
SELECT column1, 'pip_sales', column2, column3, column4,
       strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'), strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')
FROM (VALUES
    ('stg_lead', 'Lead', 0, 'open'),
    ('stg_qualified', 'Qualified', 1, 'open'),
    ('stg_proposal', 'Proposal', 2, 'open'),
    ('stg_negotiation', 'Negotiation', 3, 'open'),
    ('stg_won', 'Won', 4, 'won'),
    ('stg_lost', 'Lost', 5, 'lost')
);
