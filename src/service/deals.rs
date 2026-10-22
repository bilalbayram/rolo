use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use sqlx::{SqliteConnection, SqliteExecutor};
use utoipa::{IntoParams, ToSchema};

use super::api_keys::Principal;
use super::entity_type::EntityType;
use super::field_definitions::{self, Write};
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
    /// For imports: when the record was originally created, as an RFC 3339
    /// timestamp that isn't in the future. `updated_at` gets the same value.
    /// Defaults to now. A deal created in a won or lost stage is closed then too.
    pub created_at: Option<DateTime<Utc>>,
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

/// A deal to create, or to merge into the existing deal it matches.
#[derive(Debug, Clone, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DealUpsert {
    /// Required when no deal matches.
    pub title: Option<String>,
    /// In minor currency units, e.g. cents.
    pub amount: Option<i64>,
    /// ISO 4217 code, e.g. `USD`.
    pub currency: Option<String>,
    /// On create, defaults to the first open stage of `pipeline_id`. On update,
    /// moving to another stage follows the PATCH rules.
    pub stage_id: Option<String>,
    /// Used only when creating without `stage_id`. Defaults to the first pipeline.
    pub pipeline_id: Option<String>,
    pub company_id: Option<String>,
    pub contact_id: Option<String>,
    pub expected_close_date: Option<NaiveDate>,
    /// User ID or `"me"`. On create, omit to default to the API key's user, or
    /// null for no owner. On update, null leaves the owner unchanged.
    #[serde(default, deserialize_with = "util::double_option")]
    pub owner_id: Option<Option<String>>,
    /// On update, merged into existing custom fields; keys set to null are removed.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: CustomFields,
    /// For imports: when the record was originally created, as an RFC 3339
    /// timestamp that isn't in the future. `updated_at` gets the same value.
    /// Defaults to now. Used only when creating.
    pub created_at: Option<DateTime<Utc>>,
    /// How to find the existing deal: `custom.<key>` for unique custom fields,
    /// tried in order. The first key with a value in `custom` that matches a
    /// deal wins.
    #[schema(value_type = Vec<String>)]
    #[schemars(with = "Vec<String>")]
    pub match_on: Vec<DealMatchKey>,
}

/// A unique custom field [`upsert`] can find an existing deal by, written
/// `custom.<key>`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct DealMatchKey(String);

impl TryFrom<String> for DealMatchKey {
    type Error = String;

    fn try_from(key: String) -> std::result::Result<Self, Self::Error> {
        key.strip_prefix("custom.")
            .filter(|custom_key| !custom_key.is_empty())
            .map(|custom_key| DealMatchKey(custom_key.to_string()))
            .ok_or_else(|| format!("cannot match deals on {key:?}; expected custom.<key>"))
    }
}

/// The result of [`upsert`].
#[derive(Debug, Serialize, ToSchema, JsonSchema)]
pub struct UpsertedDeal {
    pub deal: Deal,
    /// True if a new deal was created; false if an existing one was updated.
    pub created: bool,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
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
    /// Only deals expected to close on or after this date, e.g. `2026-11-01`.
    pub expected_close_from: Option<NaiveDate>,
    /// Only deals expected to close on or before this date, e.g. `2026-11-30`.
    pub expected_close_to: Option<NaiveDate>,
    /// Comma-separated IDs to fetch, at most 100, e.g. `deal_a,deal_b`. Matches
    /// come back in the order given, in one page; unknown IDs are left out.
    /// Can't be combined with `limit`, `cursor` or `sort`.
    pub ids: Option<String>,
    /// Order: `id` (default, oldest first), `-id` (newest first),
    /// `expected_close_date` (soonest first) or `-expected_close_date` (latest
    /// first). In the date orders, deals without an expected close date come last.
    pub sort: Option<DealSort>,
    /// Exact match on defined custom fields, e.g. `{"event_type": "dugun_nisan"}`.
    /// Over REST, pass each as a `custom.<key>=<value>` query parameter.
    #[param(ignore)]
    pub custom: Option<CustomFields>,
    /// 1-200, default 50.
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page, with the same `sort`.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema, JsonSchema)]
pub enum DealSort {
    /// Oldest first.
    #[default]
    #[serde(rename = "id")]
    Id,
    /// Newest first.
    #[serde(rename = "-id")]
    IdDescending,
    /// Soonest expected close date first, then oldest first.
    #[serde(rename = "expected_close_date")]
    ExpectedCloseDate,
    /// Latest expected close date first, then newest first.
    #[serde(rename = "-expected_close_date")]
    ExpectedCloseDateDescending,
}

