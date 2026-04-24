use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;

use crate::batch::Batch;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct PostBatchRequest {
    pub job_id: usize,
}

pub async fn post_batch(
    State(state): State<AppState>,
    Json(req): Json<PostBatchRequest>,
) -> impl IntoResponse {
    let job = &state.config.jobs[req.job_id];
    match Batch::create(job) {
        Ok(batch) => {
            let id = batch.id.clone();
            state.batches.lock().unwrap().insert(id.clone(), batch);
            (StatusCode::OK, Json(json!({ "batch_id": id }))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn put_batch(
    Path(batch_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let mut batches = state.batches.lock().unwrap();
    if let Some(batch) = batches.get_mut(&batch_id) {
        if let Err(e) = batch.complete() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    } else {
        return StatusCode::NOT_FOUND.into_response();
    }
    batches.remove(&batch_id);
    StatusCode::OK.into_response()
}
