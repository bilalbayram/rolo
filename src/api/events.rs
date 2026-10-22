use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    AppState,
    api::Params,
    error::Result,
    service::events::{self, EventPage, EventQuery},
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list))
}

/// Read the change feed
///
/// Every create, update and delete is recorded here in commit order. Poll with
/// `after` set to the `next_after` of your previous call to sync changes.
#[utoipa::path(get, path = "/events", tag = "events", params(EventQuery),
    responses((status = 200, body = EventPage)))]
async fn list(
    State(state): State<AppState>,
    Params(query): Params<EventQuery>,
) -> Result<Json<EventPage>> {
    Ok(Json(events::list(&state.db, query).await?))
}