impl DealSort {
    /// The order the list query sorts in.
    fn order(self) -> &'static str {
        match self {
            DealSort::Id => "id",
            DealSort::IdDescending => "-id",
            DealSort::ExpectedCloseDate => "date",
            DealSort::ExpectedCloseDateDescending => "-date",
        }
    }

    fn by_date(self) -> bool {
        matches!(
            self,
            DealSort::ExpectedCloseDate | DealSort::ExpectedCloseDateDescending
        )
    }

    /// The sort key the list query uses for a deal without an expected close
    /// date, chosen so that such deals come last.
    fn undated_key(self) -> &'static str {
        match self {
            DealSort::ExpectedCloseDateDescending => "",
            DealSort::Id | DealSort::IdDescending | DealSort::ExpectedCloseDate => "~",
        }
    }
}

/// Word in a date cursor standing for "no expected close date".
const UNDATED: &str = "none";

/// Cursor after `deal` for a sort by expected close date: `<date>/<id>`, or
/// `none/<id>` for a deal without a date.
fn date_cursor(deal: &Deal) -> String {
    let date = deal
        .expected_close_date
        .map_or_else(|| UNDATED.to_string(), |date| date.to_string());
    format!("{date}/{}", deal.id)
}

/// The sort key and ID from a [`date_cursor`].
fn parse_date_cursor(cursor: &str, sort: DealSort) -> Result<(String, String)> {
    let invalid = || {
        Error::Invalid(format!(
            "invalid cursor for sort by expected close date: {cursor}"
        ))
    };
    let (date, id) = cursor.split_once('/').ok_or_else(invalid)?;
    let key = if date == UNDATED {
        sort.undated_key().to_string()
    } else {
        NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|_| invalid())?
            .to_string()
    };
    Ok((key, id.to_string()))
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
    let ids = util::IdList::parse(query.ids)?;
    let limit = match &ids {
        Some(_) if query.limit.is_some() || query.cursor.is_some() || query.sort.is_some() => {
            return Err(util::IdList::exclusive());
        }
        Some(ids) => ids.count(),
        None => util::clamp_limit(query.limit),
    };
    let custom = field_definitions::list_filters(db, EntityType::Deal, query.custom).await?;
    let search = match Filter::from_query(query.q) {
        Filter::All => None,
        Filter::Matching(expression) => Some(expression),
        Filter::Nothing => return Ok(Page::empty()),
    };
    let owner_id = users::resolve_me(query.owner_id, caller)?;
    let sort = query.sort.unwrap_or_default();
    let (cursor_key, cursor_id) = match query.cursor {
        None => (None, None),
        Some(cursor) if sort.by_date() => {
            let (key, id) = parse_date_cursor(&cursor, sort)?;
            (Some(key), Some(id))
        }
        Some(cursor) => (None, Some(cursor)),
    };
    let order = sort.order();
    let ids_json = ids.as_ref().map(util::IdList::to_json);
    let fetch = limit + 1;
    // Sorting by date uses the key coalesce(expected_close_date, undated key)
    // and breaks ties by ID in the same direction; the cursor holds both.
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
             AND (?9 IS NULL OR d.expected_close_date >= ?9)
             AND (?10 IS NULL OR d.expected_close_date <= ?10)
             AND (?11 IS NULL
                  OR (?12 = 'id' AND d.id > ?11)
                  OR (?12 = '-id' AND d.id < ?11)
                  OR (?12 = 'date'
                      AND (coalesce(d.expected_close_date, ?13), d.id) > (?14, ?11))
                  OR (?12 = '-date'
                      AND (coalesce(d.expected_close_date, ?13), d.id) < (?14, ?11)))
             AND (?15 IS NULL OR NOT EXISTS (
                      SELECT 1 FROM json_each(?15) filter
                      WHERE json_extract(d.custom, '$.' || filter.key) IS NOT filter.value))
             AND (?16 IS NULL OR d.id IN (SELECT value FROM json_each(?16)))
           ORDER BY CASE WHEN ?12 = 'date' THEN coalesce(d.expected_close_date, ?13) END ASC,
                    CASE WHEN ?12 = '-date' THEN coalesce(d.expected_close_date, ?13) END DESC,
                    CASE WHEN ?12 IN ('-id', '-date') THEN d.id END DESC,
                    d.id ASC
           LIMIT ?17"#,
        search,
        query.pipeline_id,
        query.stage_id,
        query.status,
        query.company_id,
        query.contact_id,
        owner_id,
        users::NO_OWNER,
        query.expected_close_from,
        query.expected_close_to,
        cursor_id,
        order,
        sort.undated_key(),
        cursor_key,
        custom,
        ids_json,
        fetch,
    )
    .fetch_all(&db.reader)
    .await?;
    let mut page = util::paginate(rows, limit, |d| &d.id);
    if sort.by_date()
        && page.next_cursor.is_some()
        && let Some(last) = page.data.last()
    {
        page.next_cursor = Some(date_cursor(last));
    }
    if let Some(ids) = &ids {
        page.data = ids.in_order(page.data, |d| &d.id);
    }
    Ok(page)
}

