//! Typed definitions for keys in records' `custom` objects, and validation of
//! custom field values against them.

use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::{SqliteConnection, SqliteExecutor};
use utoipa::{IntoParams, ToSchema};

use super::entity_type::EntityType;
use crate::custom_fields::CustomFields;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::util;

const MAX_KEY_LENGTH: usize = 64;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum FieldType {
    /// Any string.
    Text,
    /// A JSON number.
    Number,
    Boolean,
    /// `YYYY-MM-DD`.
    Date,
    /// RFC 3339 timestamp; stored in UTC.
    Datetime,
    /// http(s) URL.
    Url,
    /// Stored lowercased.
    Email,
    /// One of `options`.
    Select,
    /// An array of distinct values from `options`.
    Multiselect,
}

#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct FieldDefinition {
    pub id: String,
    pub entity_type: EntityType,
    /// Key in the record's `custom` object.
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    pub field_type: FieldType,
    /// Allowed values for select and multiselect fields.
    #[schema(value_type = Vec<String>)]
    #[schemars(with = "Vec<String>")]
    pub options: Json<Vec<String>>,
    /// Must be set when a record is created, and cannot be removed afterwards.
    pub required: bool,
    /// No two records of this type may have the same value.
    pub unique: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewFieldDefinition {
    pub entity_type: EntityType,
    /// Lowercase letters, digits and underscores, starting with a letter, e.g. `linkedin_url`.
    pub key: String,
    /// Display name. Defaults to the key.
    pub label: Option<String>,
    pub description: Option<String>,
    pub field_type: FieldType,
    /// Required for select and multiselect fields; not allowed otherwise.
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub required: bool,
    /// No two records of this type may have the same value, e.g. an
    /// `instagram_id` on contacts. Fixed once the field is defined; not allowed
    /// for boolean and multiselect fields.
    #[serde(default)]
    pub unique: bool,
}

/// Omitted fields are left unchanged. The key, record type, field type and
/// `unique` are fixed; delete and recreate the definition to change them.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldDefinitionPatch {
    pub label: Option<String>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub description: Option<Option<String>>,
    /// Replaces the allowed values. Existing records are not revalidated.
    pub options: Option<Vec<String>>,
    /// Applies to records created or updated from now on.
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct FieldDefinitionQuery {
    pub entity_type: Option<EntityType>,
}

/// How custom fields are being written, and to which record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write<'a> {
    /// The complete set of fields for a new record with this ID.
    Create(&'a str),
    /// A PATCH to merge into the fields of the record with this ID; null
    /// values remove keys.
    Update(&'a str),
}

impl Write<'_> {
    fn record_id(&self) -> &str {
        match self {
            Write::Create(id) | Write::Update(id) => id,
        }
    }
}

fn validate_key(key: String) -> Result<String> {
    let key = util::required(key, "key")?;
    let mut chars = key.chars();
    let starts_with_letter = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    let rest_valid = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !starts_with_letter || !rest_valid || key.len() > MAX_KEY_LENGTH {
        return Err(Error::Invalid(format!(
            "key must be lowercase letters, digits and underscores, start with a letter, \
             and be at most {MAX_KEY_LENGTH} characters; got {key:?}"
        )));
    }
    Ok(key)
}

fn validate_options(field_type: FieldType, options: Vec<String>) -> Result<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for option in options {
        let option = util::required(option, "option")?;
        if !cleaned.contains(&option) {
            cleaned.push(option);
        }
    }
    match field_type {
        FieldType::Select | FieldType::Multiselect if cleaned.is_empty() => Err(Error::Invalid(
            "select and multiselect fields need at least one option".into(),
        )),
        FieldType::Select | FieldType::Multiselect => Ok(cleaned),
        _ if !cleaned.is_empty() => Err(Error::Invalid(
            "options are only allowed for select and multiselect fields".into(),
        )),
        _ => Ok(cleaned),
    }
}

