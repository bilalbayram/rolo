use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::db::Db;
use crate::error::{Error, Result};
use crate::util;

const TOKEN_PREFIX: &str = "rolo_";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum Scope {
    /// Read everything.
    Read,
    /// Read and modify everything.
    Write,
}

#[derive(Debug, Serialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub scope: Scope,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Scope::Read => "read",
            Scope::Write => "write",
        })
    }
}

/// The authenticated caller, attached to each request by the auth middleware.
#[derive(Debug, Clone)]
pub struct Principal {
    pub key_id: String,
    pub scope: Scope,
}

impl Principal {
    pub fn require_write(&self) -> Result<()> {
        match self.scope {
            Scope::Write => Ok(()),
            Scope::Read => Err(Error::Forbidden),
        }
    }
}

fn hash(token: &str) -> String {
    use sha2::Digest as _;
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Returns the stored key and the plaintext token, which is not recoverable later.
pub async fn create(db: &Db, name: &str, scope: Scope) -> Result<(ApiKey, String)> {
    let mut secret = [0u8; 24];
    rand::fill(&mut secret);
    let token = format!("{TOKEN_PREFIX}{}", hex::encode(secret));
    let key = ApiKey {
        id: util::new_id("key"),
        name: name.to_string(),
        prefix: token[..TOKEN_PREFIX.len() + 8].to_string(),
        scope,
        created_at: Utc::now(),
        last_used_at: None,
        revoked_at: None,
    };
    let token_hash = hash(&token);
    sqlx::query!(
        "INSERT INTO api_keys (id, name, prefix, token_hash, scope, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        key.id,
        key.name,
        key.prefix,
        token_hash,
        key.scope,
        key.created_at,
    )
    .execute(&db.writer)
    .await?;
    Ok((key, token))
}

pub async fn list(db: &Db) -> Result<Vec<ApiKey>> {
    Ok(sqlx::query_as!(
        ApiKey,
        r#"SELECT id, name, prefix, scope AS "scope: Scope",
                  created_at AS "created_at: DateTime<Utc>",
                  last_used_at AS "last_used_at: DateTime<Utc>",
                  revoked_at AS "revoked_at: DateTime<Utc>"
           FROM api_keys ORDER BY id"#,
    )
    .fetch_all(&db.reader)
    .await?)
}

pub async fn revoke(db: &Db, id: &str) -> Result<()> {
    let now = Utc::now();
    let result = sqlx::query!(
        "UPDATE api_keys SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
        now,
        id,
    )
    .execute(&db.writer)
    .await?;
    if result.rows_affected() == 0 {
        return Err(Error::NotFound("active api key"));
    }
    Ok(())
}

pub async fn authenticate(db: &Db, token: &str) -> Result<Principal> {
    if !token.starts_with(TOKEN_PREFIX) {
        return Err(Error::Unauthorized);
    }
    let token_hash = hash(token);
    let row = sqlx::query!(
        r#"SELECT id, scope AS "scope: Scope", last_used_at AS "last_used_at: DateTime<Utc>"
           FROM api_keys WHERE token_hash = ? AND revoked_at IS NULL"#,
        token_hash,
    )
    .fetch_optional(&db.reader)
    .await?
    .ok_or(Error::Unauthorized)?;

    // Record usage at most once a minute so reads don't queue on the writer.
    let now = Utc::now();
    if row
        .last_used_at
        .is_none_or(|t| now - t > Duration::minutes(1))
    {
        let writer = db.writer.clone();
        let id = row.id.clone();
        tokio::spawn(async move {
            let _ = sqlx::query!("UPDATE api_keys SET last_used_at = ? WHERE id = ?", now, id)
                .execute(&writer)
                .await;
        });
    }
    Ok(Principal {
        key_id: row.id,
        scope: row.scope,
    })
}
