use std::{collections::BTreeMap, sync::Mutex};

use serde::{Deserialize, Deserializer};
use ulid::Generator;

/// User-defined fields stored alongside each record.
pub type Custom = BTreeMap<String, serde_json::Value>;

/// Prefixed, time-sortable identifier, e.g. `con_01jcz3...`. IDs come from a
/// monotonic generator, so they sort in creation order even within the same
/// millisecond; list endpoints rely on this for ordering and cursors.
pub fn new_id(prefix: &str) -> String {
    static GENERATOR: Mutex<Generator> = Mutex::new(Generator::new());
    let mut generator = GENERATOR.lock().unwrap_or_else(|e| e.into_inner());
    let ulid = match generator.generate() {
        Ok(ulid) => ulid,
        // 2^80 IDs in one millisecond; keep going rather than fail the request.
        Err(overflow) => overflow.commit_overflow_random(),
    };
    format!("{prefix}_{}", ulid.to_string().to_lowercase())
}

/// Distinguishes "field absent" (`None`) from "field set to null" (`Some(None)`)
/// in PATCH bodies, so clients can clear nullable fields.
pub fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// Apply a PATCH field to a nullable value.
pub fn patch<T>(current: &mut Option<T>, update: Option<Option<T>>) {
    if let Some(value) = update {
        *current = value;
    }
}

/// Merge custom-field updates: keys set to null are removed, others replaced.
pub fn merge_custom(current: &mut Custom, update: Option<Custom>) {
    for (key, value) in update.into_iter().flatten() {
        if value.is_null() {
            current.remove(&key);
        } else {
            current.insert(key, value);
        }
    }
}

/// Treat empty or whitespace-only strings as absent.
pub fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 200;

pub fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Fetch one more row than requested; if it exists there is a next page.
pub fn paginate<T>(
    mut rows: Vec<T>,
    limit: i64,
    id: impl Fn(&T) -> &str,
) -> crate::service::Page<T> {
    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|r| id(r).to_string())
    } else {
        None
    };
    crate::service::Page {
        data: rows,
        next_cursor,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_sort_in_creation_order() {
        let ids: Vec<_> = (0..10_000).map(|_| super::new_id("x")).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }
}
