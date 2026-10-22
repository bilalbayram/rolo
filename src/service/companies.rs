use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::SqliteExecutor;
use sqlx::types::Json;
use utoipa::{IntoParams, ToSchema};

use super::{Page, events};
use crate::custom_fields::CustomFields;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::service::search::Filter;
use crate::util;

#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct Company {
    pub id: String,
    pub name: String,
    /// Lowercased; unique across companies.
    pub domain: Option<String>,
    #[schema(value_type = Object)]
    #[schemars(with = "CustomFields")]
    pub custom: Json<CustomFields>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCompany {
    pub name: String,
    /// e.g. `acme.com`. Must be unique.
    pub domain: Option<String>,
    /// Arbitrary user-defined fields.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: CustomFields,
}

#[derive(Debug, Default, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompanyPatch {
    pub name: Option<String>,
    /// Set to null to clear.
    #[serde(default, deserialize_with = "util::double_option")]
    pub domain: Option<Option<String>>,
    /// Merged into existing custom fields; keys set to null are removed.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<CustomFields>,
}

#[derive(Debug, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
pub struct CompanyQuery {
    /// Full-text search: every word must match as a prefix, e.g. `acm rob`.
    /// Searches all text fields, including custom field values.
    pub q: Option<String>,
    /// 1-200, default 50.
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

fn normalize_domain(domain: Option<String>) -> Option<String> {
    util::non_empty(domain).map(|d| d.to_lowercase())
}

pub async fn get(executor: impl SqliteExecutor<'_>, id: &str) -> Result<Company> {
    sqlx::query_as!(
        Company,
        r#"SELECT id, name, domain,
                  custom AS "custom: Json<CustomFields>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM companies WHERE id = ?"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("company"))
}

pub async fn list(db: &Db, query: CompanyQuery) -> Result<Page<Company>> {
    let limit = util::clamp_limit(query.limit);
    let search = match Filter::from_query(query.q) {
        Filter::All => None,
        Filter::Matching(expression) => Some(expression),
        Filter::Nothing => return Ok(Page::empty()),
    };
    let fetch = limit + 1;
    let rows = sqlx::query_as!(
        Company,
        r#"SELECT id, name, domain,
                  custom AS "custom: Json<CustomFields>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM companies
           WHERE (?1 IS NULL OR id IN (
                      SELECT d.entity_id FROM search_index
                      JOIN search_documents d ON d.rowid = search_index.rowid
                      WHERE search_index MATCH ?1))
             AND (?2 IS NULL OR id > ?2)
           ORDER BY id
           LIMIT ?3"#,
        search,
        query.cursor,
        fetch,
    )
    .fetch_all(&db.reader)
    .await?;
    Ok(util::paginate(rows, limit, |c| &c.id))
}

pub async fn create(db: &Db, input: NewCompany) -> Result<Company> {
    let now = Utc::now();
    let company = Company {
        id: util::new_id("com"),
        name: util::required(input.name, "name")?,
        domain: normalize_domain(input.domain),
        custom: Json(input.custom),
        created_at: now,
        updated_at: now,
    };

    let mut tx = db.writer.begin().await?;
    sqlx::query!(
        "INSERT INTO companies (id, name, domain, custom, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        company.id,
        company.name,
        company.domain,
        company.custom,
        company.created_at,
        company.updated_at,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "company.created", &company.id, &company).await?;
    tx.commit().await?;
    Ok(company)
}

pub async fn update(db: &Db, id: &str, patch: CompanyPatch) -> Result<Company> {
    let mut tx = db.writer.begin().await?;
    let mut company = get(&mut *tx, id).await?;
    if let Some(name) = patch.name {
        company.name = util::required(name, "name")?;
    }
    if let Some(domain) = patch.domain {
        company.domain = normalize_domain(domain);
    }
    if let Some(update) = patch.custom {
        company.custom.merge(update);
    }
    company.updated_at = Utc::now();

    sqlx::query!(
        "UPDATE companies SET name = ?, domain = ?, custom = ?, updated_at = ? WHERE id = ?",
        company.name,
        company.domain,
        company.custom,
        company.updated_at,
        id,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "company.updated", id, &company).await?;
    tx.commit().await?;
    Ok(company)
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    let mut tx = db.writer.begin().await?;
    let company = get(&mut *tx, id).await?;
    sqlx::query!("DELETE FROM companies WHERE id = ?", id)
        .execute(&mut *tx)
        .await?;
    events::record(&mut tx, "company.deleted", id, &company).await?;
    tx.commit().await?;
    Ok(())
}
