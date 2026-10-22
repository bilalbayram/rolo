//! Extractors whose rejections use the API's JSON error format.

use axum::extract::{FromRequest, FromRequestParts};
use axum::http::request::Parts;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::custom_fields::CustomFields;
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

/// [`Params`] for lists that also filter on custom fields: each
/// `custom.<key>=<value>` parameter becomes an entry in the second field
/// (`None` if there are none), and the rest are parsed into `T` as strictly.
pub struct FilterParams<T>(pub T, pub Option<CustomFields>);

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for FilterParams<T> {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        let pairs: Vec<(String, String)> =
            serde_urlencoded::from_str(query).map_err(query_error)?;
        let mut custom = serde_json::Map::new();
        let mut rest = Vec::new();
        for (key, value) in pairs {
            let Some(field) = key.strip_prefix("custom.") else {
                rest.push((key, value));
                continue;
            };
            if field.is_empty() {
                return Err(Error::Invalid(format!("unknown query parameter {key:?}")));
            }
            if custom.insert(field.to_string(), value.into()).is_some() {
                return Err(Error::Invalid(format!(
                    "query parameter {key:?} is given more than once"
                )));
            }
        }
        let rest = serde_urlencoded::to_string(&rest).map_err(anyhow::Error::from)?;
        let params = serde_urlencoded::from_str(&rest).map_err(query_error)?;
        let custom = (!custom.is_empty()).then(|| custom.into_iter().collect());
        Ok(FilterParams(params, custom))
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
