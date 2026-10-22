use axum::{Json, extract::State, http::StatusCode};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    AppState,
    api::{Body, Id, Params},
    error::{ErrorBody, Result},
    service::{
        Page,
        companies::{self, Company, CompanyPatch, CompanyQuery, NewCompany},
    },
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update, delete))
}

/// List companies
#[utoipa::path(get, path = "/companies", tag = "companies", params(CompanyQuery),
    responses((status = 200, body = Page<Company>)))]
async fn list(
    State(state): State<AppState>,
    Params(query): Params<CompanyQuery>,
) -> Result<Json<Page<Company>>> {
    Ok(Json(companies::list(&state.db, query).await?))
}

/// Create a company
#[utoipa::path(post, path = "/companies", tag = "companies", request_body = NewCompany,
    responses((status = 201, body = Company), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn create(
    State(state): State<AppState>,
    Body(input): Body<NewCompany>,
) -> Result<(StatusCode, Json<Company>)> {
    Ok((
        StatusCode::CREATED,
        Json(companies::create(&state.db, input).await?),
    ))
}

/// Get a company
#[utoipa::path(get, path = "/companies/{id}", tag = "companies", params(("id" = String, Path)),
    responses((status = 200, body = Company), (status = 404, body = ErrorBody)))]
async fn get(State(state): State<AppState>, Id(id): Id) -> Result<Json<Company>> {
    Ok(Json(companies::get(&state.db.reader, &id).await?))
}

/// Update a company
#[utoipa::path(patch, path = "/companies/{id}", tag = "companies", params(("id" = String, Path)),
    request_body = CompanyPatch,
    responses((status = 200, body = Company), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn update(
    State(state): State<AppState>,
    Id(id): Id,
    Body(patch): Body<CompanyPatch>,
) -> Result<Json<Company>> {
    Ok(Json(companies::update(&state.db, &id, patch).await?))
}

/// Delete a company
///
/// Contacts, deals and activities linked to it are kept and unlinked.
#[utoipa::path(delete, path = "/companies/{id}", tag = "companies", params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete(State(state): State<AppState>, Id(id): Id) -> Result<StatusCode> {
    companies::delete(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
