//! Small helpers shared by the service layer.

use std::sync::Mutex;

use serde::{Deserialize, Deserializer};
use ulid::Generator;

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

/// `%query%` for a LIKE search with `ESCAPE '\'`, with wildcards in the input escaped.
pub fn like_pattern(query: Option<String>) -> Option<String> {
    non_empty(query).map(|query| {
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    })
}

/// Page size from a `limit` parameter: 1 to 200, default 50.
pub fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
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

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(
            like_pattern(Some(" 50%_off ".into())),
            Some(r"%50\%\_off%".into())
        );
        assert_eq!(like_pattern(Some("  ".into())), None);
    }
}