pub async fn get(executor: impl SqliteExecutor<'_>, id: &str) -> Result<FieldDefinition> {
    sqlx::query_as!(
        FieldDefinition,
        r#"SELECT id, entity_type AS "entity_type: EntityType", key, label, description,
                  field_type AS "field_type: FieldType",
                  options AS "options: Json<Vec<String>>",
                  required AS "required: bool",
                  is_unique AS "unique: bool",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM custom_field_definitions WHERE id = ?"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("custom field"))
}

/// The definition of `key` for records of `entity_type`, if there is one.
pub async fn get_by_key(
    conn: &mut SqliteConnection,
    entity_type: EntityType,
    key: &str,
) -> Result<Option<FieldDefinition>> {
    let definitions = list_for(conn, Some(entity_type)).await?;
    Ok(definitions
        .into_iter()
        .find(|definition| definition.key == key))
}

/// Ordered by record type, then key.
pub async fn list(db: &Db, query: FieldDefinitionQuery) -> Result<Vec<FieldDefinition>> {
    let mut conn = db.reader.acquire().await?;
    list_for(&mut conn, query.entity_type).await
}

async fn list_for(
    conn: &mut SqliteConnection,
    entity_type: Option<EntityType>,
) -> Result<Vec<FieldDefinition>> {
    Ok(sqlx::query_as!(
        FieldDefinition,
        r#"SELECT id, entity_type AS "entity_type: EntityType", key, label, description,
                  field_type AS "field_type: FieldType",
                  options AS "options: Json<Vec<String>>",
                  required AS "required: bool",
                  is_unique AS "unique: bool",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM custom_field_definitions
           WHERE ?1 IS NULL OR entity_type = ?1
           ORDER BY entity_type, key"#,
        entity_type,
    )
    .fetch_all(conn)
    .await?)
}

pub async fn create(db: &Db, input: NewFieldDefinition) -> Result<FieldDefinition> {
    let now = Utc::now();
    let key = validate_key(input.key)?;
    if input.unique
        && matches!(
            input.field_type,
            FieldType::Boolean | FieldType::Multiselect
        )
    {
        return Err(Error::Invalid(
            "boolean and multiselect fields cannot be unique".into(),
        ));
    }
    let definition = FieldDefinition {
        id: util::new_id("cfd"),
        entity_type: input.entity_type,
        label: util::non_empty(input.label).unwrap_or_else(|| key.clone()),
        key,
        description: util::non_empty(input.description),
        field_type: input.field_type,
        options: Json(validate_options(input.field_type, input.options)?),
        required: input.required,
        unique: input.unique,
        created_at: now,
        updated_at: now,
    };
    let mut tx = db.writer.begin().await?;
    if definition.unique {
        let duplicated = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM record_custom_fields
                   WHERE entity_type = ?1 AND json_extract(custom, '$.' || ?2) IS NOT NULL
                   GROUP BY json_extract(custom, '$.' || ?2)
                   HAVING count(*) > 1
               ) AS "duplicated: bool""#,
            definition.entity_type,
            definition.key,
        )
        .fetch_one(&mut *tx)
        .await?;
        if duplicated {
            return Err(Error::Conflict(format!(
                "custom.{} cannot be unique: some records already share a value",
                definition.key
            )));
        }
    }
    sqlx::query!(
        "INSERT INTO custom_field_definitions
             (id, entity_type, key, label, description, field_type, options, required,
              is_unique, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        definition.id,
        definition.entity_type,
        definition.key,
        definition.label,
        definition.description,
        definition.field_type,
        definition.options,
        definition.required,
        definition.unique,
        definition.created_at,
        definition.updated_at,
    )
    .execute(&mut *tx)
    .await
    .map_err(|err| match Error::from(err) {
        Error::Conflict(_) => Error::Conflict(format!(
            "a custom field {:?} already exists for this record type",
            definition.key
        )),
        other => other,
    })?;
    tx.commit().await?;
    Ok(definition)
}

