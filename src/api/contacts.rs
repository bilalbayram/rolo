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
use crate::service::contacts::{self, Contact, ContactPatch, ContactQuery, NewContact};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update, delete))
}

/// List contacts
#[utoipa::path(get, path = "/contacts", tag = "contacts", params(ContactQuery),
    responses((status = 200, body = Page<Contact>)))]
async fn list(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Params(query): Params<ContactQuery>,
) -> Result<Json<Page<Contact>>> {
    Ok(Json(contacts::list(&state.db, &caller, query).await?))
}

/// Create a contact
#[utoipa::path(post, path = "/contacts", tag = "contacts", request_body = NewContact,
    responses((status = 201, body = Contact), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn create(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Body(input): Body<NewContact>,
) -> Result<(StatusCode, Json<Contact>)> {
    Ok((
        StatusCode::CREATED,
        Json(contacts::create(&state.db, &caller, input).await?),
    ))
}

/// Get a contact
#[utoipa::path(get, path = "/contacts/{id}", tag = "contacts", params(("id" = String, Path)),
    responses((status = 200, body = Contact), (status = 404, body = ErrorBody)))]
async fn get(State(state): State<AppState>, Id(id): Id) -> Result<Json<Contact>> {
    Ok(Json(contacts::get(&state.db.reader, &id).await?))
}

/// Update a contact
#[utoipa::path(patch, path = "/contacts/{id}", tag = "contacts", params(("id" = String, Path)),
    request_body = ContactPatch,
    responses((status = 200, body = Contact), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn update(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Id(id): Id,
    Body(patch): Body<ContactPatch>,
) -> Result<Json<Contact>> {
    Ok(Json(
        contacts::update(&state.db, &caller, &id, patch).await?,
    ))
}

/// Delete a contact
///
/// Deals and activities linked to it are kept and unlinked.
#[utoipa::path(delete, path = "/contacts/{id}", tag = "contacts", params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete(State(state): State<AppState>, Id(id): Id) -> Result<StatusCode> {
    contacts::delete(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
