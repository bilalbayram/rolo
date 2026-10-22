//! Extractors whose rejections use the API's JSON error format.

use axum::extract::{FromRequest, FromRequestParts};
use serde::Deserialize;

use crate::error::Error;

/// JSON request body; see `axum::Json`.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(Error))]
pub struct Body<T>(pub T);

/// Query string parameters; see `axum::extract::Query`.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(Error))]
pub struct Params<T>(pub T);

/// The single `{id}` path parameter.
#[derive(Deserialize, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(Error))]
#[serde(transparent)]
pub struct Id(pub String);
