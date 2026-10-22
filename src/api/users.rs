use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::extract::{Body, Id, Params};
use crate::error::{Error, ErrorBody, Result};
use crate::service::api_keys::Principal;
use crate::service::users::{self, NewUser, User, UserPatch, UserQuery};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(me))
        .routes(routes!(get, update, delete))
}

/// List users
#[utoipa::path(get, path = "/users", tag = "users", params(UserQuery),
    responses((status = 200, body = Vec<User>)))]
async fn list(
    State(state): State<AppState>,
    Params(query): Params<UserQuery>,
) -> Result<Json<Vec<User>>> {
    Ok(Json(users::list(&state.db, query).await?))
}

/// Create a user
#[utoipa::path(post, path = "/users", tag = "users", request_body = NewUser,
    responses((status = 201, body = User), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn create(
    State(state): State<AppState>,
    Body(input): Body<NewUser>,
) -> Result<(StatusCode, Json<User>)> {
    Ok((
        StatusCode::CREATED,
        Json(users::create(&state.db, input).await?),
    ))
}

/// Get the user this API key belongs to
#[utoipa::path(get, path = "/users/me", tag = "users",
    responses((status = 200, body = User), (status = 404, body = ErrorBody)))]
async fn me(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
) -> Result<Json<User>> {
    let user_id = caller
        .user_id
        .ok_or(Error::NotFound("user for this API key"))?;
    Ok(Json(users::get(&state.db.reader, &user_id).await?))
}

/// Get a user
#[utoipa::path(get, path = "/users/{id}", tag = "users", params(("id" = String, Path)),
    responses((status = 200, body = User), (status = 404, body = ErrorBody)))]
async fn get(State(state): State<AppState>, Id(id): Id) -> Result<Json<User>> {
    Ok(Json(users::get(&state.db.reader, &id).await?))
}

/// Update a user
///
/// Set `deactivated` to true to deactivate: they keep their records but cannot
/// be assigned new ones, and their API keys stop working.
#[utoipa::path(patch, path = "/users/{id}", tag = "users", params(("id" = String, Path)),
    request_body = UserPatch,
    responses((status = 200, body = User), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn update(
    State(state): State<AppState>,
    Id(id): Id,
    Body(patch): Body<UserPatch>,
) -> Result<Json<User>> {
    Ok(Json(users::update(&state.db, &id, patch).await?))
}

/// Delete a user
///
/// Their records become unowned and their API keys are revoked. Prefer
/// deactivating, which keeps ownership.
#[utoipa::path(delete, path = "/users/{id}", tag = "users", params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete(State(state): State<AppState>, Id(id): Id) -> Result<StatusCode> {
    users::delete(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
