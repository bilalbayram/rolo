pub mod api;
pub mod db;
pub mod error;
pub mod mcp;
pub mod service;
pub mod util;

use std::sync::Arc;

use axum::{Json, Router, middleware, routing::get};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tower_http::trace::TraceLayer;
use utoipa_scalar::{Scalar, Servable};

use crate::db::Db;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
}

pub struct AppConfig {
    /// `Host` header values the MCP endpoint accepts (DNS-rebinding protection).
    /// Empty means loopback only.
    pub allowed_hosts: Vec<String>,
}

pub fn app(db: Db, config: AppConfig) -> Router {
    let state = AppState { db: db.clone() };
    let auth = middleware::from_fn_with_state(state.clone(), api::authenticate);

    let (rest, openapi) = api::router().split_for_parts();
    let rest = rest
        .layer(middleware::from_fn(api::require_write_for_mutations))
        .layer(auth.clone());

    let mut mcp_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    if !config.allowed_hosts.is_empty() {
        mcp_config = mcp_config.with_allowed_hosts(config.allowed_hosts);
    }
    let mcp = StreamableHttpService::new(
        move || Ok(mcp::RoloMcp::new(db.clone())),
        Arc::new(LocalSessionManager::default()),
        mcp_config,
    );
    let mcp = Router::new().nest_service("/mcp", mcp).layer(auth);

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