pub async fn create(db: &Db, caller: &Principal, input: NewDeal) -> Result<Deal> {
    let mut tx = db.writer.begin().await?;
    let deal = insert(&mut tx, caller, input).await?;
    tx.commit().await?;
    Ok(deal)
}

async fn insert(tx: &mut SqliteConnection, caller: &Principal, input: NewDeal) -> Result<Deal> {
    let title = util::required(input.title, "title")?;
    let currency = normalize_currency(input.currency)?;
    // An imported deal is created, and if its stage is won or lost closed, then.
    let (id, now) = util::new_record("deal", input.created_at)?;
    let company_id = util::non_empty(input.company_id);
    let contact_id = util::non_empty(input.contact_id);
    let owner_id = users::owner_for_new(input.owner_id, caller)?;

    users::ensure_assignable(tx, owner_id.as_deref()).await?;
    let custom =
        field_definitions::validate(tx, EntityType::Deal, input.custom, Write::Create(&id)).await?;
    let custom = Json(custom);
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
    events::record(tx, "deal.created", &id, &deal).await?;
    Ok(deal)
}

pub async fn update(db: &Db, caller: &Principal, id: &str, patch: DealPatch) -> Result<Deal> {
    let mut tx = db.writer.begin().await?;
    let deal = apply(&mut tx, caller, id, patch).await?;
    tx.commit().await?;
    Ok(deal)
}

async fn apply(
    tx: &mut SqliteConnection,
    caller: &Principal,
    id: &str,
    patch: DealPatch,
) -> Result<Deal> {
    let owner_id = users::owner_patch(patch.owner_id, caller)?;
    let now = Utc::now();
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
        users::ensure_assignable(tx, owner_id.as_deref()).await?;
        deal.owner_id = owner_id;
    }
    if let Some(update) = patch.custom {
        let update =
            field_definitions::validate(tx, EntityType::Deal, update, Write::Update(id)).await?;
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
    events::record(tx, "deal.updated", id, &deal).await?;
    if deal.stage_id != from_stage_id {
        let change = StageChanged {
            deal: &deal,
            from_stage_id: &from_stage_id,
            to_stage_id: &deal.stage_id,
        };
        events::record(tx, "deal.stage_changed", id, &change).await?;
    }
    Ok(deal)
}

/// Update the first existing deal that `input.match_on` finds, changing only
/// the fields `input` sets, or create the deal if none matches. Moving stages,
/// owners and custom fields follow the same rules as PATCH.
pub async fn upsert(db: &Db, caller: &Principal, input: DealUpsert) -> Result<UpsertedDeal> {
    if input.match_on.is_empty() {
        return Err(Error::Invalid("match_on must list at least one key".into()));
    }
    let mut tx = db.writer.begin().await?;
    let upserted = match find_match(&mut tx, &input).await? {
        Some(id) => {
            let patch = DealPatch {
                title: util::non_empty(input.title),
                amount: input.amount.map(Some),
                currency: util::non_empty(input.currency).map(Some),
                stage_id: util::non_empty(input.stage_id),
                company_id: util::non_empty(input.company_id).map(Some),
                contact_id: util::non_empty(input.contact_id).map(Some),
                expected_close_date: input.expected_close_date.map(Some),
                owner_id: input.owner_id.flatten().map(Some),
                custom: Some(input.custom),
            };
            UpsertedDeal {
                deal: apply(&mut tx, caller, &id, patch).await?,
                created: false,
            }
        }
        None => {
            let title = input.title.ok_or_else(|| {
                Error::Invalid("title is required to create a deal; no deal matched".into())
            })?;
            let new = NewDeal {
                title,
                amount: input.amount,
                currency: input.currency,
                stage_id: input.stage_id,
                pipeline_id: input.pipeline_id,
                company_id: input.company_id,
                contact_id: input.contact_id,
                expected_close_date: input.expected_close_date,
                owner_id: input.owner_id,
                custom: input.custom,
                created_at: input.created_at,
            };
            UpsertedDeal {
                deal: insert(&mut tx, caller, new).await?,
                created: true,
            }
        }
    };
    tx.commit().await?;
    Ok(upserted)
}

/// The first deal whose unique custom field in `input.match_on` has the value
/// `input.custom` gives it.
async fn find_match(conn: &mut SqliteConnection, input: &DealUpsert) -> Result<Option<String>> {
    // Check every key up front, so a bad key fails even without a value.
    let mut definitions = Vec::new();
    for DealMatchKey(key) in &input.match_on {
        definitions.push(field_definitions::for_matching(conn, EntityType::Deal, key).await?);
    }
    for definition in &definitions {
        let Some(value) = input
            .custom
            .get(&definition.key)
            .filter(|value| !value.is_null())
        else {
            continue;
        };
        if let Some(id) = field_definitions::find_by_unique_value(conn, definition, value).await? {
            return Ok(Some(id));
        }
    }
    Ok(None)
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
