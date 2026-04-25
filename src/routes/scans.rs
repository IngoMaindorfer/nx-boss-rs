use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::ui::{ScanEntry, find_batch_dir, list_scans, render};
use crate::batch::{BatchMetadata, JobConfig};
use crate::lock;
use crate::state::AppState;
use crate::translations::Translations;

#[derive(Template)]
#[template(path = "scans_list.html")]
struct ScansListTpl {
    scans: Vec<ScanEntry>,
    t: &'static Translations,
}

#[derive(Template)]
#[template(path = "scans_detail.html")]
struct ScansDetailTpl {
    batch_id: String,
    job_name: String,
    created_at: String,
    files: Vec<String>,
    completed: bool,
    scanner_model: Option<String>,
    scanner_serial: Option<String>,
    resolution: u32,
    pixel_format: String,
    jpeg_quality: u8,
    source: String,
    t: &'static Translations,
}

pub async fn scans_list(State(state): State<AppState>) -> Response {
    let jobs = lock!(state.jobs);
    let scans = list_scans(&jobs, 100);
    render(ScansListTpl {
        scans,
        t: state.translations,
    })
}

pub async fn scans_detail(Path(batch_id): Path<String>, State(state): State<AppState>) -> Response {
    let jobs = lock!(state.jobs);
    let Some(dir) = find_batch_dir(&jobs, &batch_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let meta_path = dir.join("metadata.json");
    let Ok(content) = std::fs::read_to_string(&meta_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(meta) = serde_json::from_str::<BatchMetadata>(&content) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let files: Vec<String> = meta.files.iter().map(|f| f.filename.clone()).collect();
    let JobConfig {
        resolution,
        pixel_format,
        jpeg_quality,
        source,
    } = meta.job_config;
    render(ScansDetailTpl {
        batch_id,
        job_name: meta.job_name,
        created_at: meta.created_at,
        files,
        completed: meta.completed,
        scanner_model: meta.scanner.model,
        scanner_serial: meta.scanner.serial,
        resolution,
        pixel_format,
        jpeg_quality,
        source,
        t: state.translations,
    })
}

pub async fn scans_file(
    Path((batch_id, filename)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    let jobs = lock!(state.jobs);
    let Some(dir) = find_batch_dir(&jobs, &batch_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let file_path = dir.join(&filename);
    match std::fs::read(&file_path) {
        Ok(bytes) => (
            [
                ("content-type", "image/jpeg"),
                ("cache-control", "max-age=3600"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
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

    #[tokio::test]
    async fn test_scans_list_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/scans").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_scans_list_shows_completed_batch() {
        let (server, _tmp) = test_server();
        let batch_id = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 0}))
            .await
            .json::<Value>()["batch_id"]
            .as_str()
            .unwrap()
            .to_string();
        server.put(&format!("/NmWebService/batch/{batch_id}")).await;
        assert!(server.get("/scans").await.text().contains(&batch_id[..8]));
    }

    #[tokio::test]
    async fn test_scans_detail_returns_200_for_existing_batch() {
        let (server, _tmp) = test_server();
        let batch_id = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 0}))
            .await
            .json::<Value>()["batch_id"]
            .as_str()
            .unwrap()
            .to_string();
        server.put(&format!("/NmWebService/batch/{batch_id}")).await;
        assert_eq!(
            server
                .get(&format!("/scans/{batch_id}"))
                .await
                .status_code(),
            200
        );
    }

    #[tokio::test]
    async fn test_scans_detail_returns_404_for_unknown() {
        let (server, _tmp) = test_server();
        assert_eq!(
            server
                .get("/scans/0000000000000000000000000000000a")
                .await
                .status_code(),
            404
        );
    }
}
