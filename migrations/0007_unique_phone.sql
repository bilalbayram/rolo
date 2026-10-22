-- Contact phone numbers are stored in E.164 form (+905321234567) and are unique
-- when present. Normalizing needs libphonenumber and the server's default
-- region, so `rolo serve` rewrites older numbers at startup
-- (contacts::normalize_stored_phones). This migration only makes the unique
-- index possible without losing data: blank numbers become NULL, and when
-- contacts share a number exactly, the oldest keeps it and the others get it in
-- custom.legacy_phone instead.
UPDATE contacts SET phone = NULL WHERE trim(phone) = '';
UPDATE contacts SET phone = trim(phone) WHERE phone <> trim(phone);
UPDATE contacts
SET custom = json_set(custom, '$.legacy_phone', phone), phone = NULL
WHERE phone IS NOT NULL
  AND EXISTS (SELECT 1 FROM contacts older WHERE older.phone = contacts.phone AND older.id < contacts.id);

CREATE UNIQUE INDEX contacts_phone_key ON contacts (phone) WHERE phone IS NOT NULL;
