use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};

use crate::batch::Batch;
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
    last_ping: Option<DateTime<Local>>,
    name: Option<String>,
    model: Option<String>,
    serial: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub jobs: Arc<Mutex<Vec<Job>>>,
    pub batches: Arc<Mutex<HashMap<String, Batch>>>,
    scanner: Arc<Mutex<ConnectedScanner>>,
    pub retention: Arc<Mutex<RetentionConfig>>,
    pub config_path: Option<PathBuf>,
    pub lang: String,
    pub translations: &'static Translations,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            translations: crate::translations::for_lang(&config.lang),
            lang: config.lang.clone(),
            jobs: Arc::new(Mutex::new(config.jobs)),
            batches: Arc::new(Mutex::new(HashMap::new())),
            scanner: Arc::new(Mutex::new(ConnectedScanner::default())),
            retention: Arc::new(Mutex::new(config.retention)),
            config_path: None,
        }
    }

    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    pub fn scanner_is_online(&self) -> bool {
        lock!(self.scanner)
            .last_ping
            .map(|t| (Local::now() - t).num_seconds() < SCANNER_ONLINE_THRESHOLD_SECS)
            .unwrap_or(false)
    }

    pub fn scanner_display_name(&self) -> String {
        lock!(self.scanner)
            .name
            .clone()
            .unwrap_or_else(|| "—".to_string())
    }

    pub fn scanner_display_model(&self) -> Option<String> {
        lock!(self.scanner).model.clone()
    }

    pub fn scanner_display_serial(&self) -> Option<String> {
        lock!(self.scanner).serial.clone()
    }

    pub fn record_ping(&self) {
        lock!(self.scanner).last_ping = Some(Local::now());
    }

    pub fn set_scanner_info(&self, name: String, model: String, serial: String) {
        let mut s = lock!(self.scanner);
        s.last_ping = Some(Local::now());
        s.name = Some(name);
        s.model = Some(model);
        s.serial = Some(serial);
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
    use crate::config::Config;

    fn make_state() -> AppState {
        AppState::new(Config::default())
    }

    #[test]
    fn test_scanner_offline_initially() {
        assert!(!make_state().scanner_is_online());
    }

    #[test]
    fn test_record_ping_makes_scanner_online() {
        let s = make_state();
        s.record_ping();
        assert!(s.scanner_is_online());
    }

    #[test]
    fn test_set_scanner_info_all_fields_visible() {
        // All three fields must be readable after a single set_scanner_info call.
        // The refactored single-lock implementation guarantees this atomically;
        // the old three-lock version had a window where fields could be inconsistent.
        let s = make_state();
        s.set_scanner_info("fi-8170".into(), "fi-8170".into(), "SN001".into());
        assert_eq!(s.scanner_display_name(), "fi-8170");
        assert_eq!(s.scanner_display_model(), Some("fi-8170".into()));
        assert_eq!(s.scanner_display_serial(), Some("SN001".into()));
        assert!(
            s.scanner_is_online(),
            "set_scanner_info must also record a ping"
        );
    }

    #[test]
    fn test_display_name_fallback_before_device_call() {
        assert_eq!(make_state().scanner_display_name(), "—");
    }
}
