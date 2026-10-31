use anyhow::{Context, Result};
use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

use chrono::{DateTime, Utc};

use crate::batch::Batch;
use crate::build_info::{BUILD, BuildInfo};
use crate::config::{Config, Job, PaperlessConfig, RetentionConfig, ScannerAuthConfig};
use crate::event_log::EventLog;
use crate::paperless::PaperlessClient;
use crate::translations::Translations;

/// Scanner is considered offline after this many seconds without a ping.
const SCANNER_ONLINE_THRESHOLD_SECS: i64 = 60;

/// Recover a poisoned Mutex by taking the inner value anyway.
/// A panic in one handler should not permanently disable all others.
#[macro_export]
macro_rules! lock {
    ($m:expr) => {
        $m.lock().unwrap_or_else(|e| e.into_inner())
    };
}

/// All mutable scanner state in one struct so it can be updated atomically under one lock.
#[derive(Default, Clone)]
struct ConnectedScanner {
    last_ping: Option<DateTime<Utc>>,
    name: Option<String>,
    model: Option<String>,
    serial: Option<String>,
}

/// Public scanner state — all fields behind one lock for atomic reads and updates.
#[derive(Clone, Default)]
pub struct ScannerState(Arc<Mutex<ConnectedScanner>>);

impl ScannerState {
    pub fn is_online(&self) -> bool {
        lock!(self.0)
            .last_ping
            .map(|t| (Utc::now() - t).num_seconds() < SCANNER_ONLINE_THRESHOLD_SECS)
            .unwrap_or(false)
    }

    pub fn display_name(&self) -> String {
        lock!(self.0)
            .name
            .clone()
            .unwrap_or_else(|| "—".to_string())
    }

    pub fn display_model(&self) -> Option<String> {
        lock!(self.0).model.clone()
    }

    pub fn display_serial(&self) -> Option<String> {
        lock!(self.0).serial.clone()
    }

    pub fn record_ping(&self) {
        lock!(self.0).last_ping = Some(Utc::now());
    }

    pub fn set_info(&self, name: String, model: String, serial: String) {
        let mut s = lock!(self.0);
        s.last_ping = Some(Utc::now());
        s.name = Some(name);
        s.model = Some(model);
        s.serial = Some(serial);
    }
}

/// Newtype for the job list. Derefs to the inner `Arc<Mutex<...>>` so
/// `lock!(state.jobs)` works at all call sites without changes.
#[derive(Clone, Default)]
pub struct JobStore(Arc<Mutex<Vec<Job>>>);

impl JobStore {
    pub fn new(jobs: Vec<Job>) -> Self {
        Self(Arc::new(Mutex::new(jobs)))
    }
}

impl Deref for JobStore {
    type Target = Arc<Mutex<Vec<Job>>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Newtype for the in-flight batch map. Same Deref trick as `JobStore`.
#[derive(Clone, Default)]
pub struct BatchStore(Arc<Mutex<HashMap<String, Arc<Mutex<Batch>>>>>);

impl Deref for BatchStore {
    type Target = Arc<Mutex<HashMap<String, Arc<Mutex<Batch>>>>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone)]
