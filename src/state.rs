use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::batch::Batch;
use crate::build_info::{BUILD, BuildInfo};
use crate::config::{Config, Job, RetentionConfig};
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
pub struct BatchStore(Arc<Mutex<HashMap<String, Batch>>>);

impl Deref for BatchStore {
    type Target = Arc<Mutex<HashMap<String, Batch>>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone)]
pub struct AppState {
    pub scanner: ScannerState,
    pub jobs: JobStore,
    pub batches: BatchStore,
    pub retention: Arc<Mutex<RetentionConfig>>,
    pub config_path: Option<PathBuf>,
    pub lang: String,
    pub translations: &'static Translations,
    pub build_info: &'static BuildInfo,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            translations: crate::translations::for_lang(&config.lang),
            build_info: &BUILD,
            lang: config.lang.clone(),
            scanner: ScannerState::default(),
            jobs: JobStore::new(config.jobs),
            batches: BatchStore::default(),
            retention: Arc::new(Mutex::new(config.retention)),
            config_path: None,
        }
    }

    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    pub fn persist_config(&self, jobs: &[Job]) {
        let retention = lock!(self.retention).clone();
        if let Some(ref path) = self.config_path
            && let Err(e) = Config::save(jobs, &retention, &self.lang, path)
        {
            tracing::warn!(error = %e, "failed to save config");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
