use schemars::JsonSchema;
use serde::Serialize;
use utoipa::ToSchema;

/// One page of a cursor-paginated list.
#[derive(Debug, Serialize, ToSchema, JsonSchema)]
pub struct Page<T> {
    pub data: Vec<T>,
    /// Pass as `cursor` to fetch the next page; null when there are no more results.
    pub next_cursor: Option<String>,
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
