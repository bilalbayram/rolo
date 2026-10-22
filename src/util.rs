//! Small helpers shared by the service layer.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use ulid::{Generator, Ulid};

use crate::error::{Error, Result};
use crate::service::Page;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

/// Prefixed, time-sortable identifier, e.g. `con_01jcz3...`. IDs come from a
/// monotonic generator, so they sort in creation order even within the same
/// millisecond; list endpoints rely on this for ordering and cursors.
pub fn new_id(prefix: &str) -> String {
    static GENERATOR: Mutex<Generator> = Mutex::new(Generator::new());
    let mut generator = GENERATOR.lock().expect("ID generator mutex poisoned");
    let ulid = match generator.generate() {
        Ok(ulid) => ulid,
        // 2^80 IDs in one millisecond; keep going rather than fail the request.
        Err(overflow) => overflow.commit_overflow_random(),
    };
    format!("{prefix}_{}", ulid.to_string().to_lowercase())
}

/// ID and creation time for a new record. `created_at` is set when importing
/// a record created earlier elsewhere; it can't be in the future, and the ID
/// then encodes that time, so the record sorts by ID where it would have.
/// Otherwise the record is created now.
pub fn new_record(
    prefix: &str,
    created_at: Option<DateTime<Utc>>,
) -> Result<(String, DateTime<Utc>)> {
    let now = Utc::now();
    match created_at {
        None => Ok((new_id(prefix), now)),
        Some(created_at) if created_at > now => Err(Error::Invalid(
            "created_at must not be in the future".into(),
        )),
        Some(created_at) => {
            let ulid = Ulid::from_datetime(created_at.into());
            Ok((
                format!("{prefix}_{}", ulid.to_string().to_lowercase()),
                created_at,
            ))
        }
    }
}

/// Serde helper distinguishing "field absent" (`None`) from "field set to null"
/// (`Some(None)`) in PATCH bodies, so clients can clear nullable fields. Use with
/// `#[serde(default, deserialize_with = "util::double_option")]`.
pub fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Apply a PATCH field to a nullable value.
pub fn patch<T>(current: &mut Option<T>, update: Option<Option<T>>) {
    if let Some(value) = update {
        *current = value;
    }
}

/// Trimmed value, or `None` for empty or whitespace-only strings.
pub fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Trimmed, non-empty value for a required text field.
pub fn required(value: String, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::Invalid(format!("{field} must not be empty")));
    }
    Ok(value.to_string())
}

/// Page size from a `limit` parameter: 1 to 200, default 50.
pub fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// IDs requested with a list endpoint's `ids` parameter, e.g. `ids=con_a,con_b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdList(Vec<String>);

impl IdList {
    /// Most IDs one request can ask for.
    pub const MAX: usize = 100;

    /// The IDs in comma-separated `ids`, without blanks or repeats; `None` if
    /// absent. More than [`IdList::MAX`] is an error.
    pub fn parse(ids: Option<String>) -> Result<Option<IdList>> {
        let Some(ids) = ids else {
            return Ok(None);
        };
        let mut list: Vec<String> = Vec::new();
        for id in ids.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if !list.iter().any(|seen| seen == id) {
                list.push(id.to_string());
            }
        }
        if list.is_empty() {
            return Err(Error::Invalid("ids must list at least one ID".into()));
        }
        if list.len() > Self::MAX {
            return Err(Error::Invalid(format!(
                "ids can list at most {} IDs",
                Self::MAX
            )));
        }
        Ok(Some(IdList(list)))
    }

    /// An error for combining `ids` with paging, sorting or totals, which
    /// don't apply: all matches come back in one page, in the order requested.
    pub fn exclusive() -> Error {
        Error::Invalid("ids can't be combined with limit, cursor, sort or include_total".into())
    }

    pub fn count(&self) -> i64 {
        i64::try_from(self.0.len()).expect("at most IdList::MAX IDs")
    }

    /// A JSON array of the IDs, for `json_each` in a query.
    pub fn to_json(&self) -> String {
        serde_json::Value::from(self.0.clone()).to_string()
    }

    /// `rows` in the order the IDs were requested.
    pub fn in_order<T>(&self, mut rows: Vec<T>, id: impl Fn(&T) -> &str) -> Vec<T> {
        rows.sort_by_key(|row| self.0.iter().position(|wanted| wanted == id(row)));
        rows
    }
}

/// Build a page from `limit + 1` fetched rows; the extra row signals a next page.
pub fn paginate<T>(mut rows: Vec<T>, limit: i64, id: impl Fn(&T) -> &str) -> Page<T> {
    let limit = usize::try_from(limit).expect("limit is clamped to a positive value");
    let next_cursor = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| id(row).to_string())
    } else {
        None
    };
    Page {
        data: rows,
        next_cursor,
        total: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_sort_in_creation_order() {
        let ids: Vec<String> = (0..10_000).map(|_| new_id("x")).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }
}
