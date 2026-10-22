use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::SqliteExecutor;
use sqlx::types::Json;
use utoipa::{IntoParams, ToSchema};

use super::api_keys::Principal;
use super::entity_type::EntityType;
use super::field_definitions::{self, Write};
use super::{Page, events, users};
use crate::custom_fields::CustomFields;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::service::search::Filter;
use crate::util;

#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct Contact {
    pub id: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    /// Lowercased; unique across contacts.
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Job title.
    pub title: Option<String>,
    pub company_id: Option<String>,
    /// User who owns the contact.
    pub owner_id: Option<String>,
    #[schema(value_type = Object)]
    #[schemars(with = "CustomFields")]
    pub custom: Json<CustomFields>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// At least one of `first_name`, `last_name` or `email` is required.
#[derive(Debug, Clone, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewContact {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    /// Must be unique.
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Job title.
    pub title: Option<String>,
    pub company_id: Option<String>,
    /// User ID or `"me"`. Omit to default to the API key's user; null for no owner.
    #[serde(default, deserialize_with = "util::double_option")]
    pub owner_id: Option<Option<String>>,
    /// Arbitrary user-defined fields.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: CustomFields,
}

/// Omitted fields are left unchanged; nullable fields set to null are cleared.
#[derive(Debug, Clone, Default, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactPatch {
    #[serde(default, deserialize_with = "util::double_option")]
    pub first_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub last_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub email: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub phone: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub company_id: Option<Option<String>>,
    /// User ID or `"me"`.
    #[serde(default, deserialize_with = "util::double_option")]
    pub owner_id: Option<Option<String>>,
    /// Merged into existing custom fields; keys set to null are removed.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<CustomFields>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
