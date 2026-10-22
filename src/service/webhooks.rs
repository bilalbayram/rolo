use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqliteExecutor;
use sqlx::types::Json;
use utoipa::{IntoParams, ToSchema};

use super::Page;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::util;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Webhook {
    pub id: String,
    pub url: String,
    pub description: Option<String>,
    /// Event type prefixes to deliver, e.g. `["deal.", "contact.created"]`. Empty means all.
    #[schema(value_type = Vec<String>)]
    pub event_types: Json<Vec<String>>,
    /// Disabled webhooks keep their place in the feed; re-enabling delivers what was missed.
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Returned once, on creation.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedWebhook {
    #[serde(flatten)]
    pub webhook: Webhook,
    /// `whsec_...` signing secret (Standard Webhooks). Shown only once.
    pub secret: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewWebhook {
    /// http(s) URL that receives a POST per event.
    pub url: String,
    pub description: Option<String>,
    /// Event type prefixes to deliver, e.g. `["deal.", "contact.created"]`. Empty means all.
    #[serde(default)]
    pub event_types: Vec<String>,
}

/// Omitted fields are left unchanged.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WebhookPatch {
    pub url: Option<String>,
    #[serde(default, deserialize_with = "util::double_option")]
    pub description: Option<Option<String>>,
    pub event_types: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum DeliveryStatus {
    /// Waiting for its first attempt or a retry.
    Pending,
    Succeeded,
    /// Gave up after the maximum number of attempts.
    Failed,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Delivery {
    pub id: String,
    pub webhook_id: String,
    pub event_seq: i64,
    pub event_id: String,
    pub event_type: String,
    pub status: DeliveryStatus,
    pub attempts: i64,
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// HTTP status of the last attempt, if a response was received.
    pub last_status_code: Option<i64>,
    pub last_error: Option<String>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeliveryQuery {
    pub status: Option<DeliveryStatus>,
    /// 1-200, default 50.
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

fn validate_url(url: String) -> Result<String> {
    let url = util::required(url, "url")?;
    let parsed =
        reqwest::Url::parse(&url).map_err(|e| Error::Invalid(format!("invalid url: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(Error::Invalid(
            "url must be an http(s) URL with a host".into(),
        ));
    }
    Ok(url)
}

fn normalize_event_types(types: Vec<String>) -> Vec<String> {
    let mut types: Vec<String> = types
        .into_iter()
        .filter_map(|t| util::non_empty(Some(t)))
        .collect();
    types.sort();
    types.dedup();
    types
}

fn new_secret() -> String {
    use base64::Engine as _;

    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    format!("whsec_{}", BASE64.encode(bytes))
}

pub async fn get(executor: impl SqliteExecutor<'_>, id: &str) -> Result<Webhook> {
    sqlx::query_as!(
        Webhook,
        r#"SELECT id, url, description,
                  event_types AS "event_types: Json<Vec<String>>",
                  enabled AS "enabled: bool",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM webhooks WHERE id = ?"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("webhook"))
}

pub async fn list(db: &Db) -> Result<Vec<Webhook>> {
    Ok(sqlx::query_as!(
        Webhook,
        r#"SELECT id, url, description,
                  event_types AS "event_types: Json<Vec<String>>",
                  enabled AS "enabled: bool",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM webhooks ORDER BY id"#,
    )
    .fetch_all(&db.reader)
    .await?)
}

/// New webhooks receive events that happen after creation, not history.
pub async fn create(db: &Db, input: NewWebhook) -> Result<CreatedWebhook> {
    let now = Utc::now();
    let webhook = Webhook {
        id: util::new_id("whk"),
        url: validate_url(input.url)?,
        description: util::non_empty(input.description),
        event_types: Json(normalize_event_types(input.event_types)),
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let secret = new_secret();
    sqlx::query!(
        "INSERT INTO webhooks (id, url, description, event_types, secret, enabled, cursor_seq, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, 1, (SELECT coalesce(max(seq), 0) FROM events), ?, ?)",
        webhook.id,
        webhook.url,
        webhook.description,
        webhook.event_types,
        secret,
        webhook.created_at,
        webhook.updated_at,
    )
    .execute(&db.writer)
    .await?;
    Ok(CreatedWebhook { webhook, secret })
}

pub async fn update(db: &Db, id: &str, patch: WebhookPatch) -> Result<Webhook> {
    let mut tx = db.writer.begin().await?;
    let mut webhook = get(&mut *tx, id).await?;
    if let Some(url) = patch.url {
        webhook.url = validate_url(url)?;
    }
    util::patch(
        &mut webhook.description,
        patch.description.map(util::non_empty),
    );
    if let Some(types) = patch.event_types {
        webhook.event_types = Json(normalize_event_types(types));
    }
    if let Some(enabled) = patch.enabled {
        webhook.enabled = enabled;
    }
    webhook.updated_at = Utc::now();
    sqlx::query!(
        "UPDATE webhooks SET url = ?, description = ?, event_types = ?, enabled = ?, updated_at = ? WHERE id = ?",
        webhook.url,
        webhook.description,
        webhook.event_types,
        webhook.enabled,
        webhook.updated_at,
        id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(webhook)
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    let result = sqlx::query!("DELETE FROM webhooks WHERE id = ?", id)
        .execute(&db.writer)
        .await?;
    if result.rows_affected() == 0 {
        return Err(Error::NotFound("webhook"));
    }
    Ok(())
}

async fn get_delivery(
    executor: impl SqliteExecutor<'_>,
    webhook_id: &str,
    id: &str,
) -> Result<Delivery> {
    sqlx::query_as!(
        Delivery,
        r#"SELECT d.id, d.webhook_id, d.event_seq, e.id AS event_id, e.type AS event_type,
                  d.status AS "status: DeliveryStatus", d.attempts,
                  d.next_attempt_at AS "next_attempt_at: DateTime<Utc>",
                  d.last_status_code, d.last_error,
                  d.delivered_at AS "delivered_at: DateTime<Utc>",
                  d.created_at AS "created_at: DateTime<Utc>",
                  d.updated_at AS "updated_at: DateTime<Utc>"
           FROM webhook_deliveries d JOIN events e ON e.seq = d.event_seq
           WHERE d.webhook_id = ? AND d.id = ?"#,
        webhook_id,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("delivery"))
}

/// Newest first.
pub async fn list_deliveries(
    db: &Db,
    webhook_id: &str,
    query: DeliveryQuery,
) -> Result<Page<Delivery>> {
    get(&db.reader, webhook_id).await?;
    let limit = util::clamp_limit(query.limit);
    let fetch = limit + 1;
    let rows = sqlx::query_as!(
        Delivery,
        r#"SELECT d.id, d.webhook_id, d.event_seq, e.id AS event_id, e.type AS event_type,
                  d.status AS "status: DeliveryStatus", d.attempts,
                  d.next_attempt_at AS "next_attempt_at: DateTime<Utc>",
                  d.last_status_code, d.last_error,
                  d.delivered_at AS "delivered_at: DateTime<Utc>",
                  d.created_at AS "created_at: DateTime<Utc>",
                  d.updated_at AS "updated_at: DateTime<Utc>"
           FROM webhook_deliveries d JOIN events e ON e.seq = d.event_seq
           WHERE d.webhook_id = ?1
             AND (?2 IS NULL OR d.status = ?2)
             AND (?3 IS NULL OR d.id < ?3)
           ORDER BY d.id DESC
           LIMIT ?4"#,
        webhook_id,
        query.status,
        query.cursor,
        fetch,
    )
    .fetch_all(&db.reader)
    .await?;
    Ok(util::paginate(rows, limit, |d| &d.id))
}

/// Queue a delivery for another attempt right away, whatever its status.
pub async fn retry_delivery(db: &Db, webhook_id: &str, id: &str) -> Result<Delivery> {
    let now = Utc::now();
    let mut tx = db.writer.begin().await?;
    let result = sqlx::query!(
        "UPDATE webhook_deliveries SET status = 'pending', next_attempt_at = ?, updated_at = ?
         WHERE webhook_id = ? AND id = ?",
        now,
        now,
        webhook_id,
        id,
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(Error::NotFound("delivery"));
    }
    let delivery = get_delivery(&mut *tx, webhook_id, id).await?;
    tx.commit().await?;
    Ok(delivery)
}
