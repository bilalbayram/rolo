//! Extractors whose rejections use the API's JSON error format.

use axum::extract::{FromRequest, FromRequestParts};
use axum::http::request::Parts;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::Error;

/// JSON request body; see `axum::Json`.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(Error))]
pub struct Body<T>(pub T);

/// Query string parameters. Parameters `T` doesn't know are rejected (with
/// `#[serde(deny_unknown_fields)]` on `T`), so a mistyped filter is an error
/// instead of silently returning unfiltered results.
pub struct Params<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for Params<T> {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        serde_urlencoded::from_str(query)
            .map(Params)
            .map_err(query_error)
    }
}

fn query_error(err: serde_urlencoded::de::Error) -> Error {
    let message = err.to_string();
    // serde reports unknown keys as "unknown field `name`, expected ...".
    match message
        .strip_prefix("unknown field `")
        .and_then(|rest| rest.split_once('`'))
    {
        Some((name, _)) => Error::Invalid(format!("unknown query parameter {name:?}")),
        None => Error::Invalid(format!("invalid query string: {message}")),
    }
}

/// The single `{id}` path parameter.
#[derive(Deserialize, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(Error))]
#[serde(transparent)]
pub struct Id(pub String);
