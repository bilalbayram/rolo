-- Full-text search over contacts, companies, deals and activities.
--
-- Source tables have TEXT primary keys, and their implicit rowids may change on
-- VACUUM, so FTS5 cannot index them directly. Instead, triggers maintain
-- `search_documents` (a stable INTEGER PRIMARY KEY plus the text to index),
-- and `search_index` is an external-content FTS5 table over it.

CREATE TABLE search_documents (
    rowid       INTEGER PRIMARY KEY,
    entity_type TEXT NOT NULL CHECK (entity_type IN ('contact', 'company', 'deal', 'activity')),
    entity_id   TEXT NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL
);

CREATE VIRTUAL TABLE search_index USING fts5 (
    title,
    body,
    content = 'search_documents',
    content_rowid = 'rowid',
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

CREATE TRIGGER search_documents_ai AFTER INSERT ON search_documents BEGIN
    INSERT INTO search_index (rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
CREATE TRIGGER search_documents_ad AFTER DELETE ON search_documents BEGIN
    INSERT INTO search_index (search_index, rowid, title, body)
    VALUES ('delete', old.rowid, old.title, old.body);
END;
CREATE TRIGGER search_documents_au AFTER UPDATE ON search_documents BEGIN
    INSERT INTO search_index (search_index, rowid, title, body)
    VALUES ('delete', old.rowid, old.title, old.body);
    INSERT INTO search_index (rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;

-- Custom field values are indexed too, so `{"linkedin": "adalovelace"}` is findable.

CREATE TRIGGER contacts_search_ai AFTER INSERT ON contacts BEGIN
    INSERT INTO search_documents (entity_type, entity_id, title, body) VALUES (
        'contact', new.id,
        coalesce(nullif(concat_ws(' ', new.first_name, new.last_name), ''), new.email, ''),
        concat_ws(' ', new.email, new.phone, new.title,
                  (SELECT group_concat(value, ' ') FROM json_each(new.custom)))
    );
END;
CREATE TRIGGER contacts_search_au AFTER UPDATE ON contacts BEGIN
    UPDATE search_documents SET
        title = coalesce(nullif(concat_ws(' ', new.first_name, new.last_name), ''), new.email, ''),
        body = concat_ws(' ', new.email, new.phone, new.title,
                         (SELECT group_concat(value, ' ') FROM json_each(new.custom)))
    WHERE entity_id = new.id;
END;
CREATE TRIGGER contacts_search_ad AFTER DELETE ON contacts BEGIN
    DELETE FROM search_documents WHERE entity_id = old.id;
END;

CREATE TRIGGER companies_search_ai AFTER INSERT ON companies BEGIN
    INSERT INTO search_documents (entity_type, entity_id, title, body) VALUES (
        'company', new.id, new.name,
        concat_ws(' ', new.domain, (SELECT group_concat(value, ' ') FROM json_each(new.custom)))
    );
END;
CREATE TRIGGER companies_search_au AFTER UPDATE ON companies BEGIN
    UPDATE search_documents SET
        title = new.name,
        body = concat_ws(' ', new.domain, (SELECT group_concat(value, ' ') FROM json_each(new.custom)))
    WHERE entity_id = new.id;
END;
CREATE TRIGGER companies_search_ad AFTER DELETE ON companies BEGIN
    DELETE FROM search_documents WHERE entity_id = old.id;
END;

CREATE TRIGGER deals_search_ai AFTER INSERT ON deals BEGIN
    INSERT INTO search_documents (entity_type, entity_id, title, body) VALUES (
        'deal', new.id, new.title,
        coalesce((SELECT group_concat(value, ' ') FROM json_each(new.custom)), '')
    );
END;
CREATE TRIGGER deals_search_au AFTER UPDATE ON deals BEGIN
    UPDATE search_documents SET
        title = new.title,
        body = coalesce((SELECT group_concat(value, ' ') FROM json_each(new.custom)), '')
    WHERE entity_id = new.id;
END;
CREATE TRIGGER deals_search_ad AFTER DELETE ON deals BEGIN
    DELETE FROM search_documents WHERE entity_id = old.id;
END;

CREATE TRIGGER activities_search_ai AFTER INSERT ON activities BEGIN
    INSERT INTO search_documents (entity_type, entity_id, title, body) VALUES (
        'activity', new.id, coalesce(new.subject, new.kind),
        concat_ws(' ', new.body, (SELECT group_concat(value, ' ') FROM json_each(new.custom)))
    );
END;
CREATE TRIGGER activities_search_au AFTER UPDATE ON activities BEGIN
    UPDATE search_documents SET
        title = coalesce(new.subject, new.kind),
        body = concat_ws(' ', new.body, (SELECT group_concat(value, ' ') FROM json_each(new.custom)))
    WHERE entity_id = new.id;
END;
CREATE TRIGGER activities_search_ad AFTER DELETE ON activities BEGIN
    DELETE FROM search_documents WHERE entity_id = old.id;
END;

-- Index existing records (the triggers above populate search_index).
INSERT INTO search_documents (entity_type, entity_id, title, body)
SELECT 'contact', id,
       coalesce(nullif(concat_ws(' ', first_name, last_name), ''), email, ''),
       concat_ws(' ', email, phone, title, (SELECT group_concat(value, ' ') FROM json_each(custom)))
FROM contacts;
INSERT INTO search_documents (entity_type, entity_id, title, body)
SELECT 'company', id, name,
       concat_ws(' ', domain, (SELECT group_concat(value, ' ') FROM json_each(custom)))
FROM companies;
INSERT INTO search_documents (entity_type, entity_id, title, body)
SELECT 'deal', id, title, coalesce((SELECT group_concat(value, ' ') FROM json_each(custom)), '')
FROM deals;
INSERT INTO search_documents (entity_type, entity_id, title, body)
SELECT 'activity', id, coalesce(subject, kind),
       concat_ws(' ', body, (SELECT group_concat(value, ' ') FROM json_each(custom)))
FROM activities;
