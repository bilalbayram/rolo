-- Fold Turkish dotless ı (U+0131) and dotted İ (U+0130) to i in the search
-- index. The unicode61 tokenizer lowercases I to i but leaves ı alone, so
-- "isik" didn't find "Işık". service::search::match_expression folds queries
-- the same way. search_documents keeps the original text for result titles and
-- snippets; folding keeps every token in place, so snippets still highlight the
-- right words.
DROP TRIGGER search_documents_ai;
DROP TRIGGER search_documents_ad;
DROP TRIGGER search_documents_au;

CREATE TRIGGER search_documents_ai AFTER INSERT ON search_documents BEGIN
    INSERT INTO search_index (rowid, title, body) VALUES (
        new.rowid,
        replace(replace(new.title, 'ı', 'i'), 'İ', 'i'),
        replace(replace(new.body, 'ı', 'i'), 'İ', 'i'));
END;
CREATE TRIGGER search_documents_ad AFTER DELETE ON search_documents BEGIN
    INSERT INTO search_index (search_index, rowid, title, body) VALUES (
        'delete', old.rowid,
        replace(replace(old.title, 'ı', 'i'), 'İ', 'i'),
        replace(replace(old.body, 'ı', 'i'), 'İ', 'i'));
END;
CREATE TRIGGER search_documents_au AFTER UPDATE ON search_documents BEGIN
    INSERT INTO search_index (search_index, rowid, title, body) VALUES (
        'delete', old.rowid,
        replace(replace(old.title, 'ı', 'i'), 'İ', 'i'),
        replace(replace(old.body, 'ı', 'i'), 'İ', 'i'));
    INSERT INTO search_index (rowid, title, body) VALUES (
        new.rowid,
        replace(replace(new.title, 'ı', 'i'), 'İ', 'i'),
        replace(replace(new.body, 'ı', 'i'), 'İ', 'i'));
END;

-- Rebuild the index from search_documents with the folding applied.
INSERT INTO search_index (search_index) VALUES ('delete-all');
INSERT INTO search_index (rowid, title, body)
SELECT rowid,
       replace(replace(title, 'ı', 'i'), 'İ', 'i'),
       replace(replace(body, 'ı', 'i'), 'İ', 'i')
FROM search_documents;
