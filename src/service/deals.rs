use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::SqliteExecutor;
use sqlx::types::Json;
use utoipa::{IntoParams, ToSchema};

use super::api_keys::Principal;
use super::pipelines::{self, StageKind};
use super::{Page, events, users};
use crate::custom_fields::CustomFields;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::service::search::Filter;
use crate::util;

#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct Deal {
    pub id: String,
    pub title: String,
    /// In minor currency units, e.g. cents: 1250000 = 12,500.00.
    pub amount: Option<i64>,
    /// ISO 4217 code, e.g. `USD`.
    pub currency: Option<String>,
    pub pipeline_id: String,
    pub stage_id: String,
    /// Kind of the current stage.
    pub status: StageKind,
    pub company_id: Option<String>,
    /// Primary contact.
    pub contact_id: Option<String>,
    pub expected_close_date: Option<NaiveDate>,
    /// Set when the deal enters a won or lost stage.
    pub closed_at: Option<DateTime<Utc>>,
    /// User who owns the deal.
    pub owner_id: Option<String>,
    #[schema(value_type = Object)]
    #[schemars(with = "CustomFields")]
    pub custom: Json<CustomFields>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewDeal {
    pub title: String,
    /// In minor currency units, e.g. cents.
    pub amount: Option<i64>,
    /// ISO 4217 code, e.g. `USD`.
    pub currency: Option<String>,
    /// Defaults to the first open stage of `pipeline_id`.
    pub stage_id: Option<String>,
    /// Used only when `stage_id` is omitted. Defaults to the first pipeline.
    pub pipeline_id: Option<String>,
    pub company_id: Option<String>,
    pub contact_id: Option<String>,
    pub expected_close_date: Option<NaiveDate>,
    /// User ID or `"me"`. Omit to default to the API key's user; null for no owner.
    #[serde(default, deserialize_with = "util::double_option")]
    pub owner_id: Option<Option<String>>,
    /// Arbitrary user-defined fields.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: CustomFields,
}

/// Omitted fields are left unchanged; nullable fields set to null are cleared.
/// Moving to a won/lost stage sets `closed_at`; moving back to open clears it.
#[derive(Debug, Clone, Default, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DealPatch {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub amount: Option<Option<i64>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub currency: Option<Option<String>>,
    pub stage_id: Option<String>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub company_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub contact_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub expected_close_date: Option<Option<NaiveDate>>,
    /// User ID or `"me"`.
    #[serde(default, deserialize_with = "util::double_option")]
    pub owner_id: Option<Option<String>>,
    /// Merged into existing custom fields; keys set to null are removed.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<CustomFields>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
pub struct DealQuery {
    /// Full-text search: every word must match as a prefix, e.g. `rollout acm`.
    /// Searches the title and custom field values.
    pub q: Option<String>,
    pub pipeline_id: Option<String>,
    pub stage_id: Option<String>,
    pub status: Option<StageKind>,
    pub company_id: Option<String>,
    pub contact_id: Option<String>,
    /// A user ID, `me`, or `none` for deals without an owner.
    pub owner_id: Option<String>,
    /// 1-200, default 50.
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

#[derive(Serialize)]
struct StageChanged<'a> {
    deal: &'a Deal,
    from_stage_id: &'a str,
    to_stage_id: &'a str,
}

fn normalize_currency(currency: Option<String>) -> Result<Option<String>> {
    let currency = util::non_empty(currency).map(|c| c.to_uppercase());
    if let Some(c) = &currency
        && !(c.len() == 3 && c.chars().all(|ch| ch.is_ascii_alphabetic()))
    {
        return Err(Error::Invalid(format!(
            "currency must be an ISO 4217 code, got {c}"
        )));
    }
    Ok(currency)
}

fn closed_at_for(
    kind: StageKind,
    current: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match kind {
        StageKind::Open => None,
        StageKind::Won | StageKind::Lost => current.or(Some(now)),
    }
}