pub struct AppState {
    pub scanner: ScannerState,
    pub admin_auth: Option<crate::admin_auth::AdminAuth>,
    pub scanner_auth_mode: crate::scanner_auth::ScannerAuthStore,
    pub scanner_auth_experiment: Option<&'static str>,
    pub scanner_auth_config: Arc<Mutex<Option<ScannerAuthConfig>>>,
    pub jobs: JobStore,
    pub batches: BatchStore,
    pub retention: Arc<Mutex<RetentionConfig>>,
    pub config_change_lock: Arc<Mutex<()>>,
    pub next_job_id: Arc<Mutex<usize>>,
    pub paperless_config: Arc<Mutex<Option<PaperlessConfig>>>,
    pub paperless_client: Arc<Mutex<Option<PaperlessClient>>>,
    pub upload_slots: Arc<Semaphore>,
    pub pdf_slots: Arc<Semaphore>,
    pub auth_slots: Arc<Semaphore>,
    pub config_path: Option<PathBuf>,
    pub default_output_path: PathBuf,
    pub lang: Arc<Mutex<String>>,
    pub translations: &'static Translations,
    pub build_info: &'static BuildInfo,
    pub event_log: EventLog,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let default_output_path = config
            .jobs
            .first()
            .map(|job| job.output_path.clone())
            .unwrap_or_else(|| PathBuf::from("./scans"));
        let next_job_id = config
            .jobs
            .iter()
            .filter_map(|j| j.job_info["job_id"].as_u64())
            .max()
            .and_then(|id| usize::try_from(id).ok())
            .and_then(|id| id.checked_add(1))
            .unwrap_or(0)
            .max(config.next_job_id);
        let mut recovered = HashMap::new();
        for job in &config.jobs {
            let Ok(entries) = std::fs::read_dir(&job.output_path) else {
                continue;
            };
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                match Batch::resume(job, &id) {
                    Ok(Some(batch)) => {
                        recovered.insert(id, Arc::new(Mutex::new(batch)));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(event = "batch.recovery_failed", batch_id = %crate::logging::safe_batch_id(&id), error_class = crate::logging::error_class(&e), "cannot recover batch")
                    }
                }
            }
        }
        Self {
            translations: crate::translations::for_lang(&config.lang),
            build_info: &BUILD,
            lang: Arc::new(Mutex::new(config.lang.clone())),
            scanner: ScannerState::default(),
            admin_auth: None,
            scanner_auth_mode: crate::scanner_auth::ScannerAuthStore::default(),
            scanner_auth_experiment: None,
            scanner_auth_config: Arc::new(Mutex::new(config.scanner_auth)),
            jobs: JobStore::new(config.jobs),
            batches: BatchStore(Arc::new(Mutex::new(recovered))),
            retention: Arc::new(Mutex::new(config.retention)),
            config_change_lock: Arc::new(Mutex::new(())),
            next_job_id: Arc::new(Mutex::new(next_job_id)),
            paperless_config: Arc::new(Mutex::new(config.paperless)),
            paperless_client: Arc::new(Mutex::new(None)),
            upload_slots: Arc::new(Semaphore::new(2)),
            pdf_slots: Arc::new(Semaphore::new(1)),
            auth_slots: Arc::new(Semaphore::new(2)),
            config_path: None,
            default_output_path,
            event_log: EventLog::default(),
        }
    }

    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    pub fn with_scanner_auth_mode(mut self, mode: crate::scanner_auth::ScannerAuthMode) -> Self {
        self.scanner_auth_mode = crate::scanner_auth::ScannerAuthStore::new(mode);
        self
    }

    pub fn with_scanner_auth_experiment(mut self, wire_type: &'static str) -> Self {
        self.scanner_auth_experiment = Some(wire_type);
        self
    }

    pub fn with_admin_auth(mut self, admin_auth: crate::admin_auth::AdminAuth) -> Self {
        self.admin_auth = Some(admin_auth);
        self
    }

    pub fn with_default_output_path(mut self, path: PathBuf) -> Self {
        self.default_output_path = path;
        self
    }

    pub fn with_paperless_client(mut self, client: PaperlessClient) -> Self {
        self.paperless_client = Arc::new(Mutex::new(Some(client)));
        self
    }

    pub fn with_event_log(mut self, event_log: EventLog) -> Self {
        self.event_log = event_log;
        self
    }

    pub fn persist_config(
        &self,
        jobs: &[Job],
        retention: &RetentionConfig,
        next_job_id: usize,
    ) -> Result<()> {
        if let Some(ref path) = self.config_path {
            Config::save_with_next(
                jobs,
                retention,
                &lock!(self.lang),
                next_job_id,
                lock!(self.paperless_config).as_ref(),
                lock!(self.scanner_auth_config).as_ref(),
                path,
            )?;
        }
        Ok(())
    }

    pub fn update_scanner_auth(&self, config: ScannerAuthConfig, pepper: &[u8]) -> Result<()> {
        let mode = crate::scanner_auth::ScannerAuthMode::from_hashed_config(&config, pepper)?;
        let _change = lock!(self.config_change_lock);
        if let Some(ref path) = self.config_path {
            let jobs = lock!(self.jobs).clone();
            let retention = lock!(self.retention).clone();
            Config::save_with_next(
                &jobs,
                &retention,
                &lock!(self.lang),
                *lock!(self.next_job_id),
                lock!(self.paperless_config).as_ref(),
                Some(&config),
                path,
            )?;
        }
        *lock!(self.scanner_auth_config) = Some(config);
        self.scanner_auth_mode.replace(mode);
        Ok(())
    }

    pub fn update_language(&self, lang: &str) -> Result<()> {
        anyhow::ensure!(matches!(lang, "de" | "en"), "unsupported language");
        let _change = lock!(self.config_change_lock);
        if let Some(ref path) = self.config_path {
            Config::save_with_next(
                &lock!(self.jobs),
                &lock!(self.retention),
                lang,
                *lock!(self.next_job_id),
                lock!(self.paperless_config).as_ref(),
                lock!(self.scanner_auth_config).as_ref(),
                path,
            )?;
        }
        *lock!(self.lang) = lang.to_owned();
        Ok(())
    }

    pub fn update_paperless(
        &self,
        mut config: Option<PaperlessConfig>,
        new_token: Option<&str>,
    ) -> Result<()> {
        let _change = lock!(self.config_change_lock);
        let previous = lock!(self.paperless_config).clone();
        let target_changed = previous.as_ref().map(|cfg| cfg.url.as_str())
            != config.as_ref().map(|cfg| cfg.url.as_str());
        if target_changed {
            let has_live_batches = lock!(self.batches).values().any(|batch| {
                lock!(batch)
                    .metadata()
                    .paperless
                    .as_ref()
                    .is_some_and(|receipt| receipt.document_id.is_none())
            });
            anyhow::ensure!(!has_live_batches, "scanner batch still in progress");
            anyhow::ensure!(
                !crate::paperless::pending_delivery_exists(&lock!(self.jobs))?,
                "Paperless delivery still pending"
            );
        }
        let mut created_secret = None;
        let next_client = if let Some(ref mut cfg) = config {
            anyhow::ensure!(
                cfg.tag_ids.iter().all(|id| *id > 0),
                "Paperless tag IDs must be positive"
            );
            anyhow::ensure!(
                cfg.batch_id_field_id.is_none_or(|id| id > 0),
                "Paperless field ID must be positive"
            );
            if let Some(secret) = new_token {
                anyhow::ensure!(
                    !secret.trim().is_empty()
                        && secret.len() <= 4096
                        && !secret.contains(['\r', '\n']),
                    "invalid Paperless API token"
                );
                let path = self
                    .config_path
                    .as_ref()
                    .and_then(|path| path.parent())
                    .context("Paperless token storage unavailable")?
                    .join(format!("paperless-token-{}", uuid::Uuid::new_v4().simple()));
                let client = PaperlessClient::new(cfg.clone(), secret.to_owned())?;
                crate::atomic_file::write(&path, secret.as_bytes())?;
                cfg.token_file = Some(path.clone());
                created_secret = Some(path);
                Some(client)
            } else {
                cfg.token_file = previous.as_ref().and_then(|old| old.token_file.clone());
                Some(PaperlessClient::from_env(cfg.clone())?)
            }
        } else {
            anyhow::ensure!(
                new_token.is_none(),
                "token requires enabled Paperless integration"
            );
            None
        };
        let save = if let Some(ref path) = self.config_path {
            Config::save_with_next(
                &lock!(self.jobs),
                &lock!(self.retention),
                &lock!(self.lang),
                *lock!(self.next_job_id),
                config.as_ref(),
                lock!(self.scanner_auth_config).as_ref(),
                path,
            )
        } else {
            Ok(())
        };
        if let Err(error) = save {
            if let Some(path) = created_secret {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
        *lock!(self.paperless_config) = config;
        *lock!(self.paperless_client) = next_client;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{Batch, ScannerInfo};
    use serde_json::json;

    #[test]
    fn scanner_config_change_is_durable_and_invalidates_old_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        let pepper = [7u8; 32];
        let first = ScannerAuthConfig {
            mode: "userpass".into(),
            username: Some("first".into()),
            password_hash: Some(
                crate::scanner_auth::hash_password("first-password", &pepper).unwrap(),
            ),
        };
        let state = AppState::new(Config::default()).with_config_path(path.clone());
        state.update_scanner_auth(first, &pepper).unwrap();
        let body = br#"{"auth_info_user":"first","auth_info_pass":"first-password"}"#;
        let session = state.scanner_auth_mode.login(body).unwrap();
        let next = ScannerAuthConfig {
            mode: "none".into(),
            username: None,
            password_hash: None,
        };
        state.update_scanner_auth(next, &pepper).unwrap();
        assert!(!state.scanner_auth_mode.session_valid(&session));
        assert_eq!(
            Config::load(&path).unwrap().scanner_auth.unwrap().mode,
            "none"
        );
    }

    #[test]
    fn scanner_config_write_failure_keeps_active_authentication() {
        let tmp = tempfile::tempdir().unwrap();
        let pepper = [7u8; 32];
        let state = AppState::new(Config::default()).with_config_path(tmp.path().to_path_buf());
        let next = ScannerAuthConfig {
            mode: "none".into(),
            username: None,
            password_hash: None,
        };
        assert!(state.update_scanner_auth(next, &pepper).is_err());
        assert!(lock!(state.scanner_auth_config).is_none());
    }

    #[test]
    fn paperless_token_rotation_stores_only_private_secret_and_updates_client() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        let state = AppState::new(Config::default()).with_config_path(path.clone());
        let config = PaperlessConfig {
            url: "http://paperless:8000".into(),
            token_env: "PAPERLESS_API_TOKEN".into(),
            token_file: None,
            tag_ids: vec![7],
            batch_id_field_id: Some(9),
        };
        state
            .update_paperless(Some(config.clone()), Some("first-secret"))
            .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("first-secret"));
        assert!(lock!(state.paperless_client).is_some());
        let first_path = Config::load(&path)
            .unwrap()
            .paperless
            .unwrap()
            .token_file
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&first_path).unwrap(),
            "first-secret"
        );
        state
            .update_paperless(Some(config), Some("second-secret"))
            .unwrap();
        let next_path = Config::load(&path)
            .unwrap()
            .paperless
            .unwrap()
            .token_file
            .unwrap();
        assert_ne!(first_path, next_path);
        assert_eq!(std::fs::read_to_string(next_path).unwrap(), "second-secret");
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("second-secret")
        );
    }

    #[test]
    fn paperless_target_change_rejects_open_scanner_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let scans = tmp.path().join("scans");
        std::fs::create_dir(&scans).unwrap();
        let job = Job {
            output_path: scans,
            consume_path: None,
            job_info: serde_json::json!({"name":"Test", "job_id":0,"color":"#4D4D4D"}),
            scan_settings: serde_json::json!({}),
        };
        let current = PaperlessConfig {
            url: "http://old:8000".into(),
            token_env: "X".into(),
            token_file: None,
            tag_ids: vec![],
            batch_id_field_id: None,
        };
        let state = AppState::new(Config {
            jobs: vec![job.clone()],
            paperless: Some(current.clone()),
            ..Default::default()
        })
        .with_config_path(tmp.path().join("config.yaml"));
        let mut batch = Batch::create(&job, crate::batch::ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        lock!(state.batches).insert(batch.id.clone(), Arc::new(Mutex::new(batch)));
        let next = PaperlessConfig {
            url: "http://new:8000".into(),
            ..current
        };
        assert!(state.update_paperless(Some(next), Some("secret")).is_err());
        assert_eq!(
            lock!(state.paperless_config).as_ref().unwrap().url,
            "http://old:8000"
        );
    }

    fn make_scanner() -> ScannerState {
        ScannerState::default()
    }

    #[test]
    fn test_scanner_offline_initially() {
        assert!(!make_scanner().is_online());
    }

    #[test]
    fn test_record_ping_makes_scanner_online() {
        let s = make_scanner();
        s.record_ping();
        assert!(s.is_online());
    }

    #[test]
    fn test_set_scanner_info_all_fields_visible() {
        // All three fields must be readable after a single set_info call.
        // The single-lock implementation guarantees this atomically.
        let s = make_scanner();
        s.set_info("fi-8170".into(), "fi-8170".into(), "SN001".into());
        assert_eq!(s.display_name(), "fi-8170");
        assert_eq!(s.display_model(), Some("fi-8170".into()));
        assert_eq!(s.display_serial(), Some("SN001".into()));
        assert!(s.is_online(), "set_info must also record a ping");
    }

    #[test]
    fn test_display_name_fallback_before_device_call() {
        assert_eq!(make_scanner().display_name(), "—");
    }

    #[test]
    fn test_incomplete_batch_survives_state_restart() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name":"Test", "job_id":0}),
            scan_settings: json!({}),
        };
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.add_file("page.jpg", b"page", json!({})).unwrap();
        let id = batch.id.clone();
        drop(batch);

        let state = AppState::new(Config {
            jobs: vec![job],
            ..Default::default()
        });
        assert!(lock!(state.batches).contains_key(&id));
    }

    #[test]
    fn test_recovered_batch_uses_its_own_job_delivery_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let consume = tmp.path().join("consume-b");
        std::fs::create_dir(&consume).unwrap();
        let make_job = |name: &str, id: usize, consume_path| Job {
            output_path: tmp.path().to_path_buf(),
            consume_path,
            job_info: json!({"name":name, "job_id":id}),
            scan_settings: json!({}),
        };
        let a = make_job("A", 0, None);
        let b = make_job("B", 1, Some(consume.clone()));
        let mut batch = Batch::create(&b, ScannerInfo::default()).unwrap();
        batch
            .add_file(
                "page.jpg",
                include_bytes!("../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        let id = batch.id.clone();
        drop(batch);
        let state = AppState::new(Config {
            jobs: vec![a, b],
            ..Default::default()
        });
        lock!(lock!(state.batches).get(&id).unwrap())
            .complete()
            .unwrap();
        assert!(
            std::fs::read_dir(consume).unwrap().any(|e| e
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "pdf"))
        );
    }
}
