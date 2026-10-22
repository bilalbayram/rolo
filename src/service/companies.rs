use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::SqliteExecutor;
use sqlx::types::Json;
use utoipa::{IntoParams, ToSchema};

use super::api_keys::Principal;
use super::entity_type::EntityType;
use super::field_definitions::{self, Write};
use super::{Page, Sort, events, users};
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
    /// User who owns the company.
    pub owner_id: Option<String>,
    #[schema(value_type = Object)]
    #[schemars(with = "CustomFields")]
    pub custom: Json<CustomFields>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCompany {
    pub name: String,
    /// e.g. `acme.com`. Must be unique.
    pub domain: Option<String>,
    /// User ID or `"me"`. Omit to default to the API key's user; null for no owner.
    #[serde(default, deserialize_with = "util::double_option")]
    pub owner_id: Option<Option<String>>,
    /// Arbitrary user-defined fields.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: CustomFields,
    /// For imports: when the record was originally created, as an RFC 3339
    /// timestamp that isn't in the future. `updated_at` gets the same value.
    /// Defaults to now.
    pub created_at: Option<DateTime<Utc>>,
}

/// Omitted fields are left unchanged; nullable fields set to null are cleared.
#[derive(Debug, Clone, Default, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompanyPatch {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub domain: Option<Option<String>>,
    /// User ID or `"me"`.
    #[serde(default, deserialize_with = "util::double_option")]
    pub owner_id: Option<Option<String>>,
    /// Merged into existing custom fields; keys set to null are removed.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<CustomFields>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CompanyQuery {
    /// Full-text search: every word must match as a prefix, e.g. `acm rob`.
    /// Searches all text fields, including custom field values.
    pub q: Option<String>,
    /// A user ID, `me`, or `none` for companies without an owner.
    pub owner_id: Option<String>,
    /// Comma-separated IDs to fetch, at most 100, e.g. `com_a,com_b`. Matches
    /// come back in the order given, in one page; unknown IDs are left out.
    /// Can't be combined with `limit`, `cursor`, `sort` or `include_total`.
    pub ids: Option<String>,
    /// `id` (default, oldest first) or `-id` (newest first).
    pub sort: Option<Sort>,
    /// Also return `total`, the number of records matching the other filters
    /// across all pages (`cursor` and `limit` don't affect it).
    #[serde(default)]
    pub include_total: bool,
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
        r#"SELECT id, name, domain, owner_id,
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

pub async fn list(db: &Db, caller: &Principal, query: CompanyQuery) -> Result<Page<Company>> {
    let ids = util::IdList::parse(query.ids)?;
    let limit = match &ids {
        Some(_)
            if query.limit.is_some()
                || query.cursor.is_some()
                || query.sort.is_some()
                || query.include_total =>
        {
            return Err(util::IdList::exclusive());
        }
        Some(ids) => ids.count(),
        None => util::clamp_limit(query.limit),
    };
    let search = match Filter::from_query(query.q) {
        Filter::All => None,
        Filter::Matching(expression) => Some(expression),
        Filter::Nothing => return Ok(Page::empty().with_total(query.include_total.then_some(0))),
    };
    let owner_id = users::resolve_me(query.owner_id, caller)?;
    let direction = query.sort.unwrap_or_default().direction();
    let ids_json = ids.as_ref().map(util::IdList::to_json);
    let total = if query.include_total {
        Some(
            sqlx::query_scalar!(
                r#"SELECT count(*) AS "total!: i64" FROM companies
                   WHERE (?1 IS NULL OR id IN (
                              SELECT d.entity_id FROM search_index
                              JOIN search_documents d ON d.rowid = search_index.rowid
                              WHERE search_index MATCH ?1))
                     AND (?2 IS NULL OR owner_id = ?2 OR (?2 = ?3 AND owner_id IS NULL))"#,
                search,
                owner_id,
                users::NO_OWNER,
            )
            .fetch_one(&db.reader)
            .await?,
        )
    } else {
        None
    };
    let fetch = limit + 1;
    let rows = sqlx::query_as!(
        Company,
        r#"SELECT id, name, domain, owner_id,
                  custom AS "custom: Json<CustomFields>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM companies
           WHERE (?1 IS NULL OR id IN (
                      SELECT d.entity_id FROM search_index
                      JOIN search_documents d ON d.rowid = search_index.rowid
                      WHERE search_index MATCH ?1))
             AND (?2 IS NULL OR owner_id = ?2 OR (?2 = ?3 AND owner_id IS NULL))
             AND (?4 IS NULL OR (?5 = 'asc' AND id > ?4) OR (?5 = 'desc' AND id < ?4))
             AND (?6 IS NULL OR id IN (SELECT value FROM json_each(?6)))
           ORDER BY CASE WHEN ?5 = 'desc' THEN id END DESC, id ASC
           LIMIT ?7"#,
        search,
        owner_id,
        users::NO_OWNER,
        query.cursor,
        direction,
        ids_json,
        fetch,
    )
    .fetch_all(&db.reader)
    .await?;
    let mut page = util::paginate(rows, limit, |c| &c.id);
    if let Some(ids) = &ids {
        page.data = ids.in_order(page.data, |c| &c.id);
    }
    Ok(page.with_total(total))
}

pub async fn create(db: &Db, caller: &Principal, input: NewCompany) -> Result<Company> {
    let (id, created_at) = util::new_record("com", input.created_at)?;
    let mut company = Company {
        id,
        name: util::required(input.name, "name")?,
        domain: normalize_domain(input.domain),
        owner_id: users::owner_for_new(input.owner_id, caller)?,
        custom: Json(input.custom),
        created_at,
        updated_at: created_at,
    };

    let mut tx = db.writer.begin().await?;
    users::ensure_assignable(&mut tx, company.owner_id.as_deref()).await?;
    let custom = std::mem::take(&mut company.custom.0);
    company.custom = Json(
        field_definitions::validate(
            &mut tx,
            EntityType::Company,
            custom,
            Write::Create(&company.id),
        )
        .await?,
    );
    sqlx::query!(
        "INSERT INTO companies (id, name, domain, owner_id, custom, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        company.id,
        company.name,
        company.domain,
        company.owner_id,
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

pub async fn update(db: &Db, caller: &Principal, id: &str, patch: CompanyPatch) -> Result<Company> {
    let owner_id = users::owner_patch(patch.owner_id, caller)?;
    let mut tx = db.writer.begin().await?;
    let mut company = get(&mut *tx, id).await?;
    if let Some(name) = patch.name {
        company.name = util::required(name, "name")?;
    }
    if let Some(domain) = patch.domain {
        company.domain = normalize_domain(domain);
    }
    if let Some(owner_id) = owner_id {
        users::ensure_assignable(&mut tx, owner_id.as_deref()).await?;
        company.owner_id = owner_id;
    }
    if let Some(update) = patch.custom {
        let update =
            field_definitions::validate(&mut tx, EntityType::Company, update, Write::Update(id))
                .await?;
        company.custom.merge(update);
    }
    company.updated_at = Utc::now();

    sqlx::query!(
        "UPDATE companies SET name = ?, domain = ?, owner_id = ?, custom = ?, updated_at = ?
         WHERE id = ?",
        company.name,
        company.domain,
        company.owner_id,
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