pub struct ContactQuery {
    /// Full-text search: every word must match as a prefix, e.g. `ada acm`.
    /// Searches all text fields, including custom field values.
    pub q: Option<String>,
    pub company_id: Option<String>,
    /// A user ID, `me`, or `none` for contacts without an owner.
    pub owner_id: Option<String>,
    /// 1-200, default 50.
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

fn normalize_email(email: Option<String>) -> Result<Option<String>> {
    let email = util::non_empty(email).map(|e| e.to_lowercase());
    if let Some(e) = &email
        && !e.contains('@')
    {
        return Err(Error::Invalid(format!("invalid email: {e}")));
    }
    Ok(email)
}

fn validate(contact: &Contact) -> Result<()> {
    if contact.first_name.is_none() && contact.last_name.is_none() && contact.email.is_none() {
        return Err(Error::Invalid(
            "a contact needs at least one of first_name, last_name or email".into(),
        ));
    }
    Ok(())
}

pub async fn get(executor: impl SqliteExecutor<'_>, id: &str) -> Result<Contact> {
    sqlx::query_as!(
        Contact,
        r#"SELECT id, first_name, last_name, email, phone, title, company_id, owner_id,
                  custom AS "custom: Json<CustomFields>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM contacts WHERE id = ?"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("contact"))
}

pub async fn list(db: &Db, caller: &Principal, query: ContactQuery) -> Result<Page<Contact>> {
    let limit = util::clamp_limit(query.limit);
    let search = match Filter::from_query(query.q) {
        Filter::All => None,
        Filter::Matching(expression) => Some(expression),
        Filter::Nothing => return Ok(Page::empty()),
    };
    let owner_id = users::resolve_me(query.owner_id, caller)?;
    let fetch = limit + 1;
    let rows = sqlx::query_as!(
        Contact,
        r#"SELECT id, first_name, last_name, email, phone, title, company_id, owner_id,
                  custom AS "custom: Json<CustomFields>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM contacts
           WHERE (?1 IS NULL OR id IN (
                      SELECT d.entity_id FROM search_index
                      JOIN search_documents d ON d.rowid = search_index.rowid
                      WHERE search_index MATCH ?1))
             AND (?2 IS NULL OR company_id = ?2)
             AND (?3 IS NULL OR owner_id = ?3 OR (?3 = ?4 AND owner_id IS NULL))
             AND (?5 IS NULL OR id > ?5)
           ORDER BY id
           LIMIT ?6"#,
        search,
        query.company_id,
        owner_id,
        users::NO_OWNER,
        query.cursor,
        fetch,
    )
    .fetch_all(&db.reader)
    .await?;
    Ok(util::paginate(rows, limit, |c| &c.id))
}

pub async fn create(db: &Db, caller: &Principal, input: NewContact) -> Result<Contact> {
    let now = Utc::now();
    let mut contact = Contact {
        id: util::new_id("con"),
        first_name: util::non_empty(input.first_name),
        last_name: util::non_empty(input.last_name),
        email: normalize_email(input.email)?,
        phone: util::non_empty(input.phone),
        title: util::non_empty(input.title),
        company_id: util::non_empty(input.company_id),
        owner_id: users::owner_for_new(input.owner_id, caller)?,
        custom: Json(input.custom),
        created_at: now,
        updated_at: now,
    };
    validate(&contact)?;

    let mut tx = db.writer.begin().await?;
    users::ensure_assignable(&mut tx, contact.owner_id.as_deref()).await?;
    let custom = std::mem::take(&mut contact.custom.0);
    contact.custom = Json(
        field_definitions::validate(&mut tx, EntityType::Contact, custom, Write::Create).await?,
    );
    sqlx::query!(
        "INSERT INTO contacts
             (id, first_name, last_name, email, phone, title, company_id, owner_id, custom,
              created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        contact.id,
        contact.first_name,
        contact.last_name,
        contact.email,
        contact.phone,
        contact.title,
        contact.company_id,
        contact.owner_id,
        contact.custom,
        contact.created_at,
        contact.updated_at,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "contact.created", &contact.id, &contact).await?;
    tx.commit().await?;
    Ok(contact)
}

pub async fn update(db: &Db, caller: &Principal, id: &str, patch: ContactPatch) -> Result<Contact> {
    let owner_id = users::owner_patch(patch.owner_id, caller)?;
    let mut tx = db.writer.begin().await?;
    let mut contact = get(&mut *tx, id).await?;
    util::patch(
        &mut contact.first_name,
        patch.first_name.map(util::non_empty),
    );
    util::patch(&mut contact.last_name, patch.last_name.map(util::non_empty));
    if let Some(email) = patch.email {
        contact.email = normalize_email(email)?;
    }
    util::patch(&mut contact.phone, patch.phone.map(util::non_empty));
    util::patch(&mut contact.title, patch.title.map(util::non_empty));
    util::patch(
        &mut contact.company_id,
        patch.company_id.map(util::non_empty),
    );
    if let Some(owner_id) = owner_id {
        users::ensure_assignable(&mut tx, owner_id.as_deref()).await?;
        contact.owner_id = owner_id;
    }
    if let Some(update) = patch.custom {
        let update =
            field_definitions::validate(&mut tx, EntityType::Contact, update, Write::Update)
                .await?;
        contact.custom.merge(update);
    }
    contact.updated_at = Utc::now();
    validate(&contact)?;

    sqlx::query!(
        "UPDATE contacts
         SET first_name = ?, last_name = ?, email = ?, phone = ?, title = ?,
             company_id = ?, owner_id = ?, custom = ?, updated_at = ?
         WHERE id = ?",
        contact.first_name,
        contact.last_name,
        contact.email,
        contact.phone,
        contact.title,
        contact.company_id,
        contact.owner_id,
        contact.custom,
        contact.updated_at,
        id,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "contact.updated", id, &contact).await?;
    tx.commit().await?;
    Ok(contact)
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    let mut tx = db.writer.begin().await?;
    let contact = get(&mut *tx, id).await?;
    sqlx::query!("DELETE FROM contacts WHERE id = ?", id)
        .execute(&mut *tx)
        .await?;
    events::record(&mut tx, "contact.deleted", id, &contact).await?;
    tx.commit().await?;
    Ok(())
}
