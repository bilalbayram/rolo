-- Typed definitions for keys in records' `custom` objects. Values for defined
-- keys are validated on write; keys without a definition stay free-form.
CREATE TABLE custom_field_definitions (
    id          TEXT PRIMARY KEY NOT NULL,
    entity_type TEXT NOT NULL CHECK (entity_type IN ('contact', 'company', 'deal', 'activity')),
    key         TEXT NOT NULL,
    label       TEXT NOT NULL,
    description TEXT,
    field_type  TEXT NOT NULL CHECK (field_type IN (
                    'text', 'number', 'boolean', 'date', 'datetime', 'url', 'email',
                    'select', 'multiselect')),
    -- Allowed values for select and multiselect fields.
    options     TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(options) AND json_type(options) = 'array'),
    required    INTEGER NOT NULL DEFAULT 0 CHECK (required IN (0, 1)),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (entity_type, key)
);
