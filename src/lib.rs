//! rolo: a headless CRM for agents and apps. REST API, MCP server and webhook
//! delivery over a single SQLite database.

pub mod api;
pub mod custom_fields;
pub mod db;
pub mod delivery;
pub mod error;
pub mod mcp;
pub mod phone;
pub mod service;
pub mod util;

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router, middleware};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tower_http::trace::TraceLayer;
use utoipa_scalar::{Scalar, Servable};

use crate::db::Db;
use crate::phone::Region;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    /// How to read phone numbers written without a country code.
    pub default_region: Region,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    /// `Host` header values the MCP endpoint accepts (DNS-rebinding protection).
    /// Empty means loopback only.
    pub allowed_hosts: Vec<String>,
    /// How to read phone numbers written without a country code.
    pub default_region: Region,
}

/// The HTTP application: REST at `/v1`, MCP at `/mcp`, docs at `/docs`.
/// Webhook delivery runs separately; see [`delivery::spawn`].
pub fn app(db: Db, config: AppConfig) -> Router {
    let default_region = config.default_region;
    let state = AppState {
        db: db.clone(),
        default_region,
    };
    let authenticate = middleware::from_fn_with_state(state.clone(), api::auth::authenticate);

    let (rest, openapi) = api::router().split_for_parts();
    // Layers run bottom to top: authenticate, check scope, then idempotency.
    let idempotency =
        middleware::from_fn_with_state(state.clone(), api::idempotency::replay_or_record);
    let rest = rest
        .layer(idempotency)
        .layer(middleware::from_fn(api::auth::require_write_for_mutations))
        .layer(authenticate.clone());

    let mut mcp_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    if !config.allowed_hosts.is_empty() {
        mcp_config = mcp_config.with_allowed_hosts(config.allowed_hosts);
    }
    let mcp = StreamableHttpService::new(
        move || Ok(mcp::RoloMcp::new(db.clone(), default_region)),
        Arc::new(LocalSessionManager::default()),
        mcp_config,
    );
    let mcp = Router::new().nest_service("/mcp", mcp).layer(authenticate);

    let spec = openapi.clone();
    Router::new()
        .merge(rest)
        .merge(mcp)
        .merge(Scalar::with_url("/docs", openapi))
        .route("/openapi.json", get(move || async move { Json(spec) }))
        .route("/health", get(|| async { "ok" }))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// The OpenAPI document, without starting a server.
pub fn openapi() -> utoipa::openapi::OpenApi {
    api::router().into_openapi()
}
