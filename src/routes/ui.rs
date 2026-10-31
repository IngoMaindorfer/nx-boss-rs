use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use chrono::{DateTime, Utc};

use super::UiState;
use crate::batch::{BatchMetadata, valid_batch_id};
use crate::build_info::BuildInfo;
use crate::config::Job;
use crate::event_log::EventRecord;
use crate::lock;
use crate::state::JobStore;
use crate::translations::Translations;

// ---------------------------------------------------------------------------
// Template rendering helper (shared by all UI sub-modules)
// ---------------------------------------------------------------------------

pub fn render<T: Template>(t: T) -> Response {
    match t.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => {
            tracing::error!(
                event = "ui.template_failed",
                error_class = "template",
                "template rendering failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
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
    pub corrupt: bool,
    pub resolution: u32,
    pub pixel_format: String,
    pub source: String,
    pub scanner_model: Option<String>,
    pub paperless_status: Option<String>,
    pub paperless_attention: bool,
    pub paperless_imported: bool,
}

#[derive(Default)]
pub struct ScanStats {
    pub documents: usize,
    pub pages: usize,
    pub paperless_imported: usize,
    pub needs_attention: usize,
    pub open_batches: usize,
}

impl ScanStats {
    pub fn from_scans(scans: &[ScanEntry]) -> Self {
        let mut stats = Self::default();
        for scan in scans {
            if scan.completed {
                stats.documents += 1;
                stats.pages += scan.page_count;
            } else if !scan.corrupt {
                stats.open_batches += 1;
            }
            stats.paperless_imported += usize::from(scan.paperless_imported);
            stats.needs_attention += usize::from(scan.paperless_attention);
        }
        stats
    }
}

pub struct JobRow {
    pub id: usize,
    pub name: String,
    pub color: String,
    pub resolution: u32,
    pub pixel_format: String,
    pub source: String,
}

// ---------------------------------------------------------------------------
// Shared filesystem helpers
// ---------------------------------------------------------------------------

pub fn paperless_status(meta: &BatchMetadata, t: &Translations) -> Option<String> {
    if !meta.completed && paperless_needs_attention(meta) {
        return Some(t.paperless_attention.into());
    }
    let receipt = meta.paperless.as_ref()?;
    Some(if let Some(id) = receipt.document_id {
        format!("{} (#{id})", t.paperless_imported)
    } else if paperless_is_overdue(meta, Utc::now()) {
        t.paperless_attention.to_string()
    } else if receipt.last_error.is_some() {
        t.paperless_failed.to_string()
    } else if receipt.task_id.is_some() {
        t.paperless_processing.to_string()
    } else {
        t.paperless_pending.to_string()
    })
}

fn paperless_is_overdue(meta: &BatchMetadata, now: DateTime<Utc>) -> bool {
    let Some(receipt) = meta.paperless.as_ref() else {
        return false;
    };
    if !meta.completed || receipt.document_id.is_some() {
        return false;
    }
    let since = receipt
        .last_attempt_at
        .as_deref()
        .unwrap_or(&meta.created_at);
    DateTime::parse_from_rfc3339(since)
        .map(|then| now.signed_duration_since(then).num_hours() >= 1)
        .unwrap_or(true)
}

pub(super) fn paperless_needs_attention(meta: &BatchMetadata) -> bool {
    if !meta.completed {
        return DateTime::parse_from_rfc3339(&meta.created_at)
            .map(|then| Utc::now().signed_duration_since(then).num_hours() >= 1)
            .unwrap_or(true);
    }
    meta.paperless.as_ref().is_some_and(|receipt| {
        receipt.document_id.is_none()
            && (receipt.last_error.is_some() || paperless_is_overdue(meta, Utc::now()))
    })
}

pub(super) fn scan_metadata(path: &std::path::Path) -> (BatchMetadata, bool) {
    match crate::batch::Batch::inspect_directory(path) {
        Ok(meta) => (meta, false),
        Err(_) => {
            let created_at = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .map(DateTime::<Utc>::from)
                .unwrap_or_else(|_| Utc::now())
                .to_rfc3339();
            (
                BatchMetadata {
                    job_name: "—".into(),
                    job_id: None,
                    created_at,
                    completed: false,
                    files: vec![],
                    scanner: Default::default(),
                    job_config: Default::default(),
                    paperless: None,
                },
                true,
            )
        }
    }
}

pub fn list_scans(jobs: &[Job], limit: usize, t: &Translations) -> Vec<ScanEntry> {
    let mut scans = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    for job in jobs {
        if !seen_paths.insert(&job.output_path) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&job.output_path) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let path = entry.path();
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|id| valid_batch_id(id))
            else {
                continue;
            };
            let (meta, corrupt) = scan_metadata(&path);
            scans.push(ScanEntry {
                batch_id: id.into(),
                job_name: meta.job_name.clone(),
                created_at: meta.created_at.clone(),
                page_count: meta.files.len(),
                completed: meta.completed,
                corrupt,
                resolution: meta.job_config.resolution,
                pixel_format: meta.job_config.pixel_format.clone(),
                source: meta.job_config.source.clone(),
                scanner_model: meta.scanner.model.clone(),
                paperless_status: paperless_status(&meta, t),
                paperless_attention: corrupt || paperless_needs_attention(&meta),
                paperless_imported: meta
                    .paperless
                    .as_ref()
                    .is_some_and(|r| r.document_id.is_some()),
            });
        }
    }
    scans.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    scans.truncate(limit);
    scans
}

