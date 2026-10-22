//! Stored responses for requests sent with an `Idempotency-Key`, so retries
//! replay the first response instead of repeating a write.

use chrono::{Duration, Utc};

use crate::db::Db;
use crate::error::{Error, Result};

/// How long a key and its response are kept.
pub const RETENTION: Duration = Duration::hours(24);

const MAX_KEY_LENGTH: usize = 255;

/// A request made with an idempotency key.
pub struct Attempt<'a> {
    /// The API key the request was made with; idempotency keys are scoped to it.
    pub api_key_id: &'a str,
    pub key: &'a str,
    /// Method, path and query, e.g. `POST /v1/contacts`.
    pub request: &'a str,
    /// Hex SHA-256 of the request body.
    pub body_hash: &'a str,
}

/// A response kept for replay.
pub struct StoredResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// What to do with an [`Attempt`].
pub enum Outcome {
    /// The key is new: run the request, then [`finish`] or [`abandon`] it.
    Run,
    /// The same request already ran with this key: send its response again.
    Replay(StoredResponse),
}

/// Claim `attempt.key`, or find the response already stored for it. A key
/// reused for a different request is `Invalid`; a key whose first request is
/// still running is a `Conflict`. Deletes expired keys first.
pub async fn begin(db: &Db, attempt: &Attempt<'_>) -> Result<Outcome> {
    let key = attempt.key;
    if key.is_empty() || key.len() > MAX_KEY_LENGTH {
        return Err(Error::Invalid(format!(
            "Idempotency-Key must be 1 to {MAX_KEY_LENGTH} characters"
        )));
    }
    let now = Utc::now();
    let expired_before = now - RETENTION;
    let mut tx = db.writer.begin().await?;
    sqlx::query!(
        "DELETE FROM idempotency_keys WHERE created_at < ?",
        expired_before
    )
    .execute(&mut *tx)
    .await?;
    let stored = sqlx::query!(
        r#"SELECT request, body_hash, status AS "status: u16", content_type, body
           FROM idempotency_keys WHERE api_key_id = ? AND key = ?"#,
        attempt.api_key_id,
        key,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(stored) = stored else {
        sqlx::query!(
            "INSERT INTO idempotency_keys (api_key_id, key, request, body_hash, created_at)
             VALUES (?, ?, ?, ?, ?)",
            attempt.api_key_id,
            key,
            attempt.request,
            attempt.body_hash,
            now,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Outcome::Run);
    };
    if stored.request != attempt.request || stored.body_hash != attempt.body_hash {
        return Err(Error::Invalid(
            "this Idempotency-Key was already used for a different request".into(),
        ));
    }
    match stored.status {
        Some(status) => Ok(Outcome::Replay(StoredResponse {
            status,
            content_type: stored.content_type,
            body: stored.body.unwrap_or_default(),
        })),
        None => Err(Error::Conflict(
            "a request with this Idempotency-Key is still in progress".into(),
        )),
    }
}

/// Store the response to a request [`begin`] let run.
pub async fn finish(db: &Db, attempt: &Attempt<'_>, response: &StoredResponse) -> Result<()> {
    sqlx::query!(
        "UPDATE idempotency_keys SET status = ?, content_type = ?, body = ?
         WHERE api_key_id = ? AND key = ?",
        response.status,
        response.content_type,
        response.body,
        attempt.api_key_id,
        attempt.key,
    )
    .execute(&db.writer)
    .await?;
    Ok(())
}

/// Release the key of a request that failed without a response worth keeping,
/// so that a retry runs it again.
pub async fn abandon(db: &Db, attempt: &Attempt<'_>) -> Result<()> {
    sqlx::query!(
        "DELETE FROM idempotency_keys WHERE api_key_id = ? AND key = ?",
        attempt.api_key_id,
        attempt.key,
    )
    .execute(&db.writer)
    .await?;
    Ok(())
}
