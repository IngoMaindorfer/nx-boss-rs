use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};

use crate::batch::Batch;
use crate::config::{Config, Job};

#[derive(Clone)]
pub struct AppState {
    pub jobs: Arc<Mutex<Vec<Job>>>,
    pub batches: Arc<Mutex<HashMap<String, Batch>>>,
    pub last_scanner_ping: Arc<Mutex<Option<DateTime<Local>>>>,
    pub scanner_name: Arc<Mutex<Option<String>>>,
    pub config_path: Option<PathBuf>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(config.jobs)),
            batches: Arc::new(Mutex::new(HashMap::new())),
            last_scanner_ping: Arc::new(Mutex::new(None)),
            scanner_name: Arc::new(Mutex::new(None)),
            config_path: None,
        }
    }

    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    pub fn scanner_is_online(&self) -> bool {
        self.last_scanner_ping
            .lock()
            .unwrap()
            .map(|t| (Local::now() - t).num_seconds() < 60)
            .unwrap_or(false)
    }

    pub fn scanner_display_name(&self) -> String {
        self.scanner_name
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "—".to_string())
    }
}
