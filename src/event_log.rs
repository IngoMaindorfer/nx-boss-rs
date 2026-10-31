use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{Layer, layer::Context as LayerContext};

const MAX_EVENTS: usize = 200;
const MAX_FILE_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EventRecord {
    #[serde(default = "one")]
    pub count: u64,
    #[serde(default)]
    pub last_seen: String,
    pub timestamp: String,
    pub level: String,
    pub event: String,
    pub batch_id: Option<String>,
    pub error_class: Option<String>,
    pub http_status: Option<u16>,
}

fn one() -> u64 {
    1
}

#[derive(Default)]
struct Inner {
    path: Option<PathBuf>,
    records: VecDeque<EventRecord>,
    persistence_ok: bool,
}

#[derive(Clone, Default)]
pub struct EventLog {
    inner: Arc<Mutex<Inner>>,
    writer: Arc<Mutex<Option<JournalWriter>>>,
    persistence_lock: Arc<Mutex<()>>,
}

struct JournalWriter {
    signal: Option<std::sync::mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for JournalWriter {
    fn drop(&mut self) {
        self.signal.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn persist(inner: &Mutex<Inner>, io: &Mutex<()>) -> Result<()> {
    let _io = crate::lock!(io);
    let (path, records) = {
        let state = crate::lock!(inner);
        (state.path.clone(), state.records.clone())
    };
    let Some(path) = path else {
        return Ok(());
    };
    let result = serde_json::to_vec(&records)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| crate::atomic_file::write(&path, &bytes));
    crate::lock!(inner).persistence_ok = result.is_ok();
    result
}

impl EventLog {
    pub fn attach(&self, path: PathBuf) -> Result<()> {
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let records = if let Some(metadata) = metadata {
            ensure!(
                metadata.file_type().is_file(),
                "event journal is not a regular file"
            );
            ensure!(
                metadata.len() <= MAX_FILE_BYTES,
                "event journal exceeds size limit"
            );
            serde_json::from_slice::<VecDeque<EventRecord>>(&std::fs::read(&path)?)
                .context("reading event journal")?
        } else {
            VecDeque::new()
        };
        let mut inner = crate::lock!(self.inner);
        let startup = std::mem::take(&mut inner.records);
        inner.records = records.into_iter().rev().take(MAX_EVENTS).collect();
        inner.records.make_contiguous().reverse();
        inner.records.extend(startup);
        while inner.records.len() > MAX_EVENTS {
            inner.records.pop_front();
        }
        inner.path = Some(path);
        drop(inner);
        self.flush()?;
        let mut writer = crate::lock!(self.writer);
        if writer.is_none() {
            let (signal, messages) = std::sync::mpsc::sync_channel(1);
            let inner = self.inner.clone();
            let io = self.persistence_lock.clone();
            let thread = std::thread::spawn(move || {
                while messages.recv().is_ok() {
                    // Bounded write cadence, with no filesystem I/O on request threads.
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    while messages.try_recv().is_ok() {}
                    let _ = persist(&inner, &io);
                }
                let _ = persist(&inner, &io);
            });
            *writer = Some(JournalWriter {
                signal: Some(signal),
                thread: Some(thread),
            });
        }
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        persist(&self.inner, &self.persistence_lock)
    }

    pub fn record(
        &self,
        level: &str,
        event: &str,
        batch_id: Option<&str>,
        error_class: Option<&str>,
        http_status: Option<u16>,
    ) {
        if !matches!(level, "WARN" | "ERROR") {
            return;
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut record = EventRecord {
            count: 1,
            last_seen: now.clone(),
            timestamp: now,
            level: level.to_string(),
            event: safe_code(event, 80).unwrap_or_else(|| "unknown".to_string()),
            batch_id: batch_id
                .filter(|id| crate::batch::valid_batch_id(id))
                .map(str::to_string),
            error_class: error_class.and_then(|value| safe_code(value, 40)),
            http_status: http_status.filter(|status| *status == 0 || (100..=599).contains(status)),
        };
        let mut inner = crate::lock!(self.inner);
        if let Some(index) = inner.records.iter().position(|old| {
            old.level == record.level
                && old.event == record.event
                && old.batch_id == record.batch_id
                && old.error_class == record.error_class
                && old.http_status == record.http_status
        }) {
            let old = inner.records.remove(index).expect("existing event");
            record.timestamp = old.timestamp;
            record.count = old.count.saturating_add(1);
        }
        inner.records.push_back(record);
        while inner.records.len() > MAX_EVENTS {
            inner.records.pop_front();
        }
        drop(inner);
        if let Some(writer) = crate::lock!(self.writer).as_ref()
            && let Some(signal) = &writer.signal
        {
            let _ = signal.try_send(());
        }
    }

    pub fn recent(&self) -> Vec<EventRecord> {
        crate::lock!(self.inner)
            .records
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    pub fn persistence_ok(&self) -> bool {
        crate::lock!(self.inner).persistence_ok
    }
}

fn safe_code(value: &str, max_len: usize) -> Option<String> {
    (!value.is_empty()
        && value.len() <= max_len
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        }))
    .then(|| value.to_string())
}

#[derive(Default)]
struct SafeFields {
    event: Option<String>,
    batch_id: Option<String>,
    error_class: Option<String>,
    http_status: Option<u16>,
}

impl Visit for SafeFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "event" => self.event = Some(value.to_string()),
            "batch_id" => self.batch_id = Some(value.to_string()),
            "error_class" => self.error_class = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "http_status" {
            self.http_status = u16::try_from(value).ok();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if matches!(field.name(), "event" | "batch_id" | "error_class") {
            let formatted = format!("{value:?}");
            self.record_str(field, formatted.trim_matches('"'));
        }
    }
}

pub struct EventLayer(pub EventLog);

impl<S: Subscriber> Layer<S> for EventLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        let level = event.metadata().level().as_str();
        if !matches!(level, "WARN" | "ERROR") {
            return;
        }
        let mut fields = SafeFields::default();
        event.record(&mut fields);
        if let Some(code) = fields.event.as_deref() {
            self.0.record(
                level,
                code,
                fields.batch_id.as_deref(),
                fields.error_class.as_deref(),
                fields.http_status,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_events_are_coalesced_instead_of_evicting_other_failures() {
        let log = EventLog::default();
        for _ in 0..210 {
            log.record(
                "WARN",
                "paperless.delivery_failed",
                None,
                Some("http_status"),
                Some(503),
            );
        }
        assert_eq!(log.recent().len(), 1);
        assert_eq!(log.recent()[0].count, 210);
    }

    #[test]
    fn recording_does_not_wait_for_filesystem_io() {
        let root = tempfile::tempdir().unwrap();
        let log = EventLog::default();
        log.attach(root.path().join("events.json")).unwrap();
        let io = crate::lock!(log.persistence_lock);
        let (tx, rx) = std::sync::mpsc::channel();
        let other = log.clone();
        std::thread::spawn(move || {
            other.record("WARN", "test.warning", None, None, None);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        drop(io);
        log.flush().unwrap();
    }

    #[test]
    fn persists_bounded_safe_events_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.json");
        let log = EventLog::default();
        log.attach(path.clone()).unwrap();
        for index in 0..210 {
            log.record(
                "WARN",
                "paperless.delivery_failed",
                Some(&format!("{index:032x}")),
                Some("http_status"),
                Some(503),
            );
        }
        log.flush().unwrap();
        let reopened = EventLog::default();
        reopened.attach(path).unwrap();
        let records = reopened.recent();
        assert_eq!(records.len(), 200);
        assert_eq!(records[0].event, "paperless.delivery_failed");
        assert_eq!(records[0].http_status, Some(503));
        assert!(reopened.persistence_ok());
    }

    #[test]
    fn rejects_untrusted_fields_and_never_persists_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.json");
        let log = EventLog::default();
        log.attach(path.clone()).unwrap();
        log.record(
            "ERROR",
            "private Token SECRET",
            Some("../../secret"),
            Some("Token SECRET"),
            None,
        );
        let records = log.recent();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event, "unknown");
        assert_eq!(records[0].batch_id, None);
        assert_eq!(records[0].error_class, None);
        log.flush().unwrap();
        assert!(!std::fs::read_to_string(path).unwrap().contains("SECRET"));
    }

    #[test]
    fn tracing_layer_captures_only_whitelisted_fields() {
        use tracing_subscriber::prelude::*;

        let log = EventLog::default();
        let subscriber = tracing_subscriber::registry().with(EventLayer(log.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(event = "paperless.delivery_failed", batch_id = %"a".repeat(32), error_class = "http_status", http_status = 503_u16, "Token PRIVATE_API_TOKEN");
            tracing::info!(event = "service.started", "ordinary message");
        });
        let records = log.recent();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event, "paperless.delivery_failed");
        assert_eq!(
            records[0].batch_id.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(records[0].http_status, Some(503));
        assert!(!format!("{records:?}").contains("PRIVATE_API_TOKEN"));
    }

    #[test]
    fn startup_warnings_survive_attaching_persistent_storage() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::default();
        log.record("WARN", "logging.invalid_level", None, None, None);
        log.attach(dir.path().join("events.json")).unwrap();
        assert_eq!(log.recent()[0].event, "logging.invalid_level");
    }

    #[test]
    fn attaching_empty_journal_proves_it_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.json");
        let log = EventLog::default();
        log.attach(path.clone()).unwrap();
        assert!(path.is_file());
        assert!(log.persistence_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_event_journal() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"[]").unwrap();
        let path = dir.path().join("events.json");
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(EventLog::default().attach(path).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"[]");
    }
}
