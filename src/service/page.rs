use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One page of a cursor-paginated list.
#[derive(Debug, Serialize, ToSchema, JsonSchema)]
pub struct Page<T> {
    pub data: Vec<T>,
    /// Pass as `cursor` to fetch the next page; null when there are no more results.
    pub next_cursor: Option<String>,
    /// How many records match the filters across all pages. Present only
    /// when requested with `include_total=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
    /// Sums of number fields over all matches, keyed like the request's `sum`,
    /// e.g. `{"custom.guests": 1250}`. Present only when requested with `sum`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sums: Option<BTreeMap<String, f64>>,
}

/// List order for records sorted by ID, which is creation order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema, JsonSchema)]
pub enum Sort {
    /// Oldest first.
    #[default]
    #[serde(rename = "id")]
    Id,
    /// Newest first.
    #[serde(rename = "-id")]
    IdDescending,
}

impl Sort {
    /// `asc` or `desc`, for the list queries.
    pub fn direction(self) -> &'static str {
        match self {
            Sort::Id => "asc",
            Sort::IdDescending => "desc",
        }
    }
}

impl<T> Page<T> {
    /// A page with no results and no next page.
    pub fn empty() -> Self {
        Self {
            data: Vec::new(),
            next_cursor: None,
            total: None,
            sums: None,
        }
    }

    /// The page with `total` set.
    pub fn with_total(self, total: Option<i64>) -> Self {
        Self { total, ..self }
    }

    /// The page with `sums` set.
    pub fn with_sums(self, sums: Option<BTreeMap<String, f64>>) -> Self {
        Self { sums, ..self }
    }
}
