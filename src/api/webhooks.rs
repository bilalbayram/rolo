use axum::Json;
use axum::extract::{FromRequestParts, State};
use axum::http::StatusCode;
use serde::Deserialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::extract::{Body, Id, Params};
use crate::error::{Error, ErrorBody, Result};
use crate::service::Page;
use crate::service::webhooks::{
    self, CreatedWebhook, Delivery, DeliveryQuery, NewWebhook, Webhook, WebhookPatch,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update, delete))
        .routes(routes!(list_deliveries))
        .routes(routes!(retry_delivery))
}

#[derive(Deserialize, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(Error))]
struct DeliveryPath {
    id: String,
    delivery_id: String,
}

/// List webhooks
#[utoipa::path(get, path = "/webhooks", tag = "webhooks",
    responses((status = 200, body = Vec<Webhook>)))]
async fn list(State(state): State<AppState>) -> Result<Json<Vec<Webhook>>> {
    Ok(Json(webhooks::list(&state.db).await?))
}

/// Create a webhook
///
/// Receives events that happen after creation. The response includes the
/// signing secret, which is not shown again. Requests are signed per Standard
/// Webhooks: verify `webhook-signature` against `webhook-id`,
/// `webhook-timestamp` and the raw body.
#[utoipa::path(post, path = "/webhooks", tag = "webhooks", request_body = NewWebhook,
    responses((status = 201, body = CreatedWebhook), (status = 422, body = ErrorBody)))]
async fn create(
    State(state): State<AppState>,
    Body(input): Body<NewWebhook>,
) -> Result<(StatusCode, Json<CreatedWebhook>)> {
    Ok((
        StatusCode::CREATED,
        Json(webhooks::create(&state.db, input).await?),
    ))
}

/// Get a webhook
#[utoipa::path(get, path = "/webhooks/{id}", tag = "webhooks", params(("id" = String, Path)),
    responses((status = 200, body = Webhook), (status = 404, body = ErrorBody)))]
async fn get(State(state): State<AppState>, Id(id): Id) -> Result<Json<Webhook>> {
    Ok(Json(webhooks::get(&state.db.reader, &id).await?))
}

/// Update a webhook
///
/// Set `enabled` to false to pause deliveries; re-enabling delivers the events
/// that happened in the meantime.
#[utoipa::path(patch, path = "/webhooks/{id}", tag = "webhooks", params(("id" = String, Path)),
    request_body = WebhookPatch,
    responses((status = 200, body = Webhook), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn update(
    State(state): State<AppState>,
    Id(id): Id,
    Body(patch): Body<WebhookPatch>,
) -> Result<Json<Webhook>> {
    Ok(Json(webhooks::update(&state.db, &id, patch).await?))
}

/// Delete a webhook and its delivery history
#[utoipa::path(delete, path = "/webhooks/{id}", tag = "webhooks", params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete(State(state): State<AppState>, Id(id): Id) -> Result<StatusCode> {
    webhooks::delete(&state.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// List deliveries
///
/// Newest first. Failed attempts are retried with backoff for about 21 hours
/// (8 attempts) before the delivery is marked `failed`.
#[utoipa::path(get, path = "/webhooks/{id}/deliveries", tag = "webhooks",
    params(("id" = String, Path), DeliveryQuery),
    responses((status = 200, body = Page<Delivery>), (status = 404, body = ErrorBody)))]
async fn list_deliveries(
    State(state): State<AppState>,
    Id(id): Id,
    Params(query): Params<DeliveryQuery>,
) -> Result<Json<Page<Delivery>>> {
    Ok(Json(
        webhooks::list_deliveries(&state.db, &id, query).await?,
    ))
}

/// Retry a delivery now
#[utoipa::path(post, path = "/webhooks/{id}/deliveries/{delivery_id}/retry", tag = "webhooks",
    params(("id" = String, Path), ("delivery_id" = String, Path)),
    responses((status = 200, body = Delivery), (status = 404, body = ErrorBody)))]
async fn retry_delivery(
    State(state): State<AppState>,
    DeliveryPath { id, delivery_id }: DeliveryPath,
) -> Result<Json<Delivery>> {
    Ok(Json(
        webhooks::retry_delivery(&state.db, &id, &delivery_id).await?,
    ))
}
