use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
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
        Some(Value::Number(n)) => match n.as_u64().and_then(|id| usize::try_from(id).ok()) {
            Some(id) => id,
            None => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "error": "invalid job_id" })),
                )
                    .into_response();
            }
        },
        Some(Value::String(s)) => match s.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                warn!(
                    event = "batch.invalid_job_id",
                    "post_batch: invalid job_id string"
                );
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "error": "invalid job_id" })),
                )
                    .into_response();
            }
        },
        _ => {
            warn!(
                event = "batch.invalid_job_id",
                "post_batch: missing or wrong type for job_id"
            );
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "missing job_id" })),
            )
                .into_response();
        }
    };

    let job = {
        let jobs = lock!(state.jobs);
        let Some(job) = jobs
            .iter()
            .find(|j| j.job_info["job_id"].as_u64() == Some(job_id as u64))
        else {
            warn!(
                event = "batch.unknown_job",
                job_id, "post_batch: job_id out of range"
            );
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "job_id out of range" })),
            )
                .into_response();
        };
        job.clone()
    }; // lock released before I/O
    let scanner = ScannerInfo {
        model: state.scanner.display_model(),
        serial: state.scanner.display_serial(),
    };
    // Batch::create does create_dir_all + file I/O — run off the async thread.
    let create_state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _change = lock!(create_state.config_change_lock);
        let paperless_options = lock!(create_state.paperless_config).as_ref().map(|config| {
            crate::batch::PaperlessUploadOptions {
                tag_ids: config.tag_ids.clone(),
                batch_id_field_id: config.batch_id_field_id,
            }
        });
        let mut batch = Batch::create(&job, scanner)?;
        if let Some(options) = paperless_options {
            batch.enable_paperless_with_options(options)?;
        }
        let id = batch.id.clone();
        lock!(create_state.batches).insert(id.clone(), Arc::new(Mutex::new(batch)));
        Ok::<_, anyhow::Error>(id)
    })
    .await;
    match result {
        Ok(Ok(id)) => {
            info!(event = "batch.created", batch_id = %id, job_id, "batch created");
            (StatusCode::OK, Json(json!({ "batch_id": id }))).into_response()
        }
        Ok(Err(e)) => {
            warn!(
                event = "batch.create_failed",
                error_class = crate::logging::error_class(&e),
                "failed to create batch"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to create batch" })),
            )
                .into_response()
        }
        Err(_) => {
            tracing::error!(
                event = "batch.worker_failed",
                "batch creation worker failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn put_batch(
    Path(batch_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // A per-batch lock serializes upload and completion without holding the map lock.
    let batch = match lock!(state.batches).get(&batch_id).cloned() {
        Some(b) => b,
        None => {
            warn!(event = "batch.unknown", "put_batch: unknown batch_id");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let Ok(_slot) = state.pdf_slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // complete() does PDF assembly + file writes — run off the async thread.
    let batch_for_work = batch.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut batch = lock!(batch_for_work);
        batch.complete()
    })
    .await;
    match result {
        Ok(Ok(())) => {
            let n_files = lock!(batch).metadata().files.len();
            {
                let mut batches = lock!(state.batches);
                if batches
                    .get(&batch_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &batch))
                {
                    batches.remove(&batch_id);
                }
            }
            info!(event = "batch.completed", batch_id = %crate::logging::safe_batch_id(&batch_id), n_files, "batch completed");
            StatusCode::OK.into_response()
        }
        Ok(Err(e)) => {
            warn!(event = "batch.complete_failed", batch_id = %crate::logging::safe_batch_id(&batch_id), error_class = crate::logging::error_class(&e), "failed to complete batch");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to complete batch" })),
            )
                .into_response()
        }
        Err(_) => {
            tracing::error!(event = "batch.worker_failed", batch_id = %crate::logging::safe_batch_id(&batch_id), "batch completion worker failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::config::{Config, Job};
    use crate::routes::test_support::TestApp;
    use crate::state::AppState;

    fn test_server() -> (TestApp, tempfile::TempDir) {
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
        (TestApp::new(AppState::new(config)), tmp)
    }

    fn empty_server() -> TestApp {
        let config = Config {
            jobs: vec![],
            ..Default::default()
        };
        TestApp::new(AppState::new(config))
    }

    async fn create_batch(server: &TestApp) -> String {
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
    async fn test_post_batch_rejects_negative_job_id() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": -1}))
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

    #[tokio::test]
    async fn test_put_batch_can_retry_when_pdf_worker_is_busy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = AppState::new(Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({"name":"Test", "job_id":0}),
                scan_settings: json!({}),
            }],
            ..Default::default()
        });
        let server = TestApp::new(state.clone());
        let id = create_batch(&server).await;
        let permit = state.pdf_slots.clone().acquire_owned().await.unwrap();
        assert_eq!(
            server
                .put(&format!("/NmWebService/batch/{id}"))
                .await
                .status_code(),
            503
        );
        drop(permit);
        assert_eq!(
            server
                .put(&format!("/NmWebService/batch/{id}"))
                .await
                .status_code(),
            200
        );
    }
}
