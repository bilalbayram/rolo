use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::extract::{Body, Id, Params};
use crate::error::{ErrorBody, Result};
use crate::service::Page;
use crate::service::api_keys::Principal;
use crate::service::deals::{self, Deal, DealPatch, DealQuery, NewDeal};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update, delete))
}

/// List deals
///
/// Filter by pipeline, stage, status (open/won/lost), company or contact.
#[utoipa::path(get, path = "/deals", tag = "deals", params(DealQuery),
    responses((status = 200, body = Page<Deal>)))]
async fn list(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Params(query): Params<DealQuery>,
) -> Result<Json<Page<Deal>>> {
    Ok(Json(deals::list(&state.db, &caller, query).await?))
}

/// Create a deal
#[utoipa::path(post, path = "/deals", tag = "deals", request_body = NewDeal,
    responses((status = 201, body = Deal), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn create(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Body(input): Body<NewDeal>,
) -> Result<(StatusCode, Json<Deal>)> {
    Ok((
        StatusCode::CREATED,
        Json(deals::create(&state.db, &caller, input).await?),
    ))
}

/// Get a deal
#[utoipa::path(get, path = "/deals/{id}", tag = "deals", params(("id" = String, Path)),
    responses((status = 200, body = Deal), (status = 404, body = ErrorBody)))]
async fn get(State(state): State<AppState>, Id(id): Id) -> Result<Json<Deal>> {
    Ok(Json(deals::get(&state.db.reader, &id).await?))
}

/// Update a deal
///
/// Move a deal by setting `stage_id`; entering a won/lost stage sets `closed_at`.
#[utoipa::path(patch, path = "/deals/{id}", tag = "deals", params(("id" = String, Path)),
    request_body = DealPatch,
    responses((status = 200, body = Deal), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn update(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Id(id): Id,
    Body(patch): Body<DealPatch>,
) -> Result<Json<Deal>> {
    Ok(Json(deals::update(&state.db, &caller, &id, patch).await?))
}

/// Delete a deal
///
/// Activities linked to it are kept and unlinked.
#[utoipa::path(delete, path = "/deals/{id}", tag = "deals", params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete(State(state): State<AppState>, Id(id): Id) -> Result<StatusCode> {
    deals::delete(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