pub async fn list_scans_without_holding_jobs_lock(
    store: &JobStore,
    t: &'static Translations,
) -> Result<(Vec<ScanEntry>, Vec<JobRow>), tokio::task::JoinError> {
    let jobs = lock!(store).clone();
    tokio::task::spawn_blocking(move || {
        let rows = job_rows(&jobs);
        let scans = list_scans(&jobs, usize::MAX, t);
        (scans, rows)
    })
    .await
}

/// Returns the batch directory for `batch_id` across all job output paths.
/// Returns `None` if `batch_id` is not a pure hex string (path traversal guard).
pub fn find_batch_dir(jobs: &[Job], batch_id: &str) -> Option<std::path::PathBuf> {
    if !valid_batch_id(batch_id) {
        return None;
    }
    for job in jobs {
        let dir = job.output_path.join(batch_id);
        if dir
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_dir())
        {
            return Some(dir);
        }
    }
    None
}

#[cfg(test)]
mod batch_directory_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_canonical_batch_ids_are_visible_in_scan_list_and_detail() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name": "test", "job_id": 0}),
            scan_settings: json!({}),
        };
        let valid_id = "a".repeat(32);
        for id in ["deadbeef", valid_id.as_str()] {
            let dir = tmp.path().join(id);
            std::fs::create_dir(&dir).unwrap();
            let meta = crate::batch::BatchMetadata {
                job_name: "test".into(),
                job_id: Some(0),
                created_at: "2026-09-30T00:00:00+00:00".into(),
                completed: true,
                files: vec![],
                scanner: Default::default(),
                job_config: Default::default(),
                paperless: None,
            };
            std::fs::write(
                dir.join("metadata.json"),
                serde_json::to_vec(&meta).unwrap(),
            )
            .unwrap();
        }
        assert!(find_batch_dir(std::slice::from_ref(&job), "deadbeef").is_none());
        assert!(find_batch_dir(std::slice::from_ref(&job), &valid_id).is_some());
        let scans = list_scans(&[job], 10, crate::translations::for_lang("en"));
        assert_eq!(scans.len(), 1);
        assert_eq!(scans[0].batch_id, valid_id);
    }

    #[cfg(unix)]
    #[test]
    fn scan_list_does_not_follow_symlinked_batch_directory() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let scans = tmp.path().join("scans");
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&scans).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(
            outside.join("metadata.json"),
            serde_json::to_vec(&crate::batch::BatchMetadata {
                job_name: "outside".into(),
                job_id: Some(0),
                created_at: "2026-09-30T00:00:00Z".into(),
                completed: true,
                files: vec![],
                scanner: Default::default(),
                job_config: Default::default(),
                paperless: None,
            })
            .unwrap(),
        )
        .unwrap();
        symlink(&outside, scans.join("a".repeat(32))).unwrap();
        let job = Job {
            output_path: scans,
            consume_path: None,
            job_info: json!({"name": "test", "job_id": 0}),
            scan_settings: json!({}),
        };
        assert!(find_batch_dir(std::slice::from_ref(&job), &"a".repeat(32)).is_none());
        assert!(list_scans(&[job], 10, crate::translations::for_lang("en")).is_empty());
    }
}

