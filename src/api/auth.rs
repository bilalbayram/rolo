use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;
use crate::error::{Error, Result};
use crate::service::api_keys::{self, Principal};

/// Resolve the bearer token to a [`Principal`] and attach it to the request.
pub async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(Error::Unauthorized)?;
    let principal = api_keys::authenticate(&state.db, token.trim()).await?;
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

/// REST mutations need a write-scoped key. (MCP checks scope per tool instead,
/// since every MCP call is a POST.)
pub async fn require_write_for_mutations(request: Request, next: Next) -> Result<Response> {
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        request
            .extensions()
            .get::<Principal>()
            .ok_or(Error::Unauthorized)?
            .require_write()?;
    }
    Ok(next.run(request).await)
}