pub async fn update(db: &Db, id: &str, patch: FieldDefinitionPatch) -> Result<FieldDefinition> {
    let mut tx = db.writer.begin().await?;
    let mut definition = get(&mut *tx, id).await?;
    if let Some(label) = patch.label {
        definition.label = util::required(label, "label")?;
    }
    util::patch(
        &mut definition.description,
        patch.description.map(util::non_empty),
    );
    if let Some(options) = patch.options {
        definition.options = Json(validate_options(definition.field_type, options)?);
    }
    if let Some(required) = patch.required {
        definition.required = required;
    }
    definition.updated_at = Utc::now();
    sqlx::query!(
        "UPDATE custom_field_definitions
         SET label = ?, description = ?, options = ?, required = ?, updated_at = ?
         WHERE id = ?",
        definition.label,
        definition.description,
        definition.options,
        definition.required,
        definition.updated_at,
        id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(definition)
}

/// Existing values for the key are kept and become free-form.
pub async fn delete(db: &Db, id: &str) -> Result<()> {
    let result = sqlx::query!("DELETE FROM custom_field_definitions WHERE id = ?", id)
        .execute(&db.writer)
        .await?;
    if result.rows_affected() == 0 {
        return Err(Error::NotFound("custom field"));
    }
    Ok(())
}

/// Validate and normalize custom field values being written to a record of
/// `entity_type`. Keys without a definition pass through unchanged. Invalid
/// values are reported together in one `Invalid` error; then values of unique
/// fields that another record already has are reported in one `Conflict`.
/// Call it in the write transaction, so that no other write can take a unique
/// value between the check and the write.
pub async fn validate(
    conn: &mut SqliteConnection,
    entity_type: EntityType,
    fields: CustomFields,
    write: Write<'_>,
) -> Result<CustomFields> {
    let definitions = list_for(conn, Some(entity_type)).await?;
    if definitions.is_empty() {
        return Ok(fields);
    }
    let definition_for = |key: &str| definitions.iter().find(|d| d.key == key);

    let mut problems: Vec<String> = Vec::new();
    if matches!(write, Write::Create(_)) {
        let missing = definitions
            .iter()
            .filter(|d| d.required && fields.get(&d.key).is_none_or(Value::is_null));
        for definition in missing {
            problems.push(format!("custom.{}: required", definition.key));
        }
    }
    let mut normalized: Vec<(String, Value)> = Vec::new();
    for (key, value) in fields {
        let Some(definition) = definition_for(&key) else {
            normalized.push((key, value));
            continue;
        };
        if value.is_null() {
            if definition.required && matches!(write, Write::Update(_)) {
                problems.push(format!("custom.{key}: required and cannot be removed"));
            }
            normalized.push((key, value));
            continue;
        }
        match check_value(definition, value) {
            Ok(value) => normalized.push((key, value)),
            Err(problem) => problems.push(format!("custom.{key}: {problem}")),
        }
    }
    if !problems.is_empty() {
        return Err(Error::Invalid(problems.join("; ")));
    }

    let record_type = entity_type.name();
    let mut conflicts: Vec<String> = Vec::new();
    for (key, value) in &normalized {
        if value.is_null() || !definition_for(key).is_some_and(|d| d.unique) {
            continue;
        }
        if find_by_value(conn, entity_type, key, value)
            .await?
            .is_some_and(|id| id != write.record_id())
        {
            conflicts.push(format!(
                "custom.{key}: another {record_type} has this value"
            ));
        }
    }
    if !conflicts.is_empty() {
        return Err(Error::Conflict(conflicts.join("; ")));
    }
    Ok(normalized.into_iter().collect())
}

/// Exact-match filters on custom fields of `entity_type` for a list query, as
/// a JSON object of normalized values to pass to `json_each`; `None` without
/// filters. Every key must be defined. Values may also be strings, as they
/// come from a query string: `"120"` for a number field, `"true"` for a
/// boolean. Multiselect fields can't be filtered. All problems are reported
/// together.
pub async fn list_filters(
    db: &Db,
    entity_type: EntityType,
    filters: Option<CustomFields>,
) -> Result<Option<String>> {
    let Some(filters) = filters.filter(|filters| !filters.is_empty()) else {
        return Ok(None);
    };
    let mut conn = db.reader.acquire().await?;
    let definitions = list_for(&mut conn, Some(entity_type)).await?;
    let mut problems: Vec<String> = Vec::new();
    let mut normalized = serde_json::Map::new();
    for (key, value) in filters {
        let Some(definition) = definitions.iter().find(|d| d.key == key) else {
            problems.push(format!(
                "custom.{key}: not a custom field defined for {}s",
                entity_type.name()
            ));
            continue;
        };
        if definition.field_type == FieldType::Multiselect {
            problems.push(format!(
                "custom.{key}: multiselect fields can't be filtered"
            ));
            continue;
        }
        match check_value(definition, from_query_string(definition.field_type, value)) {
            Ok(value) => {
                normalized.insert(key, value);
            }
            Err(problem) => problems.push(format!("custom.{key}: {problem}")),
        }
    }
    if !problems.is_empty() {
        return Err(Error::Invalid(problems.join("; ")));
    }
    Ok(Some(Value::Object(normalized).to_string()))
}

/// `value` as a number or boolean, if it is such a value written as a string.
fn from_query_string(field_type: FieldType, value: Value) -> Value {
    let Some(text) = value.as_str().map(str::trim) else {
        return value;
    };
    let parsed = match field_type {
        FieldType::Number => serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_number),
        FieldType::Boolean => text.parse::<bool>().ok().map(Value::Bool),
        _ => None,
    };
    parsed.unwrap_or(value)
}

