use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::batch::Batch;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub batches: Arc<Mutex<HashMap<String, Batch>>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            batches: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
