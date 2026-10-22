use axum::Json;
use axum::extract::State;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::extract::Params;
use crate::error::{ErrorBody, Result};
use crate::service::search::{self, SearchQuery, SearchResults};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(search))
}

/// Search all records
///
/// Ranked full-text search across contacts, companies, deals and activities,
/// including custom field values. Title matches (names, deal titles, activity
/// subjects) rank above other text. Accents are ignored.
#[utoipa::path(get, path = "/search", tag = "search", params(SearchQuery),
    responses((status = 200, body = SearchResults), (status = 422, body = ErrorBody)))]
async fn search(
    State(state): State<AppState>,
    Params(query): Params<SearchQuery>,
) -> Result<Json<SearchResults>> {
    Ok(Json(search::search_query(&state.db, query).await?))
}
