-- Unique custom fields: no two records of the same type may share a value for
-- the key. Set when the field is defined and fixed afterwards; enforced on
-- write by field_definitions::validate inside the write transaction.
ALTER TABLE custom_field_definitions
    ADD COLUMN is_unique INTEGER NOT NULL DEFAULT 0 CHECK (is_unique IN (0, 1));

-- Every record's custom fields, tagged with its record type.
CREATE VIEW record_custom_fields AS
SELECT 'contact' AS entity_type, id, custom FROM contacts
UNION ALL SELECT 'company', id, custom FROM companies
UNION ALL SELECT 'deal', id, custom FROM deals
UNION ALL SELECT 'activity', id, custom FROM activities;
