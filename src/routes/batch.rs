use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

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
            let job_name = job.job_info["name"].as_str().unwrap_or("?");
            info!(batch_id = %id, job_id = req.job_id, job_name, "batch created");
            state.batches.lock().unwrap().insert(id.clone(), batch);
            (StatusCode::OK, Json(json!({ "batch_id": id }))).into_response()
        }
        Err(e) => {
            warn!(error = %e, "failed to create batch");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

pub async fn put_batch(
    Path(batch_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let mut batches = state.batches.lock().unwrap();
    if let Some(batch) = batches.get_mut(&batch_id) {
        if let Err(e) = batch.complete() {
            warn!(batch_id = %batch_id, error = %e, "failed to complete batch");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    } else {
        warn!(batch_id = %batch_id, "put_batch: unknown batch_id");
        return StatusCode::NOT_FOUND.into_response();
    }
    let n_files = batches[&batch_id].metadata().files.len();
    batches.remove(&batch_id);
    info!(batch_id = %batch_id, n_files, "batch completed");
    StatusCode::OK.into_response()
}
