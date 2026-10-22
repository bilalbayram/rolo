//! Business logic shared by the REST API and the MCP server. Every mutation
//! runs in a write transaction that also appends to the `events` outbox.

pub mod activities;
pub mod api_keys;
pub mod companies;
pub mod contacts;
pub mod deals;
pub mod events;
pub mod pipelines;

use schemars::JsonSchema;
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::{Error, Result};

#[derive(Debug, Serialize, ToSchema, JsonSchema)]
pub struct Page<T> {
    pub data: Vec<T>,
    /// Pass as `cursor` to fetch the next page; null when there are no more results.
    pub next_cursor: Option<String>,
}

/// Trimmed, non-empty value for a required text field.
fn required(value: String, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::Invalid(format!("{field} must not be empty")));
    }
    Ok(value.to_string())
}

/// `%pattern%` for a LIKE search, with LIKE wildcards in the input escaped.
fn like_pattern(q: Option<String>) -> Option<String> {
    crate::util::non_empty(q).map(|q| {
        let escaped = q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    })
}
