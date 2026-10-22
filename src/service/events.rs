use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, types::Json};
use utoipa::{IntoParams, ToSchema};

use crate::{db::Db, error::Result, util};

#[derive(Debug, Serialize, ToSchema, JsonSchema)]
pub struct Event {
    /// Monotonic sequence number; pass the last one you processed as `after`.
    pub seq: i64,
    pub id: String,
    /// e.g. `contact.created`, `deal.stage_changed`, `company.deleted`.
    #[serde(rename = "type")]
    pub event_type: String,
    pub entity_id: String,
    /// Snapshot of the record after the change (before it, for deletes).
    #[schema(value_type = Object)]
    #[schemars(with = "serde_json::Value")]
    pub data: Json<serde_json::Value>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
pub struct EventQuery {
    /// Return events with `seq` greater than this.
    pub after: Option<i64>,
    /// Only events whose type starts with this, e.g. `deal.` or `contact.created`.
    #[serde(rename = "type")]
    #[param(rename = "type")]
    pub type_prefix: Option<String>,
    /// 1-200, default 50.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema, JsonSchema)]
pub struct EventPage {
    pub data: Vec<Event>,
    /// Pass as `after` on the next call. Equal to the input when nothing is new.
    pub next_after: i64,
}

pub async fn record(
    conn: &mut SqliteConnection,
    event_type: &str,
    entity_id: &str,
    data: &impl Serialize,
) -> Result<()> {
    let id = util::new_id("evt");
    let data = serde_json::to_string(data).map_err(anyhow::Error::from)?;
    let now = Utc::now();
    sqlx::query!(
        "INSERT INTO events (id, type, entity_id, data, occurred_at) VALUES (?, ?, ?, ?, ?)",
        id,
        event_type,
        entity_id,
        data,
        now,
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn list(db: &Db, query: EventQuery) -> Result<EventPage> {
    let after = query.after.unwrap_or(0);
    let limit = util::clamp_limit(query.limit);
    let prefix = util::non_empty(query.type_prefix);
    let data = sqlx::query_as!(
        Event,
        r#"SELECT seq, id, type AS event_type, entity_id,
                  data AS "data: Json<serde_json::Value>",
                  occurred_at AS "occurred_at: DateTime<Utc>"
           FROM events
           WHERE seq > ?1 AND (?2 IS NULL OR substr(type, 1, length(?2)) = ?2)
           ORDER BY seq
           LIMIT ?3"#,
        after,
        prefix,
        limit,
    )
    .fetch_all(&db.reader)
    .await?;
    let next_after = data.last().map_or(after, |e| e.seq);
    Ok(EventPage { data, next_after })
}
