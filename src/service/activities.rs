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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum ActivityKind {
    Note,
    Call,
    Email,
    Meeting,
    Task,
}

/// Something that happened with, or needs doing for, a contact, company or deal.
#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct Activity {
    pub id: String,
    pub kind: ActivityKind,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub contact_id: Option<String>,
    pub company_id: Option<String>,
    pub deal_id: Option<String>,
    /// When it happened (or was logged).
    pub occurred_at: DateTime<Utc>,
    /// For tasks: when it is due.
    pub due_at: Option<DateTime<Utc>>,
    /// For tasks: when it was done. Null means open.
    pub completed_at: Option<DateTime<Utc>>,
    /// User responsible for it; for tasks, the assignee.
    pub owner_id: Option<String>,
    #[schema(value_type = Object)]
    #[schemars(with = "CustomFields")]
    pub custom: Json<CustomFields>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Link to at least one of `contact_id`, `company_id` or `deal_id`.
#[derive(Debug, Clone, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewActivity {
    pub kind: ActivityKind,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub contact_id: Option<String>,
    pub company_id: Option<String>,
    pub deal_id: Option<String>,
    /// Defaults to now.
    pub occurred_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// User ID or `"me"` (for tasks, the assignee). Omit to default to the API
    /// key's user; null for no owner.
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
pub struct ActivityPatch {
    pub kind: Option<ActivityKind>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub subject: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub body: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub contact_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub company_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub deal_id: Option<Option<String>>,
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub due_at: Option<Option<DateTime<Utc>>>,
    /// Set to mark a task done, null to reopen it.
    #[serde(default, deserialize_with = "util::double_option")]
    pub completed_at: Option<Option<DateTime<Utc>>>,
    /// User ID or `"me"`; for tasks, the assignee.
    #[serde(default, deserialize_with = "util::double_option")]
    pub owner_id: Option<Option<String>>,
    /// Merged into existing custom fields; keys set to null are removed.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<CustomFields>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
pub struct ActivityQuery {
    /// Full-text search: every word must match as a prefix, e.g. `budget rob`.
    /// Searches the subject, body and custom field values.
    pub q: Option<String>,
    pub contact_id: Option<String>,
    pub company_id: Option<String>,
    pub deal_id: Option<String>,
    pub kind: Option<ActivityKind>,
    /// true: only incomplete tasks; false: only completed ones.
    pub open: Option<bool>,
    /// A user ID, `me`, or `none` for unassigned. With `open=true`, `owner_id=me`
    /// lists your outstanding tasks.
    pub owner_id: Option<String>,
    /// 1-200, default 50.
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

fn validate(activity: &Activity) -> Result<()> {
    if activity.contact_id.is_none() && activity.company_id.is_none() && activity.deal_id.is_none()
    {
        return Err(Error::Invalid(
            "an activity must link to at least one of contact_id, company_id or deal_id".into(),
        ));
    }
    Ok(())
}

pub async fn get(executor: impl SqliteExecutor<'_>, id: &str) -> Result<Activity> {
    sqlx::query_as!(
        Activity,
        r#"SELECT id, kind AS "kind: ActivityKind", subject, body, contact_id, company_id, deal_id,
                  occurred_at AS "occurred_at: DateTime<Utc>",
                  due_at AS "due_at: DateTime<Utc>",
                  completed_at AS "completed_at: DateTime<Utc>",
                  owner_id,
                  custom AS "custom: Json<CustomFields>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM activities WHERE id = ?"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("activity"))
}

/// Newest first.
pub async fn list(db: &Db, caller: &Principal, query: ActivityQuery) -> Result<Page<Activity>> {
    let limit = util::clamp_limit(query.limit);
    let search = match Filter::from_query(query.q) {
        Filter::All => None,
        Filter::Matching(expression) => Some(expression),
        Filter::Nothing => return Ok(Page::empty()),
    };
    let owner_id = users::resolve_me(query.owner_id, caller)?;
    let fetch = limit + 1;
    let rows = sqlx::query_as!(
        Activity,
        r#"SELECT id, kind AS "kind: ActivityKind", subject, body, contact_id, company_id, deal_id,
                  occurred_at AS "occurred_at: DateTime<Utc>",
                  due_at AS "due_at: DateTime<Utc>",
                  completed_at AS "completed_at: DateTime<Utc>",
                  owner_id,
                  custom AS "custom: Json<CustomFields>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM activities
           WHERE (?1 IS NULL OR id IN (
                      SELECT d.entity_id FROM search_index
                      JOIN search_documents d ON d.rowid = search_index.rowid
                      WHERE search_index MATCH ?1))
             AND (?2 IS NULL OR contact_id = ?2)
             AND (?3 IS NULL OR company_id = ?3)
             AND (?4 IS NULL OR deal_id = ?4)
             AND (?5 IS NULL OR kind = ?5)
             AND (?6 IS NULL OR (?6 = (completed_at IS NULL) AND kind = 'task'))
             AND (?7 IS NULL OR owner_id = ?7 OR (?7 = ?8 AND owner_id IS NULL))
             AND (?9 IS NULL OR id < ?9)
           ORDER BY id DESC
           LIMIT ?10"#,
        search,
        query.contact_id,
        query.company_id,
        query.deal_id,
        query.kind,
        query.open,
        owner_id,
        users::NO_OWNER,
        query.cursor,
        fetch,
    )
    .fetch_all(&db.reader)
    .await?;
    Ok(util::paginate(rows, limit, |a| &a.id))
}

pub async fn create(db: &Db, caller: &Principal, input: NewActivity) -> Result<Activity> {
    let now = Utc::now();
    let mut activity = Activity {
        id: util::new_id("act"),
        kind: input.kind,
        subject: util::non_empty(input.subject),
        body: util::non_empty(input.body),
        contact_id: util::non_empty(input.contact_id),
        company_id: util::non_empty(input.company_id),
        deal_id: util::non_empty(input.deal_id),
        occurred_at: input.occurred_at.unwrap_or(now),
        due_at: input.due_at,
        completed_at: input.completed_at,
        owner_id: users::owner_for_new(input.owner_id, caller)?,
        custom: Json(input.custom),
        created_at: now,
        updated_at: now,
    };
    validate(&activity)?;

    let mut tx = db.writer.begin().await?;
    users::ensure_assignable(&mut tx, activity.owner_id.as_deref()).await?;
    let custom = std::mem::take(&mut activity.custom.0);
    activity.custom = Json(
        field_definitions::validate(
            &mut tx,
            EntityType::Activity,
            custom,
            Write::Create(&activity.id),
        )
        .await?,
    );
    sqlx::query!(
        "INSERT INTO activities
             (id, kind, subject, body, contact_id, company_id, deal_id,
              occurred_at, due_at, completed_at, owner_id, custom, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        activity.id,
        activity.kind,
        activity.subject,
        activity.body,
        activity.contact_id,
        activity.company_id,
        activity.deal_id,
        activity.occurred_at,
        activity.due_at,
        activity.completed_at,
        activity.owner_id,
        activity.custom,
        activity.created_at,
        activity.updated_at,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "activity.created", &activity.id, &activity).await?;
    tx.commit().await?;
    Ok(activity)
}

pub async fn update(
    db: &Db,
    caller: &Principal,
    id: &str,
    patch: ActivityPatch,
) -> Result<Activity> {
    let owner_id = users::owner_patch(patch.owner_id, caller)?;
    let mut tx = db.writer.begin().await?;
    let mut activity = get(&mut *tx, id).await?;
    if let Some(kind) = patch.kind {
        activity.kind = kind;
    }
    util::patch(&mut activity.subject, patch.subject.map(util::non_empty));
    util::patch(&mut activity.body, patch.body.map(util::non_empty));
    util::patch(
        &mut activity.contact_id,
        patch.contact_id.map(util::non_empty),
    );
    util::patch(
        &mut activity.company_id,
        patch.company_id.map(util::non_empty),
    );
    util::patch(&mut activity.deal_id, patch.deal_id.map(util::non_empty));
    if let Some(occurred_at) = patch.occurred_at {
        activity.occurred_at = occurred_at;
    }
    util::patch(&mut activity.due_at, patch.due_at);
    util::patch(&mut activity.completed_at, patch.completed_at);
    if let Some(owner_id) = owner_id {
        users::ensure_assignable(&mut tx, owner_id.as_deref()).await?;
        activity.owner_id = owner_id;
    }
    if let Some(update) = patch.custom {
        let update =
            field_definitions::validate(&mut tx, EntityType::Activity, update, Write::Update(id))
                .await?;
        activity.custom.merge(update);
    }
    activity.updated_at = Utc::now();
    validate(&activity)?;

    sqlx::query!(
        "UPDATE activities
         SET kind = ?, subject = ?, body = ?, contact_id = ?, company_id = ?, deal_id = ?,
             occurred_at = ?, due_at = ?, completed_at = ?, owner_id = ?, custom = ?,
             updated_at = ?
         WHERE id = ?",
        activity.kind,
        activity.subject,
        activity.body,
        activity.contact_id,
        activity.company_id,
        activity.deal_id,
        activity.occurred_at,
        activity.due_at,
        activity.completed_at,
        activity.owner_id,
        activity.custom,
        activity.updated_at,
        id,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "activity.updated", id, &activity).await?;
    tx.commit().await?;
    Ok(activity)
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    let mut tx = db.writer.begin().await?;
    let activity = get(&mut *tx, id).await?;
    sqlx::query!("DELETE FROM activities WHERE id = ?", id)
        .execute(&mut *tx)
        .await?;
    events::record(&mut tx, "activity.deleted", id, &activity).await?;
    tx.commit().await?;
    Ok(())
}
