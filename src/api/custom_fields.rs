use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::extract::{Body, Id, Params};
use crate::error::{ErrorBody, Result};
use crate::service::field_definitions::{
    self, FieldDefinition, FieldDefinitionPatch, FieldDefinitionQuery, NewFieldDefinition,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update, delete))
}

/// List custom field definitions
#[utoipa::path(get, path = "/custom-fields", tag = "custom fields", params(FieldDefinitionQuery),
    responses((status = 200, body = Vec<FieldDefinition>)))]
async fn list(
    State(state): State<AppState>,
    Params(query): Params<FieldDefinitionQuery>,
) -> Result<Json<Vec<FieldDefinition>>> {
    Ok(Json(field_definitions::list(&state.db, query).await?))
}

/// Define a custom field
///
/// From now on, values for `key` in `custom` on records of `entity_type` are
/// validated against `field_type` (and normalized, e.g. emails lowercased),
/// and `required` fields must be set when a record is created. Existing records
/// are not revalidated.
#[utoipa::path(post, path = "/custom-fields", tag = "custom fields", request_body = NewFieldDefinition,
    responses((status = 201, body = FieldDefinition), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn create(
    State(state): State<AppState>,
    Body(input): Body<NewFieldDefinition>,
) -> Result<(StatusCode, Json<FieldDefinition>)> {
    Ok((
        StatusCode::CREATED,
        Json(field_definitions::create(&state.db, input).await?),
    ))
}

/// Get a custom field definition
#[utoipa::path(get, path = "/custom-fields/{id}", tag = "custom fields", params(("id" = String, Path)),
    responses((status = 200, body = FieldDefinition), (status = 404, body = ErrorBody)))]
async fn get(State(state): State<AppState>, Id(id): Id) -> Result<Json<FieldDefinition>> {
    Ok(Json(field_definitions::get(&state.db.reader, &id).await?))
}

/// Update a custom field definition
#[utoipa::path(patch, path = "/custom-fields/{id}", tag = "custom fields", params(("id" = String, Path)),
    request_body = FieldDefinitionPatch,
    responses((status = 200, body = FieldDefinition), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn update(
    State(state): State<AppState>,
    Id(id): Id,
    Body(patch): Body<FieldDefinitionPatch>,
) -> Result<Json<FieldDefinition>> {
    Ok(Json(
        field_definitions::update(&state.db, &id, patch).await?,
    ))
}

/// Delete a custom field definition
///
/// Existing values are kept and become free-form.
#[utoipa::path(delete, path = "/custom-fields/{id}", tag = "custom fields", params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete(State(state): State<AppState>, Id(id): Id) -> Result<StatusCode> {
    field_definitions::delete(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
