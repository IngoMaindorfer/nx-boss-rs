use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::batch::{Batch, ScannerInfo};
use crate::lock;
use crate::state::AppState;

pub async fn post_batch(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    // Scanner may send job_id as integer or string
    let job_id = match body.get("job_id") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
        Some(Value::String(s)) => match s.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                warn!("post_batch: invalid job_id string");
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "error": "invalid job_id" })),
                )
                    .into_response();
            }
        },
        other => {
            warn!(got = ?other, "post_batch: missing or wrong type for job_id");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "missing job_id" })),
            )
                .into_response();
        }
    };

    let jobs = lock!(state.jobs);
    if job_id >= jobs.len() {
        warn!(job_id, "post_batch: job_id out of range");
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "job_id out of range" })),
        )
            .into_response();
    }

    let job = jobs[job_id].clone();
    drop(jobs);
    let scanner = ScannerInfo {
        model: state.scanner_display_model(),
        serial: state.scanner_display_serial(),
    };
    match Batch::create(&job, scanner) {
        Ok(batch) => {
            let id = batch.id.clone();
            let job_name = job.job_info["name"].as_str().unwrap_or("?").to_string();
            info!(batch_id = %id, job_id, job_name, "batch created");
            lock!(state.batches).insert(id.clone(), batch);
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
    // Remove from map before I/O so the lock is not held during file writes / PDF assembly.
    let mut batch = match lock!(state.batches).remove(&batch_id) {
        Some(b) => b,
        None => {
            warn!(batch_id = %batch_id, "put_batch: unknown batch_id");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    // Lock is released here — complete() may do significant file I/O.
    match batch.complete() {
        Ok(()) => {
            info!(batch_id = %batch_id, n_files = batch.metadata().files.len(), "batch completed");
            StatusCode::OK.into_response()
        }
        Err(e) => {
            warn!(batch_id = %batch_id, error = %e, "failed to complete batch");
            // Re-insert so the scanner can retry.
            lock!(state.batches).insert(batch_id, batch);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use serde_json::{Value, json};

    use crate::config::{Config, Job};
    use crate::routes::router;
    use crate::state::AppState;

    fn test_server() -> (TestServer, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({
                    "name": "TestJob", "job_id": 0, "color": "#4D4D4D",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: json!({}),
            }],
            ..Default::default()
        };
        (TestServer::new(router(AppState::new(config))), tmp)
    }

    fn empty_server() -> TestServer {
        let config = Config {
            jobs: vec![],
            ..Default::default()
        };
        TestServer::new(router(AppState::new(config)))
    }

    async fn create_batch(server: &TestServer) -> String {
        server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 0}))
            .await
            .json::<Value>()["batch_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn test_post_batch_valid_job_id() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 0}))
            .await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.json::<Value>()["batch_id"].is_string());
    }

    #[tokio::test]
    async fn test_post_batch_string_job_id() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": "0"}))
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_post_batch_out_of_range_job_id() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 999}))
            .await;
        assert_eq!(resp.status_code(), 422);
    }

    #[tokio::test]
    async fn test_post_batch_missing_job_id() {
        let (server, _tmp) = test_server();
        let resp = server.post("/NmWebService/batch").json(&json!({})).await;
        assert_eq!(resp.status_code(), 422);
    }

    #[tokio::test]
    async fn test_post_batch_invalid_string_job_id() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": "notanumber"}))
            .await;
        assert_eq!(resp.status_code(), 422);
    }

    #[tokio::test]
    async fn test_post_batch_no_jobs_configured() {
        let server = empty_server();
        let resp = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 0}))
            .await;
        assert_eq!(resp.status_code(), 422);
    }

    #[tokio::test]
    async fn test_put_batch_completes_successfully() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let resp = server.put(&format!("/NmWebService/batch/{batch_id}")).await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_put_batch_unknown_id_returns_404() {
        let (server, _tmp) = test_server();
        let resp = server.put("/NmWebService/batch/deadbeefdeadbeef").await;
        assert_eq!(resp.status_code(), 404);
    }

    #[tokio::test]
    async fn test_put_batch_removes_from_active_batches() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        server.put(&format!("/NmWebService/batch/{batch_id}")).await;
        // Second PUT on same batch_id must return 404 — batch was removed
        let resp = server.put(&format!("/NmWebService/batch/{batch_id}")).await;
        assert_eq!(resp.status_code(), 404);
    }
}
