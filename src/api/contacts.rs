use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::extract::{Body, FilterParams, Id};
use crate::error::{ErrorBody, Result};
use crate::service::Page;
use crate::service::api_keys::Principal;
use crate::service::contacts::{
    self, Contact, ContactPatch, ContactQuery, ContactUpsert, NewContact, UpsertedContact,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(upsert))
        .routes(routes!(get, update, delete))
}

/// List contacts
///
/// Filter on defined custom fields with `custom.<key>=<value>` parameters, e.g.
/// `custom.source=instagram`: exact matches on the normalized value. For
/// number, boolean, date, datetime and select fields, a comma-separated list
/// matches any of its values. `has` and `missing` filter on whether custom
/// fields are set, e.g. `missing=custom.instagram_id`. `sum=custom.<key>` adds
/// up a number field over all matches.
#[utoipa::path(get, path = "/contacts", tag = "contacts", params(ContactQuery),
    responses((status = 200, body = Page<Contact>)))]
async fn list(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    FilterParams(mut query, custom): FilterParams<ContactQuery>,
) -> Result<Json<Page<Contact>>> {
    query.custom = custom;
    Ok(Json(
        contacts::list(&state.db, &caller, state.default_region, query).await?,
    ))
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
        Json(contacts::create(&state.db, &caller, state.default_region, input).await?),
    ))
}

/// Create or update a contact
///
/// Finds an existing contact by the keys in `match_on` (default email, then
/// phone; `custom.<key>` needs a unique custom field). If one matches, only the
/// fields this body sets to a non-blank value are changed and `custom` is
/// merged; otherwise the contact is created. Returns 200 with `created: false`
/// after an update, 201 with `created: true` after a create.
#[utoipa::path(post, path = "/contacts/upsert", tag = "contacts", request_body = ContactUpsert,
    responses((status = 200, body = UpsertedContact), (status = 201, body = UpsertedContact), (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn upsert(
    State(state): State<AppState>,
    Extension(caller): Extension<Principal>,
    Body(input): Body<ContactUpsert>,
) -> Result<(StatusCode, Json<UpsertedContact>)> {
    let upserted = contacts::upsert(&state.db, &caller, state.default_region, input).await?;
    let status = if upserted.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(upserted)))
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
        contacts::update(&state.db, &caller, state.default_region, &id, patch).await?,
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
