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
    corrupt: bool,
    delivery_pending: bool,
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
    let jobs = lock!(state.jobs).clone();
    let Some(dir) = find_batch_dir(&jobs, &batch_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (meta, corrupt) = super::ui::scan_metadata(&dir);
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
        corrupt,
        delivery_pending: meta
            .paperless
            .as_ref()
            .is_some_and(|r| r.document_id.is_none()),
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
        body.extend_from_slice(include_bytes!("../../tests/fixtures/scan_page.jpg"));
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
        let server = TestApp::new(AppState::new(Config {
            jobs: vec![job],
            ..Default::default()
        }));
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
        let server = TestApp::new(AppState::new(Config {
            jobs: vec![job],
            ..Default::default()
        }));
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

#[cfg(test)]
mod recovery_action_tests {
    use super::*;
    use crate::{
        admin_auth::AdminAuth,
        batch::{Batch, ScannerInfo},
        config::{Config, Job},
        state::AppState,
    };
    use axum_test::TestServer;
    use serde_json::json;
    #[tokio::test]
    async fn admin_can_complete_and_discard_after_memory_sweep_with_stepup() {
        let root = tempfile::tempdir().unwrap();
        let auth_dir = tempfile::tempdir().unwrap();
        let job = Job {
            output_path: root.path().to_owned(),
            consume_path: None,
            job_info: json!({"name":"test","job_id":0}),
            scan_settings: json!({}),
        };
        let first = Batch::create(&job, ScannerInfo::default()).unwrap();
        let second = Batch::create(&job, ScannerInfo::default()).unwrap();
        let auth = AdminAuth::open(auth_dir.path(), &[0x42; 32]).unwrap();
        let session = auth.test_session();
        let state = AppState::new(Config {
            jobs: vec![job],
            ..Config::default()
        })
        .with_admin_auth(auth.clone());
        crate::lock!(state.batches).clear();
        let server = TestServer::new(crate::routes::admin_router(state));
        let cookie = format!("__Host-nx-admin={session}");
        let url = format!("/scans/{}/complete", first.id);
        let denied = server
            .post(&url)
            .form(&[("unused", "")])
            .add_header("cookie", &cookie)
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .await;
        assert_eq!(denied.status_code(), 401);
        auth.test_confirm_stepup(&session);
        let done = server
            .post(&url)
            .form(&[("unused", "")])
            .add_header("cookie", &cookie)
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .await;
        assert_eq!(done.status_code(), 303);
        let meta: BatchMetadata = serde_json::from_slice(
            &std::fs::read(root.path().join(&first.id).join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert!(meta.completed);
        auth.test_confirm_stepup(&session);
        let discarded = server
            .post(&format!("/scans/{}/discard", second.id))
            .form(&[("unused", "")])
            .add_header("cookie", &cookie)
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .await;
        assert_eq!(discarded.status_code(), 303);
        assert!(!root.path().join(&second.id).exists());
    }
}

#[derive(serde::Deserialize, Default)]
pub struct RecoveryForm {
    #[serde(default)]
    confirm_duplicate: String,
    #[serde(default)]
    document_id: String,
}

pub async fn recover(
    Path((id, action)): Path<(String, String)>,
    UiState(state): UiState,
    headers: axum::http::HeaderMap,
    axum::Form(form): axum::Form<RecoveryForm>,
) -> Response {
    if !crate::batch::valid_batch_id(&id)
        || !matches!(action.as_str(), "complete" | "discard" | "retry" | "assign")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(auth) = state.admin_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !super::admin::secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !super::admin_cookie(&headers).is_some_and(|session| auth.consume_passkey_stepup(session)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if action == "retry" && form.confirm_duplicate != "yes" {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let document = if action == "assign" {
        match form.document_id.parse::<u64>() {
            Ok(id) if id > 0 => Some(id),
            _ => return StatusCode::BAD_REQUEST.into_response(),
        }
    } else {
        None
    };
    let lock = state.delivery_lock.clone();
    let _delivery = lock.lock().await;
    let task = state.clone();
    let batch_id = id.clone();
    let discard = action == "discard";
    let Ok(_pdf_slot) = state.pdf_slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let _change = crate::lock!(task.config_change_lock);
        let jobs = crate::lock!(task.jobs).clone();
        let dir =
            find_batch_dir(&jobs, &batch_id).ok_or_else(|| anyhow::anyhow!("scan not found"))?;
        let existing = crate::lock!(task.batches).get(&batch_id).cloned();
        if discard {
            let _active = existing.as_ref().map(|batch| crate::lock!(batch));
            let parent = dir.parent().expect("batch has parent");
            let tomb = parent.join(format!(".discarded-{batch_id}"));
            std::fs::rename(&dir, &tomb)?;
            crate::atomic_file::sync_dir(parent)?;
            std::fs::remove_dir_all(tomb)?;
            crate::atomic_file::sync_dir(parent)?;
            crate::lock!(task.batches).remove(&batch_id);
            return Ok(());
        }
        let batch = if let Some(existing) = existing {
            existing
        } else {
            let mut found = None;
            for job in &jobs {
                if let Some(batch) = crate::batch::Batch::open(job, &batch_id)? {
                    found = Some(batch);
                    break;
                }
            }
            let recovered = std::sync::Arc::new(std::sync::Mutex::new(
                found.ok_or_else(|| anyhow::anyhow!("scan not found"))?,
            ));
            crate::lock!(task.batches)
                .entry(batch_id.clone())
                .or_insert(recovered)
                .clone()
        };
        {
            let mut saved = crate::lock!(batch);
            match action.as_str() {
                "complete" => saved.complete()?,
                "retry" => saved.retry_paperless()?,
                "assign" => {
                    anyhow::ensure!(
                        saved.metadata().completed
                            && saved
                                .metadata()
                                .paperless
                                .as_ref()
                                .is_some_and(|r| r.document_id.is_none()),
                        "no pending delivery"
                    );
                    saved.record_paperless_document(document.expect("validated document"))?;
                }
                _ => unreachable!(),
            }
        }
        crate::lock!(task.batches).remove(&batch_id);
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => axum::response::Redirect::to(&if discard {
            "/scans".into()
        } else {
            format!("/scans/{id}")
        })
        .into_response(),
        _ => {
            let message = if state.translations.code == "de" {
                "Aktion fehlgeschlagen. Beschädigte Scandaten bitte prüfen oder verwerfen; für erneutes Senden die Duplikatwarnung bestätigen."
            } else {
                "Operation failed. Inspect or discard corrupt scan data; confirm the duplicate warning before retrying delivery."
            };
            (
                StatusCode::CONFLICT,
                axum::response::Html(format!(
                    "<p role=alert>{message}</p><a href=\"/scans/{id}\">Scan</a>"
                )),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod delivery_recovery_tests {
    use crate::{
        admin_auth::AdminAuth,
        batch::{Batch, BatchMetadata, ScannerInfo},
        config::{Config, Job},
        state::AppState,
    };
    use axum_test::{TestResponse, TestServer};
    use serde_json::json;
    async fn act(
        server: &TestServer,
        auth: &AdminAuth,
        session: &str,
        id: &str,
        operation: &str,
        fields: &[(&str, &str)],
        confirm: bool,
    ) -> TestResponse {
        if confirm {
            auth.test_confirm_stepup(session);
        }
        server
            .post(&format!("/scans/{id}/{operation}"))
            .form(&fields)
            .add_header("cookie", format!("__Host-nx-admin={session}"))
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .await
    }
    #[tokio::test]
    async fn retry_requires_warning_and_fresh_confirmation_then_manual_assignment_releases_delivery()
     {
        let root = tempfile::tempdir().unwrap();
        let auth_dir = tempfile::tempdir().unwrap();
        let job = Job {
            output_path: root.path().into(),
            consume_path: None,
            job_info: json!({"name":"test","job_id":0}),
            scan_settings: json!({}),
        };
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        batch
            .add_file(
                "page.jpg",
                include_bytes!("../../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        batch.complete().unwrap();
        batch.record_paperless_attempt().unwrap();
        batch.record_paperless_task("failed-task".into()).unwrap();
        batch.record_paperless_error("failed").unwrap();
        let auth = AdminAuth::open(auth_dir.path(), &[0x42; 32]).unwrap();
        let session = auth.test_session();
        let state = AppState::new(Config {
            jobs: vec![job.clone()],
            ..Config::default()
        })
        .with_admin_auth(auth.clone());
        let server = TestServer::new(crate::routes::admin_router(state));
        assert_eq!(
            act(
                &server,
                &auth,
                &session,
                &batch.id,
                "retry",
                &[("confirm_duplicate", "yes")],
                false
            )
            .await
            .status_code(),
            401
        );
        assert_eq!(
            act(
                &server,
                &auth,
                &session,
                &batch.id,
                "retry",
                &[("confirm_duplicate", "no")],
                true
            )
            .await
            .status_code(),
            400
        );
        assert_eq!(
            act(
                &server,
                &auth,
                &session,
                &batch.id,
                "retry",
                &[("confirm_duplicate", "yes")],
                true
            )
            .await
            .status_code(),
            303
        );
        let path = root.path().join(&batch.id).join("metadata.json");
        let meta: BatchMetadata = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let receipt = meta.paperless.unwrap();
        assert!(receipt.task_id.is_none());
        assert_eq!(receipt.upload_attempts, 0);
        assert!(receipt.last_error.is_none());
        assert_eq!(
            act(
                &server,
                &auth,
                &session,
                &batch.id,
                "assign",
                &[("document_id", "42")],
                false
            )
            .await
            .status_code(),
            401
        );
        assert_eq!(
            act(
                &server,
                &auth,
                &session,
                &batch.id,
                "assign",
                &[("document_id", "0")],
                true
            )
            .await
            .status_code(),
            400
        );
        assert_eq!(
            act(
                &server,
                &auth,
                &session,
                &batch.id,
                "assign",
                &[("document_id", "42")],
                true
            )
            .await
            .status_code(),
            303
        );
        assert!(!crate::paperless::pending_delivery_exists(&[job]).unwrap());
    }
    #[tokio::test]
    async fn corrupt_batch_is_visible_and_blocks_target_change_until_explicit_discard() {
        let root = tempfile::tempdir().unwrap();
        let auth_dir = tempfile::tempdir().unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        std::fs::create_dir(root.path().join(id)).unwrap();
        let job = Job {
            output_path: root.path().into(),
            consume_path: None,
            job_info: json!({"name":"test","job_id":0}),
            scan_settings: json!({}),
        };
        let error =
            crate::paperless::pending_delivery_exists(std::slice::from_ref(&job)).unwrap_err();
        assert!(error.to_string().contains("corrupt"));
        let auth = AdminAuth::open(auth_dir.path(), &[0x42; 32]).unwrap();
        let session = auth.test_session();
        let state = AppState::new(Config {
            jobs: vec![job.clone()],
            ..Config::default()
        })
        .with_admin_auth(auth.clone());
        let server = TestServer::new(crate::routes::admin_router(state));
        let page = server
            .get(&format!("/scans/{id}"))
            .add_header("cookie", format!("__Host-nx-admin={session}"))
            .await;
        assert_eq!(page.status_code(), 200);
        assert!(page.text().contains("Beschädigte Scandaten"));
        assert_eq!(
            act(&server, &auth, &session, id, "discard", &[], false)
                .await
                .status_code(),
            401
        );
        assert_eq!(
            act(&server, &auth, &session, id, "discard", &[], true)
                .await
                .status_code(),
            303
        );
        assert!(!crate::paperless::pending_delivery_exists(&[job]).unwrap());
    }
}
