use askama::Template;
use axum::{
    Form,
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use tracing::warn;

use crate::batch::{BatchMetadata, JobConfig};
use crate::config::Job;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Template rendering helper
// ---------------------------------------------------------------------------

fn render<T: Template>(t: T) -> Response {
    match t.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Data types used by templates
// ---------------------------------------------------------------------------

pub struct ScanEntry {
    pub batch_id: String,
    pub job_name: String,
    pub created_at: String,
    pub page_count: usize,
    pub completed: bool,
    pub resolution: u32,
    pub pixel_format: String,
    pub duplex: bool,
    pub scanner_model: Option<String>,
}

pub struct JobRow {
    pub id: usize,
    pub name: String,
    pub color: String,
    pub output_path: String,
    pub resolution: u32,
    pub pixel_format: String,
    pub duplex: bool,
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTpl {
    #[allow(dead_code)]
    scanner_online: bool,
    scanner_name: String,
    scanner_model: Option<String>,
    scanner_serial: Option<String>,
    recent_scans: Vec<ScanEntry>,
    jobs: Vec<JobRow>,
}

#[derive(Template)]
#[template(path = "scanner_status.html")]
struct ScannerStatusTpl {
    online: bool,
    name: String,
    model: Option<String>,
}

#[derive(Template)]
#[template(path = "jobs_list.html")]
struct JobsListTpl {
    jobs: Vec<JobRow>,
}

#[derive(Template)]
#[template(path = "jobs_form.html")]
struct JobsFormTpl {
    editing: bool,
    job_id: usize,
    name: String,
    color: String,
    output_path: String,
    consume_path: String,
    resolution: u32,
    jpeg_quality: u8,
    pixel_format: String,
    duplex: bool,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "scans_list.html")]
struct ScansListTpl {
    scans: Vec<ScanEntry>,
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
    duplex: bool,
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn list_scans(jobs: &[Job], limit: usize) -> Vec<ScanEntry> {
    let mut scans = Vec::new();
    for job in jobs {
        if let Ok(entries) = std::fs::read_dir(&job.output_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let meta_path = path.join("metadata.json");
                if let Ok(content) = std::fs::read_to_string(&meta_path)
                    && let Ok(meta) = serde_json::from_str::<BatchMetadata>(&content)
                {
                    let batch_id = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    scans.push(ScanEntry {
                        batch_id,
                        job_name: meta.job_name,
                        created_at: meta.created_at,
                        page_count: meta.files.len(),
                        completed: meta.completed,
                        resolution: meta.job_config.resolution,
                        pixel_format: meta.job_config.pixel_format,
                        duplex: meta.job_config.duplex,
                        scanner_model: meta.scanner.model,
                    });
                }
            }
        }
    }
    scans.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    scans.truncate(limit);
    scans
}

fn find_batch_dir(jobs: &[Job], batch_id: &str) -> Option<std::path::PathBuf> {
    // Guard against path traversal: batch_id must be a plain hex UUID
    if !batch_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    for job in jobs {
        let dir = job.output_path.join(batch_id);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    None
}

fn job_rows(jobs: &[Job]) -> Vec<JobRow> {
    jobs.iter()
        .enumerate()
        .map(|(i, j)| JobRow {
            id: i,
            name: j.name().to_string(),
            color: j.color().to_string(),
            output_path: j.output_path.to_string_lossy().to_string(),
            resolution: j.resolution(),
            pixel_format: j.pixel_format().to_string(),
            duplex: j.duplex(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Handlers — Dashboard
// ---------------------------------------------------------------------------

pub async fn dashboard(State(state): State<AppState>) -> Response {
    let online = state.scanner_is_online();
    let name = state.scanner_display_name();
    let model = state.scanner_display_model();
    let serial = state.scanner_display_serial();
    let jobs = state.jobs.lock().unwrap();
    let recent_scans = list_scans(&jobs, 10);
    let job_rows = job_rows(&jobs);
    render(DashboardTpl {
        scanner_online: online,
        scanner_name: name,
        scanner_model: model,
        scanner_serial: serial,
        recent_scans,
        jobs: job_rows,
    })
}

pub async fn scanner_status(State(state): State<AppState>) -> Response {
    render(ScannerStatusTpl {
        online: state.scanner_is_online(),
        name: state.scanner_display_name(),
        model: state.scanner_display_model(),
    })
}

// ---------------------------------------------------------------------------
// Handlers — Jobs CRUD
// ---------------------------------------------------------------------------

pub async fn jobs_list(State(state): State<AppState>) -> Response {
    let jobs = state.jobs.lock().unwrap();
    render(JobsListTpl {
        jobs: job_rows(&jobs),
    })
}

pub async fn jobs_new() -> Response {
    render(JobsFormTpl {
        editing: false,
        job_id: 0,
        name: String::new(),
        color: "#4D4D4D".to_string(),
        output_path: String::new(),
        consume_path: String::new(),
        resolution: 300,
        jpeg_quality: 80,
        pixel_format: "rgb24".to_string(),
        duplex: true,
        error: None,
    })
}

#[derive(Deserialize)]
pub struct JobFormData {
    pub name: String,
    pub color: String,
    pub output_path: String,
    #[serde(default)]
    pub consume_path: String,
    pub resolution: u32,
    pub jpeg_quality: u8,
    pub pixel_format: String,
    // HTML select sends "true"/"false" strings
    #[serde(default = "default_duplex")]
    pub duplex: String,
}

fn default_duplex() -> String {
    "true".to_string()
}

pub async fn jobs_create(State(state): State<AppState>, Form(form): Form<JobFormData>) -> Response {
    let yaml = job_form_to_yaml(&form);
    match crate::config::Config::parse(&yaml) {
        Ok(parsed) => {
            if parsed.jobs.is_empty() {
                return render(form_to_tpl(
                    false,
                    0,
                    &form,
                    Some("Ungültige Konfiguration".to_string()),
                ));
            }
            let new_job = parsed.jobs.into_iter().next().unwrap();
            {
                let mut jobs = state.jobs.lock().unwrap();
                let id = jobs.len();
                // fix job_id to actual position
                let mut info = new_job.job_info.clone();
                info["job_id"] = serde_json::json!(id);
                let job = Job {
                    job_info: info,
                    ..new_job
                };
                jobs.push(job);
                if let Some(ref path) = state.config_path
                    && let Err(e) = crate::config::Config::save(&jobs, path)
                {
                    warn!(error = %e, "failed to save config");
                }
            }
            Redirect::to("/jobs").into_response()
        }
        Err(e) => render(form_to_tpl(false, 0, &form, Some(e.to_string()))),
    }
}

pub async fn jobs_edit(Path(id): Path<usize>, State(state): State<AppState>) -> Response {
    let jobs = state.jobs.lock().unwrap();
    let Some(job) = jobs.get(id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    render(JobsFormTpl {
        editing: true,
        job_id: id,
        name: job.name().to_string(),
        color: job.color().to_string(),
        output_path: job.output_path.to_string_lossy().to_string(),
        consume_path: job
            .consume_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        resolution: job.resolution(),
        jpeg_quality: job.jpeg_quality(),
        pixel_format: job.pixel_format().to_string(),
        duplex: job.duplex(),
        error: None,
    })
}

pub async fn jobs_update(
    Path(id): Path<usize>,
    State(state): State<AppState>,
    Form(form): Form<JobFormData>,
) -> Response {
    let yaml = job_form_to_yaml(&form);
    match crate::config::Config::parse(&yaml) {
        Ok(parsed) => {
            if parsed.jobs.is_empty() {
                return render(form_to_tpl(
                    true,
                    id,
                    &form,
                    Some("Ungültige Konfiguration".to_string()),
                ));
            }
            let updated = parsed.jobs.into_iter().next().unwrap();
            {
                let mut jobs = state.jobs.lock().unwrap();
                if id >= jobs.len() {
                    return StatusCode::NOT_FOUND.into_response();
                }
                let mut info = updated.job_info.clone();
                info["job_id"] = serde_json::json!(id);
                jobs[id] = Job {
                    job_info: info,
                    ..updated
                };
                if let Some(ref path) = state.config_path
                    && let Err(e) = crate::config::Config::save(&jobs, path)
                {
                    warn!(error = %e, "failed to save config");
                }
            }
            Redirect::to("/jobs").into_response()
        }
        Err(e) => render(form_to_tpl(true, id, &form, Some(e.to_string()))),
    }
}

pub async fn jobs_delete(Path(id): Path<usize>, State(state): State<AppState>) -> Response {
    let mut jobs = state.jobs.lock().unwrap();
    if id >= jobs.len() {
        return StatusCode::NOT_FOUND.into_response();
    }
    jobs.remove(id);
    // Re-assign job_ids to keep them contiguous
    for (i, job) in jobs.iter_mut().enumerate() {
        job.job_info["job_id"] = serde_json::json!(i);
    }
    if let Some(ref path) = state.config_path
        && let Err(e) = crate::config::Config::save(&jobs, path)
    {
        warn!(error = %e, "failed to save config after delete");
    }
    // HTMX: respond with updated jobs list fragment; plain browser: redirect
    Redirect::to("/jobs").into_response()
}

// ---------------------------------------------------------------------------
// Handlers — Scans browser
// ---------------------------------------------------------------------------

pub async fn scans_list(State(state): State<AppState>) -> Response {
    let jobs = state.jobs.lock().unwrap();
    let scans = list_scans(&jobs, 100);
    render(ScansListTpl { scans })
}

pub async fn scans_detail(Path(batch_id): Path<String>, State(state): State<AppState>) -> Response {
    let jobs = state.jobs.lock().unwrap();
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
        duplex,
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
        duplex,
    })
}

pub async fn scans_file(
    Path((batch_id, filename)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    let jobs = state.jobs.lock().unwrap();
    let Some(dir) = find_batch_dir(&jobs, &batch_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Guard against path traversal: filename must not contain separators
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

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn job_form_to_yaml(form: &JobFormData) -> String {
    let consume_line = if form.consume_path.trim().is_empty() {
        String::new()
    } else {
        format!("    consume_path: {}\n", form.consume_path.trim())
    };
    let source = if form.duplex == "true" {
        "feeder"
    } else {
        "feederFront"
    };
    format!(
        "jobs:\n  {}:\n    output_path: {}\n{}    color: '{}'\n    scan_settings:\n      source: {}\n      pixelFormats:\n        resolution: {}\n        jpegQuality: {}\n        pixelFormat: {}\n",
        form.name.trim(),
        form.output_path.trim(),
        consume_line,
        form.color,
        source,
        form.resolution,
        form.jpeg_quality,
        form.pixel_format,
    )
}

fn form_to_tpl(
    editing: bool,
    job_id: usize,
    form: &JobFormData,
    error: Option<String>,
) -> JobsFormTpl {
    JobsFormTpl {
        editing,
        job_id,
        error,
        name: form.name.clone(),
        color: form.color.clone(),
        output_path: form.output_path.clone(),
        consume_path: form.consume_path.clone(),
        resolution: form.resolution,
        jpeg_quality: form.jpeg_quality,
        pixel_format: form.pixel_format.clone(),
        duplex: form.duplex == "true",
    }
}

// ---------------------------------------------------------------------------
// Tests (RED first)
// ---------------------------------------------------------------------------

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
        };
        let server = TestServer::new(router(AppState::new(config)));
        (server, tmp)
    }

    // --- Dashboard ---

    #[tokio::test]
    async fn test_dashboard_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_dashboard_shows_job_name() {
        let (server, _tmp) = test_server();
        assert!(server.get("/").await.text().contains("TestJob"));
    }

    #[tokio::test]
    async fn test_scanner_status_offline_initially() {
        let (server, _tmp) = test_server();
        let body = server.get("/api/scanner-status").await.text();
        assert!(body.contains("offline"));
    }

    #[tokio::test]
    async fn test_scanner_status_online_after_heartbeat() {
        let (server, _tmp) = test_server();
        server.get("/NmWebService/heartbeat").await;
        let body = server.get("/api/scanner-status").await.text();
        assert!(body.contains("online"));
    }

    // --- Jobs ---

    #[tokio::test]
    async fn test_jobs_list_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/jobs").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_jobs_list_shows_job_name() {
        let (server, _tmp) = test_server();
        assert!(server.get("/jobs").await.text().contains("TestJob"));
    }

    #[tokio::test]
    async fn test_jobs_new_form_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/jobs/new").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_jobs_edit_form_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/jobs/0/edit").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_jobs_edit_form_prefills_name() {
        let (server, _tmp) = test_server();
        assert!(server.get("/jobs/0/edit").await.text().contains("TestJob"));
    }

    #[tokio::test]
    async fn test_jobs_edit_unknown_returns_404() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/jobs/99/edit").await.status_code(), 404);
    }

    #[tokio::test]
    async fn test_jobs_create_redirects() {
        let (server, tmp) = test_server();
        let out = tmp.path().join("new");
        std::fs::create_dir_all(&out).unwrap();
        let resp = server
            .post("/jobs")
            .form(&[
                ("name", "NewJob"),
                ("output_path", out.to_str().unwrap()),
                ("color", "#ff0000"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        assert_eq!(resp.status_code(), 303);
    }

    #[tokio::test]
    async fn test_jobs_create_appears_in_list() {
        let (server, tmp) = test_server();
        let out = tmp.path().join("new2");
        std::fs::create_dir_all(&out).unwrap();
        server
            .post("/jobs")
            .form(&[
                ("name", "CreatedJob"),
                ("output_path", out.to_str().unwrap()),
                ("color", "#00ff00"),
                ("resolution", "600"),
                ("jpeg_quality", "90"),
                ("pixel_format", "gray8"),
                ("consume_path", ""),
            ])
            .await;
        assert!(server.get("/jobs").await.text().contains("CreatedJob"));
    }

    #[tokio::test]
    async fn test_jobs_update_redirects() {
        let (server, tmp) = test_server();
        let resp = server
            .post("/jobs/0")
            .form(&[
                ("name", "UpdatedJob"),
                ("output_path", tmp.path().to_str().unwrap()),
                ("color", "#0000ff"),
                ("resolution", "600"),
                ("jpeg_quality", "95"),
                ("pixel_format", "gray8"),
                ("consume_path", ""),
            ])
            .await;
        assert_eq!(resp.status_code(), 303);
    }

    #[tokio::test]
    async fn test_jobs_update_reflected_in_list() {
        let (server, tmp) = test_server();
        server
            .post("/jobs/0")
            .form(&[
                ("name", "RenamedJob"),
                ("output_path", tmp.path().to_str().unwrap()),
                ("color", "#0000ff"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        assert!(server.get("/jobs").await.text().contains("RenamedJob"));
    }

    #[tokio::test]
    async fn test_jobs_delete_redirects() {
        let (server, _tmp) = test_server();
        let resp = server.delete("/jobs/0").await;
        assert_eq!(resp.status_code(), 303);
    }

    #[tokio::test]
    async fn test_jobs_delete_removes_from_list() {
        let (server, _tmp) = test_server();
        server.delete("/jobs/0").await;
        assert!(!server.get("/jobs").await.text().contains("TestJob"));
    }

    // --- Scans ---

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
