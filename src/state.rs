use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};

use crate::batch::Batch;
use crate::config::{Config, Job, RetentionConfig};

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

#[derive(Clone)]
pub struct AppState {
    pub jobs: Arc<Mutex<Vec<Job>>>,
    pub batches: Arc<Mutex<HashMap<String, Batch>>>,
    pub last_scanner_ping: Arc<Mutex<Option<DateTime<Local>>>>,
    pub scanner_name: Arc<Mutex<Option<String>>>,
    pub scanner_model: Arc<Mutex<Option<String>>>,
    pub scanner_serial: Arc<Mutex<Option<String>>>,
    pub retention: Arc<Mutex<RetentionConfig>>,
    pub config_path: Option<PathBuf>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(config.jobs)),
            batches: Arc::new(Mutex::new(HashMap::new())),
            last_scanner_ping: Arc::new(Mutex::new(None)),
            scanner_name: Arc::new(Mutex::new(None)),
            scanner_model: Arc::new(Mutex::new(None)),
            scanner_serial: Arc::new(Mutex::new(None)),
            retention: Arc::new(Mutex::new(config.retention)),
            config_path: None,
        }
    }

    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    pub fn scanner_is_online(&self) -> bool {
        lock!(self.last_scanner_ping)
            .map(|t| (Local::now() - t).num_seconds() < SCANNER_ONLINE_THRESHOLD_SECS)
            .unwrap_or(false)
    }

    pub fn scanner_display_name(&self) -> String {
        lock!(self.scanner_name)
            .clone()
            .unwrap_or_else(|| "—".to_string())
    }

    pub fn scanner_display_model(&self) -> Option<String> {
        lock!(self.scanner_model).clone()
    }

    pub fn scanner_display_serial(&self) -> Option<String> {
        lock!(self.scanner_serial).clone()
    }

    pub fn record_ping(&self) {
        *lock!(self.last_scanner_ping) = Some(Local::now());
    }

    pub fn set_scanner_info(&self, name: String, model: String, serial: String) {
        self.record_ping();
        *lock!(self.scanner_name) = Some(name);
        *lock!(self.scanner_model) = Some(model);
        *lock!(self.scanner_serial) = Some(serial);
    }
}