pub fn job_rows(jobs: &[Job]) -> Vec<JobRow> {
    jobs.iter()
        .map(|j| JobRow {
            id: j.job_info["job_id"].as_u64().unwrap_or(0) as usize,
            name: j.name().to_string(),
            color: j.color().to_string(),
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
    stats: ScanStats,
    recent_error_count: usize,
    paperless_attention_count: usize,
    jobs: Vec<JobRow>,
    t: &'static Translations,
    build: &'static BuildInfo,
}

#[derive(Template)]
#[template(path = "events.html")]
struct EventsTpl {
    events: Vec<EventRecord>,
    persistence_ok: bool,
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

pub async fn dashboard(UiState(state): UiState) -> Response {
    let online = state.scanner.is_online();
    let name = state.scanner.display_name();
    let model = state.scanner.display_model();
    let serial = state.scanner.display_serial();
    let Ok((all_scans, job_rows)) =
        list_scans_without_holding_jobs_lock(&state.jobs, state.translations).await
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let stats = ScanStats::from_scans(&all_scans);
    let paperless_attention_count = stats.needs_attention;
    let recent_scans = all_scans.into_iter().take(10).collect();
    let recent_error_count = state
        .event_log
        .recent()
        .iter()
        .filter(|event| event.level == "ERROR")
        .count();
    render(DashboardTpl {
        scanner_online: online,
        scanner_name: name,
        scanner_model: model,
        scanner_serial: serial,
        recent_scans,
        stats,
        recent_error_count,
        paperless_attention_count,
        jobs: job_rows,
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn events(UiState(state): UiState) -> Response {
    render(EventsTpl {
        events: state.event_log.recent(),
        persistence_ok: state.event_log.persistence_ok(),
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn scanner_status(UiState(state): UiState) -> Response {
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
    use super::paperless_status;
    use axum_test::TestServer;

    use crate::config::{Config, Job};
    use crate::routes::test_support::TestApp;
    use crate::state::AppState;

    #[test]
    fn dashboard_counts_completed_documents_pages_and_import_outcomes() {
        let scan = |completed, pages, imported, attention| super::ScanEntry {
            batch_id: "a".repeat(32),
            job_name: "Scan".into(),
            created_at: "2026-09-30T00:00:00Z".into(),
            page_count: pages,
            completed,
            corrupt: false,
            resolution: 300,
            pixel_format: "rgb24".into(),
            source: "feeder".into(),
            scanner_model: None,
            paperless_status: None,
            paperless_attention: attention,
            paperless_imported: imported,
        };
        let scans = [
            scan(true, 3, true, false),
            scan(true, 2, false, true),
            scan(false, 1, false, false),
        ];
        let stats = super::ScanStats::from_scans(&scans);
        assert_eq!(stats.documents, 2);
        assert_eq!(stats.pages, 5);
        assert_eq!(stats.paperless_imported, 1);
        assert_eq!(stats.needs_attention, 1);
        assert_eq!(stats.open_batches, 1);
    }

    #[test]
    fn old_unconfirmed_paperless_receipt_needs_attention() {
        let mut meta = crate::batch::BatchMetadata {
            job_name: "Scan".into(),
            job_id: Some(0),
            created_at: "2026-01-01T00:00:00Z".into(),
            completed: true,
            files: vec![],
            scanner: Default::default(),
            job_config: Default::default(),
            paperless: Some(Default::default()),
        };
        let de = crate::translations::for_lang("de");
        assert_eq!(
            paperless_status(&meta, de).as_deref(),
            Some(de.paperless_attention)
        );
        meta.paperless.as_mut().unwrap().document_id = Some(42);
        assert_eq!(
            paperless_status(&meta, de).as_deref(),
            Some("importiert (#42)")
        );
    }

    #[test]
    fn recent_batches_are_quiet_and_stale_open_batches_need_attention() {
        let mut meta = crate::batch::BatchMetadata {
            job_name: "Scan".into(),
            job_id: Some(0),
            created_at: chrono::Utc::now().to_rfc3339(),
            completed: true,
            files: vec![],
            scanner: Default::default(),
            job_config: Default::default(),
            paperless: Some(Default::default()),
        };
        assert!(!super::paperless_needs_attention(&meta));
        meta.created_at = "2026-01-01T00:00:00Z".into();
        meta.completed = false;
        assert!(super::paperless_needs_attention(&meta));
        meta.completed = true;
        meta.paperless.as_mut().unwrap().last_attempt_at = Some(chrono::Utc::now().to_rfc3339());
        assert!(!super::paperless_needs_attention(&meta));
    }

    fn test_server() -> (TestApp, tempfile::TempDir) {
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
        (TestApp::new(AppState::new(config)), tmp)
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
        let scans = list_scans(&jobs, 100, crate::translations::for_lang("de"));
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
    async fn operational_events_are_browsable_only_on_admin_routes() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = crate::event_log::EventLog::default();
        journal.attach(tmp.path().join("events.json")).unwrap();
        journal.record(
            "ERROR",
            "paperless.delivery_failed",
            Some(&"a".repeat(32)),
            Some("http_status"),
            Some(503),
        );
        let state = AppState::new(Config::default()).with_event_log(journal);
        let admin = TestApp::new(state.clone());
        let html = admin.get("/events").await.text();
        assert!(html.contains("paperless.delivery_failed"));
        assert!(html.contains("503"));
        assert!(html.contains("/scans/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(!html.contains("PRIVATE_API_TOKEN"));
        let scanner = TestServer::new(crate::routes::scanner_router(state));
        assert_eq!(scanner.get("/events").await.status_code(), 404);
    }

    #[tokio::test]
    async fn pixel_formats_are_localized_on_dashboard_jobs_and_scan_details() {
        for (format, german, english) in [
            ("gray8", "Graustufen", "Grayscale"),
            ("rgb24", "Farbe (RGB)", "Color (RGB)"),
        ] {
            let tmp = tempfile::TempDir::new().unwrap();
            let job = Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: serde_json::json!({
                    "name": "TestJob", "job_id": 0, "color": "#4D4D4D"
                }),
                scan_settings: serde_json::json!({
                    "parameters": { "task": { "actions": { "streams": { "sources": {
                        "pixelFormats": { "pixelFormat": format }
                    } } } } }
                }),
            };
            let batch =
                crate::batch::Batch::create(&job, crate::batch::ScannerInfo::default()).unwrap();
            let detail_url = format!("/scans/{}", batch.id);
            let server = TestApp::new(AppState::new(Config {
                jobs: vec![job],
                ..Default::default()
            }));

            for (lang, label) in [("de", german), ("en", english)] {
                for path in ["/", "/jobs", detail_url.as_str()] {
                    let html = server
                        .get(path)
                        .clear_headers()
                        .add_header("host", "localhost")
                        .add_header("cookie", server.cookie(&format!("ui_lang={lang}")))
                        .await
                        .text();
                    assert!(html.contains(label), "missing {label} on {path}");
                    assert!(
                        !html.contains(format!(">{format}<").as_str()),
                        "raw {format} on {path}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn scan_rows_open_their_detail_page() {
        let (server, tmp) = test_server();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: serde_json::json!({"name":"TestJob", "job_id":0}),
            scan_settings: serde_json::json!({}),
        };
        let batch =
            crate::batch::Batch::create(&job, crate::batch::ScannerInfo::default()).unwrap();
        let destination = format!("/scans/{}", batch.id);
        for page in ["/", "/scans"] {
            let html = server.get(page).await.text();
            assert!(html.contains(&format!("data-scan-href=\"{destination}\"")));
            assert!(html.contains(&format!("href=\"{destination}\"")));
        }
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

    fn server_with_source(source: &str) -> (TestApp, tempfile::TempDir) {
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
        (TestApp::new(AppState::new(config)), tmp)
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

#[cfg(test)]
mod recovery_inventory_tests {
    use super::*;
    use crate::{
        batch::{Batch, ScannerInfo},
        config::{Config, Job},
    };
    use serde_json::json;
    #[test]
    fn stale_open_and_corrupt_batches_require_attention() {
        let root = tempfile::tempdir().unwrap();
        let job = Job {
            output_path: root.path().to_owned(),
            consume_path: None,
            job_info: json!({"name":"test","job_id":0}),
            scan_settings: json!({}),
        };
        let batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        let path = root.path().join(&batch.id).join("metadata.json");
        let mut meta: BatchMetadata =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        meta.created_at = "2026-01-01T00:00:00Z".into();
        std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
        assert!(paperless_needs_attention(&meta));
        std::fs::remove_file(&path).unwrap();
        let scans = list_scans(&[job], 100, crate::translations::for_lang("de"));
        assert_eq!(scans.len(), 1);
        assert!(scans[0].paperless_attention);
        drop(Config::default());
    }
}
