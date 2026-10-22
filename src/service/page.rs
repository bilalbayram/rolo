use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One page of a cursor-paginated list.
#[derive(Debug, Serialize, ToSchema, JsonSchema)]
pub struct Page<T> {
    pub data: Vec<T>,
    /// Pass as `cursor` to fetch the next page; null when there are no more results.
    pub next_cursor: Option<String>,
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
        }
    }
}