/// The definition of `key` for `entity_type`, if records can be matched on
/// it, which needs a unique field.
pub async fn for_matching(
    conn: &mut SqliteConnection,
    entity_type: EntityType,
    key: &str,
) -> Result<FieldDefinition> {
    get_by_key(conn, entity_type, key)
        .await?
        .filter(|definition| definition.unique)
        .ok_or_else(|| {
            Error::Invalid(format!(
                "cannot match on custom.{key}: it is not a unique custom field for {}s",
                entity_type.name()
            ))
        })
}

/// The record whose unique field `definition` holds `value`, after
/// normalizing `value` as on write.
pub async fn find_by_unique_value(
    conn: &mut SqliteConnection,
    definition: &FieldDefinition,
    value: &Value,
) -> Result<Option<String>> {
    let value = normalize_value(definition, value.clone())?;
    find_by_value(conn, definition.entity_type, &definition.key, &value).await
}

/// The first record of `entity_type` whose custom field `key` equals `value`.
pub async fn find_by_value(
    conn: &mut SqliteConnection,
    entity_type: EntityType,
    key: &str,
    value: &Value,
) -> Result<Option<String>> {
    let value = value.to_string();
    Ok(sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM record_custom_fields
           WHERE entity_type = ?1 AND json_extract(custom, '$.' || ?2) = json_extract(?3, '$')
           ORDER BY id
           LIMIT 1"#,
        entity_type,
        key,
        value,
    )
    .fetch_optional(conn)
    .await?)
}

/// `value` checked and normalized for `definition`, as it would be stored.
pub fn normalize_value(definition: &FieldDefinition, value: Value) -> Result<Value> {
    check_value(definition, value)
        .map_err(|problem| Error::Invalid(format!("custom.{}: {problem}", definition.key)))
}

