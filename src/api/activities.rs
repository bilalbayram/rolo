use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::extract::{Body, Id, Params};
use crate::error::{ErrorBody, Result};
use crate::service::Page;
use crate::service::activities::{self, Activity, ActivityPatch, ActivityQuery, NewActivity};
use crate::service::api_keys::Principal;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update, delete))
}

/// List activities
///
/// Newest first. Filter by linked record, kind, or `open=true` for outstanding tasks.
#[utoipa::path(get, path = "/activities", tag = "activities", params(ActivityQuery),
    responses((status = 200, body = Page<Activity>)))]
async fn list(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Params(query): Params<ActivityQuery>,
) -> Result<Json<Page<Activity>>> {
    Ok(Json(activities::list(&state.db, &caller, query).await?))
}

/// Log an activity
#[utoipa::path(post, path = "/activities", tag = "activities", request_body = NewActivity,
    responses((status = 201, body = Activity), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn create(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Body(input): Body<NewActivity>,
) -> Result<(StatusCode, Json<Activity>)> {
    Ok((
        StatusCode::CREATED,
        Json(activities::create(&state.db, &caller, input).await?),
    ))
}

/// Get an activity
#[utoipa::path(get, path = "/activities/{id}", tag = "activities", params(("id" = String, Path)),
    responses((status = 200, body = Activity), (status = 404, body = ErrorBody)))]
async fn get(State(state): State<AppState>, Id(id): Id) -> Result<Json<Activity>> {
    Ok(Json(activities::get(&state.db.reader, &id).await?))
}

/// Update an activity
#[utoipa::path(patch, path = "/activities/{id}", tag = "activities", params(("id" = String, Path)),
    request_body = ActivityPatch,
    responses((status = 200, body = Activity), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn update(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Id(id): Id,
    Body(patch): Body<ActivityPatch>,
) -> Result<Json<Activity>> {
    Ok(Json(
        activities::update(&state.db, &caller, &id, patch).await?,
    ))
}

/// Delete an activity
#[utoipa::path(delete, path = "/activities/{id}", tag = "activities", params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete(State(state): State<AppState>, Id(id): Id) -> Result<StatusCode> {
    activities::delete(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
