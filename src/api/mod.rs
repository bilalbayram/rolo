//! REST API under `/v1`. Handlers are thin wrappers over `crate::service`.

mod activities;
pub mod auth;
mod companies;
mod contacts;
mod deals;
pub mod doc;
mod events;
pub mod extract;
mod pipelines;
mod webhooks;

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

/// All `/v1` routes, carrying their OpenAPI operations.
pub fn router() -> OpenApiRouter<AppState> {
    let v1 = OpenApiRouter::new()
        .merge(companies::router())
        .merge(contacts::router())
        .merge(deals::router())
        .merge(pipelines::router())
        .merge(activities::router())
        .merge(events::router())
        .merge(webhooks::router());
    OpenApiRouter::with_openapi(doc::ApiDoc::openapi()).nest("/v1", v1)
}
