//! `Idempotency-Key` support for REST POSTs: a retry with the same key and the
//! same request gets the first response again, marked `Idempotent-Replayed: true`.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::Sha256;

use crate::AppState;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::service::api_keys::Principal;
use crate::service::idempotency::{self, Attempt, Outcome, StoredResponse};

pub const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");
pub const IDEMPOTENT_REPLAYED: HeaderName = HeaderName::from_static("idempotent-replayed");

/// Largest request body read for hashing; the same as axum's JSON body limit.
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

/// For a POST with an `Idempotency-Key`, replay the stored response or run the
/// request and store its response. Must run after authentication, since keys
/// are scoped per API key.
pub async fn replay_or_record(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response> {
    if request.method() != Method::POST {
        return Ok(next.run(request).await);
    }
    let Some(key) = request.headers().get(&IDEMPOTENCY_KEY) else {
        return Ok(next.run(request).await);
    };
    let key = key
        .to_str()
        .map_err(|_| Error::Invalid("Idempotency-Key must be printable ASCII".into()))?
        .trim()
        .to_string();
    let api_key_id = request
        .extensions()
        .get::<Principal>()
        .ok_or(Error::Unauthorized)?
        .key_id
        .clone();
    let uri = request.uri();
    let target = uri
        .path_and_query()
        .map_or(uri.path(), |target| target.as_str());
    let target = format!("{} {target}", request.method());

    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, MAX_REQUEST_BYTES)
        .await
        .map_err(|_| Error::Invalid("request body is too large".into()))?;
    let body_hash = {
        use sha2::Digest as _;
        hex::encode(Sha256::digest(&body))
    };
    let attempt = Attempt {
        api_key_id: &api_key_id,
        key: &key,
        request: &target,
        body_hash: &body_hash,
    };
    if let Outcome::Replay(stored) = idempotency::begin(&state.db, &attempt).await? {
        return Ok(replay(stored));
    }

    // Run to completion even if the client goes away, so its retry finds the
    // response instead of a key stuck in progress.
    let request = Request::from_parts(parts, Body::from(body));
    let task = tokio::spawn(async move {
        let response = next.run(request).await;
        let attempt = Attempt {
            api_key_id: &api_key_id,
            key: &key,
            request: &target,
            body_hash: &body_hash,
        };
        record(&state.db, &attempt, response).await
    });
    Ok(task.await.map_err(anyhow::Error::from)?)
}

/// Store `response` for replay, or release the key after a server error so a
/// retry runs the request again.
async fn record(db: &Db, attempt: &Attempt<'_>, response: Response) -> Response {
    if response.status().is_server_error() {
        if let Err(err) = idempotency::abandon(db, attempt).await {
            tracing::error!(error = ?err, "releasing idempotency key");
        }
        return response;
    }
    let (parts, body) = response.into_parts();
    let body = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(err) => {
            if let Err(err) = idempotency::abandon(db, attempt).await {
                tracing::error!(error = ?err, "releasing idempotency key");
            }
            return Error::Internal(anyhow::Error::from(err)).into_response();
        }
    };
    let stored = StoredResponse {
        status: parts.status.as_u16(),
        content_type: parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: body.to_vec(),
    };
    if let Err(err) = idempotency::finish(db, attempt, &stored).await {
        tracing::error!(error = ?err, "storing idempotent response");
    }
    Response::from_parts(parts, Body::from(body))
}

fn replay(stored: StoredResponse) -> Response {
    let mut response = Response::new(Body::from(stored.body));
    *response.status_mut() =
        StatusCode::from_u16(stored.status).expect("stored statuses come from responses");
    let headers = response.headers_mut();
    if let Some(content_type) = stored
        .content_type
        .and_then(|value| HeaderValue::from_str(&value).ok())
    {
        headers.insert(header::CONTENT_TYPE, content_type);
    }
    headers.insert(IDEMPOTENT_REPLAYED, HeaderValue::from_static("true"));
    response
}
