use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::api::extract::{Body, Id};
use crate::error::{ErrorBody, Result};
use crate::service::pipelines::{self, NewPipeline, Pipeline};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get))
}

/// List pipelines with their stages
#[utoipa::path(get, path = "/pipelines", tag = "pipelines",
    responses((status = 200, body = Vec<Pipeline>)))]
async fn list(State(state): State<AppState>) -> Result<Json<Vec<Pipeline>>> {
    Ok(Json(pipelines::list(&state.db).await?))
}

/// Create a pipeline
#[utoipa::path(post, path = "/pipelines", tag = "pipelines", request_body = NewPipeline,
    responses((status = 201, body = Pipeline), (status = 422, body = ErrorBody)))]
async fn create(
    State(state): State<AppState>,
    Body(input): Body<NewPipeline>,
) -> Result<(StatusCode, Json<Pipeline>)> {
    Ok((
        StatusCode::CREATED,
        Json(pipelines::create(&state.db, input).await?),
    ))
}

/// Get a pipeline
#[utoipa::path(get, path = "/pipelines/{id}", tag = "pipelines", params(("id" = String, Path)),
    responses((status = 200, body = Pipeline), (status = 404, body = ErrorBody)))]
async fn get(State(state): State<AppState>, Id(id): Id) -> Result<Json<Pipeline>> {
    Ok(Json(pipelines::get(&state.db, &id).await?))
}
