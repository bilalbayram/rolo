use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqliteExecutor};
use utoipa::ToSchema;

use super::events;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::util;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum StageKind {
    /// Deal in progress.
    Open,
    /// Terminal: the deal was won.
    Won,
    /// Terminal: the deal was lost.
    Lost,
}

#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct Stage {
    pub id: String,
    pub pipeline_id: String,
    pub name: String,
    pub position: i64,
    pub kind: StageKind,
}

#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct Pipeline {
    pub id: String,
    pub name: String,
    pub position: i64,
    /// Ordered by position.
    pub stages: Vec<Stage>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPipeline {
    pub name: String,
    /// In order. Include at least one `open` stage; deals start in the first one.
    pub stages: Vec<NewStage>,
}

#[derive(Debug, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewStage {
    pub name: String,
    pub kind: StageKind,
}

struct PipelineRow {
    id: String,
    name: String,
    position: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl PipelineRow {
    fn with_stages(self, all: &[Stage]) -> Pipeline {
        Pipeline {
            stages: all
                .iter()
                .filter(|s| s.pipeline_id == self.id)
                .cloned()
                .collect(),
            id: self.id,
            name: self.name,
            position: self.position,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

pub async fn list(db: &Db) -> Result<Vec<Pipeline>> {
    let rows = sqlx::query_as!(
        PipelineRow,
        r#"SELECT id, name, position,
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM pipelines ORDER BY position, id"#,
    )
    .fetch_all(&db.reader)
    .await?;
    let stages = sqlx::query_as!(
        Stage,
        r#"SELECT id, pipeline_id, name, position, kind AS "kind: StageKind"
           FROM stages ORDER BY position, id"#,
    )
    .fetch_all(&db.reader)
    .await?;
    Ok(rows.into_iter().map(|p| p.with_stages(&stages)).collect())
}

pub async fn get(db: &Db, id: &str) -> Result<Pipeline> {
    let mut conn = db.reader.acquire().await?;
    fetch(&mut conn, id).await
}

async fn fetch(conn: &mut SqliteConnection, id: &str) -> Result<Pipeline> {
    let row = sqlx::query_as!(
        PipelineRow,
        r#"SELECT id, name, position,
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM pipelines WHERE id = ?"#,
        id,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::NotFound("pipeline"))?;
    let stages = sqlx::query_as!(
        Stage,
        r#"SELECT id, pipeline_id, name, position, kind AS "kind: StageKind"
           FROM stages WHERE pipeline_id = ? ORDER BY position, id"#,
        id,
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(row.with_stages(&stages))
}

pub async fn get_stage(executor: impl SqliteExecutor<'_>, id: &str) -> Result<Stage> {
    sqlx::query_as!(
        Stage,
        r#"SELECT id, pipeline_id, name, position, kind AS "kind: StageKind"
           FROM stages WHERE id = ?"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| Error::Invalid(format!("unknown stage_id: {id}")))
}

/// The stage new deals land in: the first open stage of the given pipeline,
/// or of the first pipeline when none is given.
pub async fn default_stage(
    executor: impl SqliteExecutor<'_>,
    pipeline_id: Option<&str>,
) -> Result<Stage> {
    sqlx::query_as!(
        Stage,
        r#"SELECT s.id, s.pipeline_id, s.name, s.position, s.kind AS "kind: StageKind"
           FROM stages s JOIN pipelines p ON p.id = s.pipeline_id
           WHERE s.kind = 'open' AND (?1 IS NULL OR p.id = ?1)
           ORDER BY p.position, p.id, s.position, s.id
           LIMIT 1"#,
        pipeline_id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| match pipeline_id {
        Some(id) => Error::Invalid(format!("pipeline {id} does not exist or has no open stage")),
        None => Error::Invalid("no pipeline with an open stage exists".into()),
    })
}

pub async fn create(db: &Db, input: NewPipeline) -> Result<Pipeline> {
    let name = util::required(input.name, "name")?;
    if !input.stages.iter().any(|s| s.kind == StageKind::Open) {
        return Err(Error::Invalid(
            "a pipeline needs at least one open stage".into(),
        ));
    }
    let id = util::new_id("pip");
    let now = Utc::now();

    let mut tx = db.writer.begin().await?;
    sqlx::query!(
        "INSERT INTO pipelines (id, name, position, created_at, updated_at)
         VALUES (?, ?, (SELECT coalesce(max(position) + 1, 0) FROM pipelines), ?, ?)",
        id,
        name,
        now,
        now,
    )
    .execute(&mut *tx)
    .await?;
    let stages = input.stages.into_iter().enumerate();
    for (position, stage) in stages {
        let stage_id = util::new_id("stg");
        let stage_name = util::required(stage.name, "stage name")?;
        let position = position as i64;
        sqlx::query!(
            "INSERT INTO stages (id, pipeline_id, name, position, kind, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            stage_id,
            id,
            stage_name,
            position,
            stage.kind,
            now,
            now,
        )
        .execute(&mut *tx)
        .await?;
    }
    let pipeline = fetch(&mut tx, &id).await?;
    events::record(&mut tx, "pipeline.created", &id, &pipeline).await?;
    tx.commit().await?;
    Ok(pipeline)
}