pub async fn get(executor: impl SqliteExecutor<'_>, id: &str) -> Result<Deal> {
    sqlx::query_as!(
        Deal,
        r#"SELECT d.id, d.title, d.amount, d.currency,
                  s.pipeline_id, d.stage_id, s.kind AS "status: StageKind",
                  d.company_id, d.contact_id,
                  d.expected_close_date AS "expected_close_date: NaiveDate",
                  d.closed_at AS "closed_at: DateTime<Utc>",
                  d.owner_id,
                  d.custom AS "custom: Json<CustomFields>",
                  d.created_at AS "created_at: DateTime<Utc>",
                  d.updated_at AS "updated_at: DateTime<Utc>"
           FROM deals d JOIN stages s ON s.id = d.stage_id
           WHERE d.id = ?"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("deal"))
}

pub async fn list(db: &Db, caller: &Principal, query: DealQuery) -> Result<Page<Deal>> {
    let limit = util::clamp_limit(query.limit);
    let search = match Filter::from_query(query.q) {
        Filter::All => None,
        Filter::Matching(expression) => Some(expression),
        Filter::Nothing => return Ok(Page::empty()),
    };
    let owner_id = users::resolve_me(query.owner_id, caller)?;
    let fetch = limit + 1;
    let rows = sqlx::query_as!(
        Deal,
        r#"SELECT d.id, d.title, d.amount, d.currency,
                  s.pipeline_id, d.stage_id, s.kind AS "status: StageKind",
                  d.company_id, d.contact_id,
                  d.expected_close_date AS "expected_close_date: NaiveDate",
                  d.closed_at AS "closed_at: DateTime<Utc>",
                  d.owner_id,
                  d.custom AS "custom: Json<CustomFields>",
                  d.created_at AS "created_at: DateTime<Utc>",
                  d.updated_at AS "updated_at: DateTime<Utc>"
           FROM deals d JOIN stages s ON s.id = d.stage_id
           WHERE (?1 IS NULL OR d.id IN (
                      SELECT sd.entity_id FROM search_index
                      JOIN search_documents sd ON sd.rowid = search_index.rowid
                      WHERE search_index MATCH ?1))
             AND (?2 IS NULL OR s.pipeline_id = ?2)
             AND (?3 IS NULL OR d.stage_id = ?3)
             AND (?4 IS NULL OR s.kind = ?4)
             AND (?5 IS NULL OR d.company_id = ?5)
             AND (?6 IS NULL OR d.contact_id = ?6)
             AND (?7 IS NULL OR d.owner_id = ?7 OR (?7 = ?8 AND d.owner_id IS NULL))
             AND (?9 IS NULL OR d.id > ?9)
           ORDER BY d.id
           LIMIT ?10"#,
        search,
        query.pipeline_id,
        query.stage_id,
        query.status,
        query.company_id,
        query.contact_id,
        owner_id,
        users::NO_OWNER,
        query.cursor,
        fetch,
    )
    .fetch_all(&db.reader)
    .await?;
    Ok(util::paginate(rows, limit, |d| &d.id))
}

pub async fn create(db: &Db, caller: &Principal, input: NewDeal) -> Result<Deal> {
    let title = util::required(input.title, "title")?;
    let currency = normalize_currency(input.currency)?;
    let now = Utc::now();
    let id = util::new_id("deal");
    let company_id = util::non_empty(input.company_id);
    let contact_id = util::non_empty(input.contact_id);
    let owner_id = users::owner_for_new(input.owner_id, caller)?;
    let custom = Json(input.custom);

    let mut tx = db.writer.begin().await?;
    users::ensure_assignable(&mut tx, owner_id.as_deref()).await?;
    let stage = match util::non_empty(input.stage_id) {
        Some(stage_id) => pipelines::get_stage(&mut *tx, &stage_id).await?,
        None => {
            pipelines::default_stage(&mut *tx, util::non_empty(input.pipeline_id).as_deref())
                .await?
        }
    };
    let closed_at = closed_at_for(stage.kind, None, now);
    sqlx::query!(
        "INSERT INTO deals
             (id, title, amount, currency, stage_id, company_id, contact_id,
              expected_close_date, closed_at, owner_id, custom, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        id,
        title,
        input.amount,
        currency,
        stage.id,
        company_id,
        contact_id,
        input.expected_close_date,
        closed_at,
        owner_id,
        custom,
        now,
        now,
    )
    .execute(&mut *tx)
    .await?;
    let deal = get(&mut *tx, &id).await?;
    events::record(&mut tx, "deal.created", &id, &deal).await?;
    tx.commit().await?;
    Ok(deal)
}

pub async fn update(db: &Db, caller: &Principal, id: &str, patch: DealPatch) -> Result<Deal> {
    let owner_id = users::owner_patch(patch.owner_id, caller)?;
    let now = Utc::now();
    let mut tx = db.writer.begin().await?;
    let mut deal = get(&mut *tx, id).await?;
    let from_stage_id = deal.stage_id.clone();

    if let Some(title) = patch.title {
        deal.title = util::required(title, "title")?;
    }
    util::patch(&mut deal.amount, patch.amount);
    if let Some(currency) = patch.currency {
        deal.currency = normalize_currency(currency)?;
    }
    if let Some(stage_id) = patch.stage_id
        && stage_id != deal.stage_id
    {
        let stage = pipelines::get_stage(&mut *tx, &stage_id).await?;
        deal.closed_at = closed_at_for(stage.kind, deal.closed_at, now);
        deal.pipeline_id = stage.pipeline_id;
        deal.stage_id = stage.id;
        deal.status = stage.kind;
    }
    util::patch(&mut deal.company_id, patch.company_id.map(util::non_empty));
    util::patch(&mut deal.contact_id, patch.contact_id.map(util::non_empty));
    util::patch(&mut deal.expected_close_date, patch.expected_close_date);
    if let Some(owner_id) = owner_id {
        users::ensure_assignable(&mut tx, owner_id.as_deref()).await?;
        deal.owner_id = owner_id;
    }
    if let Some(update) = patch.custom {
        deal.custom.merge(update);
    }
    deal.updated_at = now;

    sqlx::query!(
        "UPDATE deals
         SET title = ?, amount = ?, currency = ?, stage_id = ?, company_id = ?, contact_id = ?,
             expected_close_date = ?, closed_at = ?, owner_id = ?, custom = ?, updated_at = ?
         WHERE id = ?",
        deal.title,
        deal.amount,
        deal.currency,
        deal.stage_id,
        deal.company_id,
        deal.contact_id,
        deal.expected_close_date,
        deal.closed_at,
        deal.owner_id,
        deal.custom,
        deal.updated_at,
        id,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "deal.updated", id, &deal).await?;
    if deal.stage_id != from_stage_id {
        let change = StageChanged {
            deal: &deal,
            from_stage_id: &from_stage_id,
            to_stage_id: &deal.stage_id,
        };
        events::record(&mut tx, "deal.stage_changed", id, &change).await?;
    }
    tx.commit().await?;
    Ok(deal)
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    let mut tx = db.writer.begin().await?;
    let deal = get(&mut *tx, id).await?;
    sqlx::query!("DELETE FROM deals WHERE id = ?", id)
        .execute(&mut *tx)
        .await?;
    events::record(&mut tx, "deal.deleted", id, &deal).await?;
    tx.commit().await?;
    Ok(())
}