/// The normalized value, or a description of why it is invalid.
fn check_value(definition: &FieldDefinition, value: Value) -> std::result::Result<Value, String> {
    let options = &definition.options.0;
    let string = || value.as_str().map(str::trim);
    match definition.field_type {
        FieldType::Text => string()
            .map(|_| value.clone())
            .ok_or_else(|| "must be a string".into()),
        FieldType::Number if value.is_number() => Ok(value),
        FieldType::Number => Err("must be a number".into()),
        FieldType::Boolean if value.is_boolean() => Ok(value),
        FieldType::Boolean => Err("must be true or false".into()),
        FieldType::Date => string()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .map(|date| Value::String(date.format("%Y-%m-%d").to_string()))
            .ok_or_else(|| "must be a date like 2026-10-22".into()),
        FieldType::Datetime => string()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|at| Value::String(at.with_timezone(&Utc).to_rfc3339()))
            .ok_or_else(|| "must be an RFC 3339 timestamp like 2026-10-22T09:00:00Z".into()),
        FieldType::Url => string()
            .filter(|s| {
                reqwest::Url::parse(s).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
                })
            })
            .map(|s| Value::String(s.to_string()))
            .ok_or_else(|| "must be an http(s) URL".into()),
        FieldType::Email => string()
            .filter(|s| s.contains('@') && !s.contains(char::is_whitespace))
            .map(|s| Value::String(s.to_lowercase()))
            .ok_or_else(|| "must be an email address".into()),
        FieldType::Select => string()
            .filter(|s| options.iter().any(|option| option == s))
            .map(|s| Value::String(s.to_string()))
            .ok_or_else(|| format!("must be one of: {}", options.join(", "))),
        FieldType::Multiselect => {
            let invalid = || format!("must be an array of values from: {}", options.join(", "));
            let items = value.as_array().ok_or_else(invalid)?;
            let mut selected: Vec<Value> = Vec::new();
            for item in items {
                let item = item
                    .as_str()
                    .map(str::trim)
                    .filter(|s| options.iter().any(|option| option == s))
                    .ok_or_else(invalid)?;
                let item = Value::String(item.to_string());
                if !selected.contains(&item) {
                    selected.push(item);
                }
            }
            Ok(Value::Array(selected))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn definition(field_type: FieldType, options: &[&str]) -> FieldDefinition {
        let now = Utc::now();
        FieldDefinition {
            id: "cfd_test".into(),
            entity_type: EntityType::Contact,
            key: "field".into(),
            label: "Field".into(),
            description: None,
            field_type,
            options: Json(options.iter().map(|o| o.to_string()).collect()),
            required: false,
            unique: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn checks_and_normalizes_values() {
        let check = |field_type, options: &[&str], value| {
            check_value(&definition(field_type, options), value)
        };
        assert_eq!(check(FieldType::Number, &[], json!(4.5)), Ok(json!(4.5)));
        assert!(check(FieldType::Number, &[], json!("4.5")).is_err());
        assert_eq!(
            check(FieldType::Date, &[], json!(" 2026-10-22 ")),
            Ok(json!("2026-10-22"))
        );
        assert!(check(FieldType::Date, &[], json!("22/10/2026")).is_err());
        assert_eq!(
            check(FieldType::Datetime, &[], json!("2026-10-22T11:00:00+02:00")),
            Ok(json!("2026-10-22T09:00:00+00:00"))
        );
        assert_eq!(
            check(FieldType::Email, &[], json!("Ada@Example.com")),
            Ok(json!("ada@example.com"))
        );
        assert!(check(FieldType::Url, &[], json!("javascript:alert(1)")).is_err());
        assert_eq!(
            check(FieldType::Select, &["gold", "silver"], json!("gold")),
            Ok(json!("gold"))
        );
        assert_eq!(
            check(FieldType::Select, &["gold", "silver"], json!("bronze")),
            Err("must be one of: gold, silver".into())
        );
        assert_eq!(
            check(FieldType::Multiselect, &["a", "b"], json!(["b", "a", "b"])),
            Ok(json!(["b", "a"]))
        );
        assert!(check(FieldType::Multiselect, &["a"], json!("a")).is_err());
    }

    #[test]
    fn validates_keys_and_options() {
        assert!(validate_key("linkedin_url".into()).is_ok());
        assert!(validate_key("LinkedIn".into()).is_err());
        assert!(validate_key("1st".into()).is_err());
        assert!(validate_options(FieldType::Select, vec![]).is_err());
        assert!(validate_options(FieldType::Text, vec!["x".into()]).is_err());
        assert_eq!(
            validate_options(
                FieldType::Select,
                vec![" a ".into(), "a".into(), "b".into()]
            )
            .ok(),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }
}
