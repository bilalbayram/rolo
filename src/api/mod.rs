//! REST API under `/v1`. Handlers are thin wrappers over `crate::service`.

mod activities;
mod companies;
mod contacts;
mod deals;
mod events;
mod pipelines;

use axum::{
    extract::{FromRequest, FromRequestParts, Request, State},
    http::{Method, header},
    middleware::Next,
    response::Response,
};
use utoipa::{
    Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};
use utoipa_axum::router::OpenApiRouter;

use crate::{
    AppState,
    error::{Error, Result},
    service::api_keys::{self, Principal},
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "rolo",
        description = "Headless CRM API. Authenticate with `Authorization: Bearer <api key>`. \
                       Every mutation is also published to `/v1/events`."
    ),
    modifiers(&BearerAuth),
    security(("bearer" = [])),
    tags(
        (name = "companies"),
        (name = "contacts"),
        (name = "deals"),
        (name = "pipelines"),
        (name = "activities", description = "Notes, calls, emails, meetings and tasks"),
        (name = "events", description = "Change feed of every mutation, for sync and integrations"),
    )
)]
pub struct ApiDoc;

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

/// `axum::Json` whose rejections use the API's JSON error format.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(Error))]
pub struct Body<T>(pub T);

/// `axum::extract::Query` whose rejections use the API's JSON error format.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(Error))]
pub struct Params<T>(pub T);

/// `axum::extract::Path` whose rejections use the API's JSON error format.
#[derive(FromRequestParts, serde::Deserialize)]
#[from_request(via(axum::extract::Path), rejection(Error))]
#[serde(transparent)]
pub struct Id(pub String);

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi()).nest(
        "/v1",
        OpenApiRouter::new()
            .merge(companies::router())
            .merge(contacts::router())
            .merge(deals::router())
            .merge(pipelines::router())
            .merge(activities::router())
            .merge(events::router()),
    )
}

/// Resolve the bearer token to a [`Principal`] and attach it to the request.
pub async fn authenticate(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(Error::Unauthorized)?;
    let principal = api_keys::authenticate(&state.db, token.trim()).await?;
    req.extensions_mut().insert(principal);
    Ok(next.run(req).await)
}

/// REST mutations need a write-scoped key. (MCP checks scope per tool instead,
/// since every MCP call is a POST.)
pub async fn require_write_for_mutations(req: Request, next: Next) -> Result<Response> {
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        req.extensions()
            .get::<Principal>()
            .ok_or(Error::Unauthorized)?
            .require_write()?;
    }
    Ok(next.run(req).await)
}
