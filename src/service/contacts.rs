use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{SqliteExecutor, types::Json};
use utoipa::{IntoParams, ToSchema};

use super::{Page, events, like_pattern};
use crate::{
    db::Db,
    error::{Error, Result},
    util::{self, Custom, double_option, non_empty},
};

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
    #[schema(value_type = Object)]
    #[schemars(with = "Custom")]
    pub custom: Json<Custom>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// At least one of `first_name`, `last_name` or `email` is required.
#[derive(Debug, Deserialize, ToSchema, JsonSchema)]
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
    /// Arbitrary user-defined fields.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: Custom,
}

/// Omitted fields are left unchanged; nullable fields set to null are cleared.
#[derive(Debug, Default, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactPatch {
    #[serde(default, deserialize_with = "double_option")]
    pub first_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub last_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub email: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub phone: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub company_id: Option<Option<String>>,
    /// Merged into existing custom fields; keys set to null are removed.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Custom>,
}

#[derive(Debug, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
pub struct ContactQuery {
    /// Case-insensitive substring match on name, email or phone.
    pub q: Option<String>,
    pub company_id: Option<String>,
    /// 1-200, default 50.
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

fn normalize_email(email: Option<String>) -> Result<Option<String>> {
    let email = non_empty(email).map(|e| e.to_lowercase());
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

pub async fn get(ex: impl SqliteExecutor<'_>, id: &str) -> Result<Contact> {
    sqlx::query_as!(
        Contact,
        r#"SELECT id, first_name, last_name, email, phone, title, company_id,
                  custom AS "custom: Json<Custom>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM contacts WHERE id = ?"#,
        id,
    )
    .fetch_optional(ex)
    .await?
    .ok_or(Error::NotFound("contact"))
}

pub async fn list(db: &Db, query: ContactQuery) -> Result<Page<Contact>> {
    let limit = util::clamp_limit(query.limit);
    let pattern = like_pattern(query.q);
    let fetch = limit + 1;
    let rows = sqlx::query_as!(
        Contact,
        r#"SELECT id, first_name, last_name, email, phone, title, company_id,
                  custom AS "custom: Json<Custom>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM contacts
           WHERE (?1 IS NULL
                  OR (coalesce(first_name, '') || ' ' || coalesce(last_name, '')) LIKE ?1 ESCAPE '\'
                  OR email LIKE ?1 ESCAPE '\'
                  OR phone LIKE ?1 ESCAPE '\')
             AND (?2 IS NULL OR company_id = ?2)
             AND (?3 IS NULL OR id > ?3)
           ORDER BY id
           LIMIT ?4"#,
        pattern,
        query.company_id,
        query.cursor,
        fetch,
    )
    .fetch_all(&db.reader)
    .await?;
    Ok(util::paginate(rows, limit, |c| &c.id))
}

pub async fn create(db: &Db, input: NewContact) -> Result<Contact> {
    let now = Utc::now();
    let contact = Contact {
        id: util::new_id("con"),
        first_name: non_empty(input.first_name),
        last_name: non_empty(input.last_name),
        email: normalize_email(input.email)?,
        phone: non_empty(input.phone),
        title: non_empty(input.title),
        company_id: non_empty(input.company_id),
        custom: Json(input.custom),
        created_at: now,
        updated_at: now,
    };
    validate(&contact)?;

    let mut tx = db.writer.begin().await?;
    sqlx::query!(
        "INSERT INTO contacts
             (id, first_name, last_name, email, phone, title, company_id, custom, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        contact.id,
        contact.first_name,
        contact.last_name,
        contact.email,
        contact.phone,
        contact.title,
        contact.company_id,
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

pub async fn update(db: &Db, id: &str, patch: ContactPatch) -> Result<Contact> {
    let mut tx = db.writer.begin().await?;
    let mut contact = get(&mut *tx, id).await?;
    util::patch(&mut contact.first_name, patch.first_name.map(non_empty));
    util::patch(&mut contact.last_name, patch.last_name.map(non_empty));
    if let Some(email) = patch.email {
        contact.email = normalize_email(email)?;
    }
    util::patch(&mut contact.phone, patch.phone.map(non_empty));
    util::patch(&mut contact.title, patch.title.map(non_empty));
    util::patch(&mut contact.company_id, patch.company_id.map(non_empty));
    util::merge_custom(&mut contact.custom, patch.custom);
    contact.updated_at = Utc::now();
    validate(&contact)?;

    sqlx::query!(
        "UPDATE contacts
         SET first_name = ?, last_name = ?, email = ?, phone = ?, title = ?,
             company_id = ?, custom = ?, updated_at = ?
         WHERE id = ?",
        contact.first_name,
        contact.last_name,
        contact.email,
        contact.phone,
        contact.title,
        contact.company_id,
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
