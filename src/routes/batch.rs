use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use crate::batch::Batch;
use crate::state::AppState;

pub async fn post_batch(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    debug!(body = %body, "POST /batch body");

    // Scanner may send job_id as integer or string
    let job_id = match body.get("job_id") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
        Some(Value::String(s)) => match s.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                warn!(body = %body, "post_batch: invalid job_id string");
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "error": "invalid job_id" })),
                )
                    .into_response();
            }
        },
        other => {
            warn!(body = %body, got = ?other, "post_batch: missing or wrong type for job_id");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "missing job_id" })),
            )
                .into_response();
        }
    };

    if job_id >= state.config.jobs.len() {
        warn!(job_id, "post_batch: job_id out of range");
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "job_id out of range" })),
        )
            .into_response();
    }

    let job = &state.config.jobs[job_id];
    match Batch::create(job) {
        Ok(batch) => {
            let id = batch.id.clone();
            let job_name = job.job_info["name"].as_str().unwrap_or("?");
            info!(batch_id = %id, job_id, job_name, "batch created");
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
