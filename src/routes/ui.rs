use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

use crate::batch::BatchMetadata;
use crate::build_info::BuildInfo;
use crate::config::Job;
use crate::lock;
use crate::state::AppState;
use crate::translations::Translations;

// ---------------------------------------------------------------------------
// Template rendering helper (shared by all UI sub-modules)
// ---------------------------------------------------------------------------

pub fn render<T: Template>(t: T) -> Response {
    match t.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Shared data types
// ---------------------------------------------------------------------------

pub struct ScanEntry {
    pub batch_id: String,
    pub job_name: String,
    pub created_at: String,
    pub page_count: usize,
    pub completed: bool,
    pub resolution: u32,
    pub pixel_format: String,
    pub source: String,
    pub scanner_model: Option<String>,
}

pub struct JobRow {
    pub id: usize,
    pub name: String,
    pub color: String,
    pub output_path: String,
    pub resolution: u32,
    pub pixel_format: String,
    pub source: String,
}

// ---------------------------------------------------------------------------
// Shared filesystem helpers
// ---------------------------------------------------------------------------

pub fn list_scans(jobs: &[Job], limit: usize) -> Vec<ScanEntry> {
    let mut scans = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    for job in jobs {
        if !seen_paths.insert(&job.output_path) {
            continue;
        }
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
                        source: meta.job_config.source,
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

/// Returns the batch directory for `batch_id` across all job output paths.
/// Returns `None` if `batch_id` is not a pure hex string (path traversal guard).
pub fn find_batch_dir(jobs: &[Job], batch_id: &str) -> Option<std::path::PathBuf> {
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

pub fn job_rows(jobs: &[Job]) -> Vec<JobRow> {
    jobs.iter()
        .enumerate()
        .map(|(i, j)| JobRow {
            id: i,
            name: j.name().to_string(),
            color: j.color().to_string(),
            output_path: j.output_path.to_string_lossy().to_string(),
            resolution: j.resolution(),
            pixel_format: j.pixel_format().to_string(),
            source: j.source().to_string(),
        })
        .collect()
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
    t: &'static Translations,
    build: &'static BuildInfo,
}

#[derive(Template)]
#[template(path = "scanner_status.html")]
struct ScannerStatusTpl {
    online: bool,
    name: String,
    model: Option<String>,
    t: &'static Translations,
}

// ---------------------------------------------------------------------------
// Handlers — Dashboard
// ---------------------------------------------------------------------------

pub async fn dashboard(State(state): State<AppState>) -> Response {
    let online = state.scanner.is_online();
    let name = state.scanner.display_name();
    let model = state.scanner.display_model();
    let serial = state.scanner.display_serial();
    let jobs = lock!(state.jobs);
    let recent_scans = list_scans(&jobs, 10);
    let job_rows = job_rows(&jobs);
    render(DashboardTpl {
        scanner_online: online,
        scanner_name: name,
        scanner_model: model,
        scanner_serial: serial,
        recent_scans,
        jobs: job_rows,
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn scanner_status(State(state): State<AppState>) -> Response {
    render(ScannerStatusTpl {
        online: state.scanner.is_online(),
        name: state.scanner.display_name(),
        model: state.scanner.display_model(),
        t: state.translations,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use axum_test::TestServer;

    use crate::config::{Config, Job};
    use crate::routes::router;
    use crate::state::AppState;

    fn test_server() -> (TestServer, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: serde_json::json!({
                    "name": "TestJob", "job_id": 0, "color": "#4D4D4D",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: serde_json::json!({}),
            }],
            ..Default::default()
        };
        (TestServer::new(router(AppState::new(config))), tmp)
    }

    #[test]
    fn test_list_scans_deduplicates_shared_output_path() {
        use super::list_scans;
        let tmp = tempfile::TempDir::new().unwrap();
        let make_job = |id: usize, name: &str| Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: serde_json::json!({
                "name": name, "job_id": id, "color": "#4D4D4D",
                "type": 0, "job_setting": {}, "hierarchy_list": null
            }),
            scan_settings: serde_json::json!({}),
        };

        // One batch directory shared by both jobs
        let batch_dir = tmp.path().join("deadbeefdeadbeefdeadbeefdeadbeef");
        std::fs::create_dir_all(&batch_dir).unwrap();
        std::fs::write(
            batch_dir.join("metadata.json"),
            serde_json::to_string(&serde_json::json!({
                "job_name": "Job1",
                "created_at": "2026-04-27T19:57:39+00:00",
                "completed": true,
                "files": [],
                "scanner": {},
                "job_config": {
                    "resolution": 300, "pixel_format": "rgb24",
                    "jpeg_quality": 80, "source": "feeder"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let jobs = vec![make_job(0, "Job1"), make_job(1, "Job2")];
        let scans = list_scans(&jobs, 100);
        assert_eq!(
            scans.len(),
            1,
            "shared output_path must not produce duplicate entries"
        );
    }

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

    fn server_with_source(source: &str) -> (TestServer, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: serde_json::json!({
                    "name": "SourceJob", "job_id": 0, "color": "#4D4D4D",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: serde_json::json!({
                    "parameters": {
                        "task": {
                            "actions": {
                                "streams": {
                                    "sources": { "source": source }
                                }
                            }
                        }
                    }
                }),
            }],
            ..Default::default()
        };
        (TestServer::new(router(AppState::new(config))), tmp)
    }

    #[tokio::test]
    async fn test_dashboard_feeder_shows_duplex() {
        let (server, _tmp) = server_with_source("feeder");
        let body = server.get("/").await.text();
        assert!(
            body.contains("Doppelseitig"),
            "feeder must show Doppelseitig"
        );
    }

    #[tokio::test]
    async fn test_dashboard_feeder_front_shows_front_label() {
        let (server, _tmp) = server_with_source("feederFront");
        let body = server.get("/").await.text();
        assert!(
            body.contains("Einseitig (Vorderseite)"),
            "feederFront must show Einseitig (Vorderseite), got: {}",
            &body[body.find("SourceJob").unwrap_or(0)..][..200.min(body.len())]
        );
    }

    #[tokio::test]
    async fn test_dashboard_feeder_rear_shows_rear_label() {
        let (server, _tmp) = server_with_source("feederRear");
        let body = server.get("/").await.text();
        assert!(
            body.contains("Einseitig (R\u{fc}ckseite)"),
            "feederRear must show Einseitig (Rückseite)"
        );
    }
}
