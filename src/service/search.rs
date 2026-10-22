//! Full-text search over the `search_index` FTS5 table (see migration 0003).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::entity_type::EntityType;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::util;

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 50;

#[derive(Debug, Clone, Serialize, ToSchema, JsonSchema)]
pub struct SearchHit {
    pub entity_type: EntityType,
    /// ID of the matching record; fetch it from its endpoint for full details.
    pub id: String,
    /// Name, title or subject of the record.
    pub title: String,
    /// Excerpt around the match, with matched terms wrapped in `**`.
    pub snippet: String,
}

#[derive(Debug, Serialize, ToSchema, JsonSchema)]
pub struct SearchResults {
    /// Best matches first.
    pub data: Vec<SearchHit>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// Words to find. Each word matches as a prefix (`acm` finds Acme), and all
    /// words must match. Punctuation is ignored.
    pub q: String,
    /// Comma-separated record types to include, e.g. `contact,company`. Default: all.
    pub types: Option<String>,
    /// 1-50, default 20.
    pub limit: Option<i64>,
}

/// How a list endpoint's optional `q` parameter filters results.
pub enum Filter {
    /// No `q`: no filtering.
    All,
    /// FTS5 MATCH expression to filter by.
    Matching(String),
    /// `q` contained no searchable words, so nothing can match.
    Nothing,
}

impl Filter {
    pub fn from_query(q: Option<String>) -> Self {
        match util::non_empty(q) {
            None => Filter::All,
            Some(q) => match match_expression(&q) {
                Some(expression) => Filter::Matching(expression),
                None => Filter::Nothing,
            },
        }
    }
}

/// FTS5 MATCH expression requiring every word of `q` as a prefix, e.g.
/// `ada@acme` becomes `"ada"* "acme"*`. Quoting each word keeps user input from
/// being parsed as FTS5 query syntax. Turkish ı and İ become i, as in the index
/// (see migration 0010), so `isik` and `Işık` match the same words. `None` if
/// `q` has no words.
pub fn match_expression(q: &str) -> Option<String> {
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| format!("\"{}\"*", word.replace(['ı', 'İ'], "i")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn parse_types(types: Option<&str>) -> Result<Vec<EntityType>> {
    let Some(types) = types else {
        return Ok(Vec::new());
    };
    types
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            serde_json::from_value(serde_json::Value::String(name.to_string())).map_err(|_| {
                Error::Invalid(format!(
                    "unknown type {name:?}; expected contact, company, deal or activity"
                ))
            })
        })
        .collect()
}

/// Ranked search across record types; `types` empty means all.
pub async fn search(
    db: &Db,
    q: &str,
    types: &[EntityType],
    limit: Option<i64>,
) -> Result<SearchResults> {
    let Some(expression) = match_expression(q) else {
        return Ok(SearchResults { data: Vec::new() });
    };
    let types = (!types.is_empty())
        .then(|| serde_json::to_string(types))
        .transpose()
        .map_err(anyhow::Error::from)?;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // Title matches weigh 10x body matches; bm25 is lower for better matches.
    let data = sqlx::query_as!(
        SearchHit,
        r#"SELECT d.entity_type AS "entity_type!: EntityType",
                  d.entity_id AS "id!",
                  d.title AS "title!",
                  snippet(search_index, -1, '**', '**', '…', 12) AS "snippet!: String"
           FROM search_index
           JOIN search_documents d ON d.rowid = search_index.rowid
           WHERE search_index MATCH ?1
             AND (?2 IS NULL OR d.entity_type IN (SELECT value FROM json_each(?2)))
           ORDER BY bm25(search_index, 10.0, 1.0)
           LIMIT ?3"#,
        expression,
        types,
        limit,
    )
    .fetch_all(&db.reader)
    .await?;
    Ok(SearchResults { data })
}

/// [`search`] with REST query parameters.
pub async fn search_query(db: &Db, query: SearchQuery) -> Result<SearchResults> {
    let types = parse_types(query.types.as_deref())?;
    search(db, &query.q, &types, query.limit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_expression_quotes_words_as_prefixes() {
        assert_eq!(
            match_expression("ada@acme.io"),
            Some(r#""ada"* "acme"* "io"*"#.into())
        );
        assert_eq!(
            match_expression(r#"x" OR title:*"#),
            Some(r#""x"* "OR"* "title"*"#.into())
        );
        assert_eq!(match_expression("Zoë"), Some(r#""Zoë"*"#.into()));
        assert_eq!(
            match_expression("IŞIK Işık"),
            Some(r#""IŞIK"* "Işik"*"#.into())
        );
        assert_eq!(match_expression("İstanbul"), Some(r#""istanbul"*"#.into()));
        assert_eq!(match_expression(" -- "), None);
    }

    #[test]
    fn parses_types() {
        let types = parse_types(Some("contact, deal")).expect("valid types");
        assert_eq!(types, vec![EntityType::Contact, EntityType::Deal]);
        assert!(parse_types(Some("people")).is_err());
        assert!(parse_types(None).expect("no types").is_empty());
    }
}
