use askama::Template;
use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::UiState;
use super::ui::{
    ScanEntry, find_batch_dir, list_scans_without_holding_jobs_lock, paperless_needs_attention,
    paperless_status, render,
};
use crate::batch::is_safe_path;
use crate::batch::{BatchMetadata, JobConfig};
use crate::build_info::BuildInfo;
use crate::lock;
use crate::translations::Translations;

#[derive(Template)]
#[template(path = "scans_list.html")]
struct ScansListTpl {
    scans: Vec<ScanEntry>,
    paperless_attention_count: usize,
    t: &'static Translations,
    build: &'static BuildInfo,
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
    paperless_status: Option<String>,
    paperless_error: Option<String>,
    paperless_attention: bool,
    paperless_task_id: Option<String>,
    paperless_attempts: Option<u32>,
    paperless_last_attempt: Option<String>,
    t: &'static Translations,
    build: &'static BuildInfo,
}

pub async fn scans_list(UiState(state): UiState) -> Response {
    let Ok((all_scans, _)) =
        list_scans_without_holding_jobs_lock(&state.jobs, state.translations).await
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let paperless_attention_count = all_scans
        .iter()
        .filter(|scan| scan.paperless_attention)
        .count();
    let scans = all_scans
        .into_iter()
        .enumerate()
        .filter_map(|(index, scan)| (index < 100 || scan.paperless_attention).then_some(scan))
        .collect();
    render(ScansListTpl {
        scans,
        paperless_attention_count,
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn scans_detail(Path(batch_id): Path<String>, UiState(state): UiState) -> Response {
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
    let paperless_status = paperless_status(&meta, state.translations);
    let paperless_error = meta.paperless.as_ref().and_then(|p| p.last_error.clone());
    let paperless_attention = paperless_needs_attention(&meta);
    let paperless_task_id = meta.paperless.as_ref().and_then(|p| p.task_id.clone());
    let paperless_attempts = meta.paperless.as_ref().map(|p| p.upload_attempts);
    let paperless_last_attempt = meta
        .paperless
        .as_ref()
        .and_then(|p| p.last_attempt_at.clone());
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
        paperless_status,
        paperless_error,
        paperless_attention,
        paperless_task_id,
        paperless_attempts,
        paperless_last_attempt,
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn scans_file(
    Path((batch_id, filename)): Path<(String, String)>,
    UiState(state): UiState,
) -> Response {
    // Release the lock before the async read — MutexGuard is not Send.
    let dir = {
        let jobs = lock!(state.jobs);
        match find_batch_dir(&jobs, &batch_id) {
            Some(d) => d,
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };
    let file_path = dir.join(&filename);
    if !is_safe_path(&dir, &file_path) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let metadata = match tokio::fs::read(dir.join("metadata.json")).await {
        Ok(bytes) => match serde_json::from_slice::<BatchMetadata>(&bytes) {
            Ok(meta) => meta,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if !metadata.files.iter().any(|page| page.filename == filename) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::symlink_metadata(&file_path).await {
        Ok(info) if info.file_type().is_file() => {}
        _ => return StatusCode::NOT_FOUND.into_response(),
    }
    match tokio::fs::read(&file_path).await {
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

    fn multipart_image(boundary: &str, batch_id: &str, filename: &str) -> Vec<u8> {
        let param = format!(r#"{{"batch_id":"{batch_id}"}}"#);
        let mut body = Vec::new();
        let sep = format!("--{boundary}\r\n");
        let end = format!("--{boundary}--\r\n");
        body.extend_from_slice(sep.as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"image\"; filename=\"{filename}\"\r\n\
                 Content-Type: image/jpeg\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(b"\xff\xd8\xff\xe0fake");
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(sep.as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"parameter\"\r\n\r\n");
        body.extend_from_slice(param.as_bytes());
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(end.as_bytes());
        body
    }

    #[tokio::test]
    async fn test_scans_file_serves_existing_image() {
        let (server, _tmp) = test_server();
        let batch_id = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 0}))
            .await
            .json::<Value>()["batch_id"]
            .as_str()
            .unwrap()
            .to_string();
        let boundary = "testboundary123";
        let body = multipart_image(boundary, &batch_id, "page1.jpg");
        server
            .post("/NmWebService/image")
            .bytes(body.into())
            .content_type(&format!("multipart/form-data; boundary={boundary}"))
            .await;
        server.put(&format!("/NmWebService/batch/{batch_id}")).await;
        let resp = server
            .get(&format!("/scans/{batch_id}/files/page1.jpg"))
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_scans_file_rejects_dotdot_traversal() {
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
        // URL-encoded ".." — axum decodes this before routing
        let resp = server
            .get(&format!("/scans/{batch_id}/files/..%2Fetc%2Fpasswd"))
            .await;
        assert!(
            resp.status_code() == 400 || resp.status_code() == 404,
            "traversal attempt must not return 200, got {}",
            resp.status_code()
        );
    }

    #[tokio::test]
    async fn test_scans_file_does_not_serve_metadata_or_unlisted_files() {
        let (server, tmp) = test_server();
        let batch_id = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 0}))
            .await
            .json::<Value>()["batch_id"]
            .as_str()
            .unwrap()
            .to_string();
        std::fs::write(tmp.path().join(&batch_id).join("private.jpg"), b"secret").unwrap();
        assert_eq!(
            server
                .get(&format!("/scans/{batch_id}/files/metadata.json"))
                .await
                .status_code(),
            404
        );
        assert_eq!(
            server
                .get(&format!("/scans/{batch_id}/files/private.jpg"))
                .await
                .status_code(),
            404
        );
    }

    #[tokio::test]
    async fn test_scan_detail_shows_pending_paperless_delivery() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name":"Scan", "job_id":0}),
            scan_settings: json!({}),
        };
        let mut batch =
            crate::batch::Batch::create(&job, crate::batch::ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        batch
            .add_file(
                "page.jpg",
                include_bytes!("../../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        batch.complete().unwrap();
        let id = batch.id.clone();
        let server = TestServer::new(router(AppState::new(Config {
            jobs: vec![job],
            ..Default::default()
        })));
        let page = server.get(&format!("/scans/{id}")).await.text();
        assert!(page.contains("Paperless"));
        assert!(page.contains("wartet"));
    }

    #[tokio::test]
    async fn overdue_paperless_import_is_visible_on_dashboard_list_and_detail() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name": "Scan", "job_id": 0}),
            scan_settings: json!({}),
        };
        let mut batch =
            crate::batch::Batch::create(&job, crate::batch::ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        batch
            .add_file(
                "page.jpg",
                include_bytes!("../../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        batch.complete().unwrap();
        let id = batch.id.clone();
        let path = tmp.path().join(&id).join("metadata.json");
        let mut metadata: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        metadata["created_at"] = json!("2026-01-01T00:00:00Z");
        metadata["paperless"]["task_id"] = json!("6f421a77-2013-4a1b-a42f-e5b763d80126");
        metadata["paperless"]["upload_attempts"] = json!(2);
        crate::atomic_file::write(&path, &serde_json::to_vec(&metadata).unwrap()).unwrap();
        let server = TestServer::new(router(AppState::new(Config {
            jobs: vec![job],
            ..Default::default()
        })));
        let dashboard = server.get("/").await.text();
        assert!(dashboard.contains("Paperless-Importe brauchen Prüfung: 1"));
        let list = server.get("/scans").await.text();
        assert!(list.contains("data-paperless-attention=\"true\""));
        let detail = server.get(&format!("/scans/{id}")).await.text();
        assert!(detail.contains("Klärung nötig"));
        assert!(detail.contains("Upload-Versuche"));
        assert!(detail.contains("6f421a77-2013-4a1b-a42f-e5b763d80126"));
    }

    #[tokio::test]
    async fn scans_list_keeps_old_attention_batch_beyond_recent_limit() {
        let (server, tmp) = test_server();
        for index in 0..101 {
            let id = format!("{index:032x}");
            let dir = tmp.path().join(&id);
            std::fs::create_dir(&dir).unwrap();
            let metadata = json!({
                "job_name": "TestJob",
                "job_id": 0,
                "created_at": if index == 0 { "2026-01-01T00:00:00Z" } else { "2026-09-30T00:00:00Z" },
                "completed": true,
                "files": [],
                "paperless": if index == 0 { json!({"task_id": null, "document_id": null, "last_error": null}) } else { Value::Null }
            });
            std::fs::write(
                dir.join("metadata.json"),
                serde_json::to_vec(&metadata).unwrap(),
            )
            .unwrap();
        }
        let list = server.get("/scans").await.text();
        assert!(list.contains("/scans/00000000000000000000000000000000"));
        assert!(list.contains("Paperless-Importe brauchen Prüfung: 1"));
    }
}
