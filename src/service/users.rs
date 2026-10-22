use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqliteExecutor};
use utoipa::{IntoParams, ToSchema};

use super::api_keys::Principal;
use super::events;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::util;

/// Owner value meaning "the user the API key belongs to".
const ME: &str = "me";

/// Owner filter value matching records without an owner.
pub const NO_OWNER: &str = "none";

#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct User {
    pub id: String,
    pub name: String,
    /// Lowercased; unique across users.
    pub email: String,
    /// Deactivated users keep their records but cannot be assigned new ones,
    /// and their API keys stop working.
    pub deactivated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewUser {
    pub name: String,
    /// Must be unique.
    pub email: String,
}

/// Omitted fields are left unchanged.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserPatch {
    pub name: Option<String>,
    pub email: Option<String>,
    /// true deactivates the user; false reactivates them.
    pub deactivated: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams, JsonSchema)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct UserQuery {
    /// Include deactivated users (default false).
    #[serde(default)]
    pub include_deactivated: bool,
}

fn normalize_email(email: String) -> Result<String> {
    let email = util::required(email, "email")?.to_lowercase();
    if !email.contains('@') {
        return Err(Error::Invalid(format!("invalid email: {email}")));
    }
    Ok(email)
}

pub async fn get(executor: impl SqliteExecutor<'_>, id: &str) -> Result<User> {
    sqlx::query_as!(
        User,
        r#"SELECT id, name, email,
                  deactivated_at AS "deactivated_at: DateTime<Utc>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM users WHERE id = ?"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or(Error::NotFound("user"))
}

/// Ordered by name.
pub async fn list(db: &Db, query: UserQuery) -> Result<Vec<User>> {
    Ok(sqlx::query_as!(
        User,
        r#"SELECT id, name, email,
                  deactivated_at AS "deactivated_at: DateTime<Utc>",
                  created_at AS "created_at: DateTime<Utc>",
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM users
           WHERE ?1 OR deactivated_at IS NULL
           ORDER BY name COLLATE NOCASE, id"#,
        query.include_deactivated,
    )
    .fetch_all(&db.reader)
    .await?)
}

pub async fn create(db: &Db, input: NewUser) -> Result<User> {
    let now = Utc::now();
    let user = User {
        id: util::new_id("usr"),
        name: util::required(input.name, "name")?,
        email: normalize_email(input.email)?,
        deactivated_at: None,
        created_at: now,
        updated_at: now,
    };
    let mut tx = db.writer.begin().await?;
    sqlx::query!(
        "INSERT INTO users (id, name, email, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        user.id,
        user.name,
        user.email,
        user.created_at,
        user.updated_at,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "user.created", &user.id, &user).await?;
    tx.commit().await?;
    Ok(user)
}

pub async fn update(db: &Db, id: &str, patch: UserPatch) -> Result<User> {
    let now = Utc::now();
    let mut tx = db.writer.begin().await?;
    let mut user = get(&mut *tx, id).await?;
    if let Some(name) = patch.name {
        user.name = util::required(name, "name")?;
    }
    if let Some(email) = patch.email {
        user.email = normalize_email(email)?;
    }
    match patch.deactivated {
        Some(true) if user.deactivated_at.is_none() => user.deactivated_at = Some(now),
        Some(false) => user.deactivated_at = None,
        _ => {}
    }
    user.updated_at = now;
    sqlx::query!(
        "UPDATE users SET name = ?, email = ?, deactivated_at = ?, updated_at = ? WHERE id = ?",
        user.name,
        user.email,
        user.deactivated_at,
        user.updated_at,
        id,
    )
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, "user.updated", id, &user).await?;
    tx.commit().await?;
    Ok(user)
}

/// Records they owned become unowned and their API keys are revoked. Prefer
/// deactivating, which keeps ownership history.
pub async fn delete(db: &Db, id: &str) -> Result<()> {
    let now = Utc::now();
    let mut tx = db.writer.begin().await?;
    let user = get(&mut *tx, id).await?;
    sqlx::query!(
        "UPDATE api_keys SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
        now,
        id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM users WHERE id = ?", id)
        .execute(&mut *tx)
        .await?;
    events::record(&mut tx, "user.deleted", id, &user).await?;
    tx.commit().await?;
    Ok(())
}

/// Check that a record may be assigned to `owner_id`.
pub async fn ensure_assignable(conn: &mut SqliteConnection, owner_id: Option<&str>) -> Result<()> {
    let Some(owner_id) = owner_id else {
        return Ok(());
    };
    let user = get(&mut *conn, owner_id).await.map_err(|err| match err {
        Error::NotFound(_) => Error::Invalid(format!("unknown user: {owner_id}")),
        other => other,
    })?;
    if user.deactivated_at.is_some() {
        return Err(Error::Invalid(format!("user {owner_id} is deactivated")));
    }
    Ok(())
}

/// Replace `"me"` with the caller's user ID.
pub fn resolve_me(owner_id: Option<String>, caller: &Principal) -> Result<Option<String>> {
    match owner_id.as_deref() {
        Some(ME) => {
            caller.user_id.clone().map(Some).ok_or_else(|| {
                Error::Invalid("\"me\" needs an API key that belongs to a user".into())
            })
        }
        _ => Ok(owner_id),
    }
}

/// Owner for a new record: omitted means the caller's user (none if the API
/// key has no user), null means no owner, and `"me"` is resolved.
pub fn owner_for_new(
    requested: Option<Option<String>>,
    caller: &Principal,
) -> Result<Option<String>> {
    match requested {
        None => Ok(caller.user_id.clone()),
        Some(owner_id) => resolve_me(owner_id, caller),
    }
}

/// Owner change from a PATCH body, with `"me"` resolved.
pub fn owner_patch(
    requested: Option<Option<String>>,
    caller: &Principal,
) -> Result<Option<Option<String>>> {
    requested
        .map(|owner_id| resolve_me(owner_id, caller))
        .transpose()
}
