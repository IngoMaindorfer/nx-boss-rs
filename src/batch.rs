use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::config::Job;

pub const MAX_PAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_BATCH_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BATCH_PAGES: usize = 100;

pub fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// Returned by [`Batch::add_file`] when the filename would escape the batch directory.
/// Typed so callers can distinguish path-traversal rejections from I/O errors without
/// fragile string matching.
#[derive(Debug)]
pub struct PathTraversalError;

impl std::fmt::Display for PathTraversalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("filename escapes batch directory")
    }
}

impl std::error::Error for PathTraversalError {}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("batch is already complete")]
    Complete,
    #[error("conflicting upload for existing page")]
    Conflict,
    #[error("batch page or byte limit exceeded")]
    LimitExceeded,
}

#[derive(Debug, thiserror::Error)]
#[error("corrupt scan data; inspect or discard the damaged batch before changing configuration")]
pub struct CorruptBatchError;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ScannerInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct JobConfig {
    pub resolution: u32,
    pub pixel_format: String,
    pub jpeg_quality: u8,
    pub source: String,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            resolution: 300,
            pixel_format: "rgb24".to_string(),
            jpeg_quality: 80,
            source: "feeder".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BatchMetadata {
    pub job_name: String,
    #[serde(default)]
    pub job_id: Option<usize>,
    pub created_at: String,
    pub completed: bool,
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub scanner: ScannerInfo,
    #[serde(default)]
    pub job_config: JobConfig,
    #[serde(default)]
    pub paperless: Option<PaperlessReceipt>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PaperlessReceipt {
    pub task_id: Option<String>,
    pub document_id: Option<u64>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub upload_attempts: u32,
    #[serde(default)]
    pub last_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_options: Option<PaperlessUploadOptions>,
}

/// Multipart metadata fixed before a batch can be sent to Paperless.
/// Legacy receipts may lack this field; the worker pins their current settings
/// before its next network request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaperlessUploadOptions {
    pub tag_ids: Vec<u32>,
    pub batch_id_field_id: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub filename: String,
    pub received_at: String,
    pub parameters: Value,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug)]
pub struct Batch {
    pub id: String,
    dir: PathBuf,
    consume_path: Option<PathBuf>,
    metadata: BatchMetadata,
}

pub fn valid_batch_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Batch {
    pub fn has_pending_work(job: &Job, id: &str) -> Result<bool> {
        if !valid_batch_id(id) {
            return Ok(false);
        }
        let Some(batch) = Self::open(job, id)? else {
            return Ok(false);
        };
        Ok(!batch.metadata.completed
            || batch
                .metadata
                .paperless
                .as_ref()
                .is_some_and(|p| p.document_id.is_none()))
    }

    pub fn resume(job: &Job, id: &str) -> Result<Option<Self>> {
        let batch = Self::open(job, id)?;
        Ok(batch.filter(|b| !b.metadata.completed))
    }

    pub fn open_for_paperless(job: &Job, id: &str) -> Result<Option<Self>> {
        let batch = Self::open(job, id)?;
        Ok(batch.filter(|b| {
            b.metadata.completed
                && b.metadata
                    .paperless
                    .as_ref()
                    .is_some_and(|p| p.document_id.is_none())
        }))
    }

    pub(crate) fn open(job: &Job, id: &str) -> Result<Option<Self>> {
        if !valid_batch_id(id) {
            return Ok(None);
        }
        let dir = job.output_path.join(id);
        if !dir.symlink_metadata().is_ok_and(|m| m.file_type().is_dir()) {
            return Ok(None);
        }
        let metadata = Self::inspect_directory(&dir)?;
        let belongs_to_job = metadata
            .job_id
            .map(|id| job.job_info["job_id"].as_u64() == Some(id as u64))
            .unwrap_or_else(|| metadata.job_name == job.name());
        if !belongs_to_job {
            return Ok(None);
        }
        Ok(Some(Self {
            id: id.to_string(),
            dir,
            consume_path: job.consume_path.clone(),
            metadata,
        }))
    }

    pub(crate) fn read_metadata(dir: &Path) -> Result<BatchMetadata> {
        let meta_path = dir.join("metadata.json");
        if !meta_path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_file())
        {
            return Err(CorruptBatchError.into());
        }
        let metadata: BatchMetadata =
            serde_json::from_slice(&std::fs::read(&meta_path).map_err(|_| CorruptBatchError)?)
                .map_err(|_| CorruptBatchError)?;
        Ok(metadata)
    }

    pub(crate) fn inspect_directory(dir: &Path) -> Result<BatchMetadata> {
        let mut metadata = Self::read_metadata(dir)?;
        for page in &mut metadata.files {
            if page.filename.starts_with('.')
                || !page
                    .filename
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                || !dir
                    .join(&page.filename)
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_file())
            {
                return Err(CorruptBatchError.into());
            }
            page.size = std::fs::metadata(dir.join(&page.filename))?.len();
        }
        Ok(metadata)
    }

    pub fn create(job: &Job, scanner: ScannerInfo) -> Result<Self> {
        let id = Uuid::now_v6(b"nxboss").simple().to_string();
        let dir = job.output_path.join(&id);
        std::fs::create_dir_all(&dir)?;
        crate::atomic_file::sync_dir(&job.output_path)?;
        let metadata = BatchMetadata {
            job_name: job.job_info["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            job_id: job.job_info["job_id"]
                .as_u64()
                .and_then(|id| usize::try_from(id).ok()),
            created_at: now_iso(),
            completed: false,
            files: vec![],
            scanner,
            job_config: JobConfig {
                resolution: job.resolution(),
                pixel_format: job.pixel_format().to_string(),
                jpeg_quality: job.jpeg_quality(),
                source: job.source().to_string(),
            },
            paperless: None,
        };
        let consume_path = job.consume_path.clone();
        let batch = Self {
            id,
            dir,
            consume_path,
            metadata,
        };
        batch.flush_metadata()?;
        Ok(batch)
    }

    #[cfg(test)]
    pub fn enable_paperless(&mut self) -> Result<()> {
        self.enable_paperless_with_options(PaperlessUploadOptions::default())
    }

    pub fn enable_paperless_with_options(&mut self, options: PaperlessUploadOptions) -> Result<()> {
        self.update_metadata(|metadata| {
            metadata.paperless = Some(PaperlessReceipt {
                upload_options: Some(options),
                ..PaperlessReceipt::default()
            });
        })
    }

    pub fn ensure_paperless_options(&mut self, options: PaperlessUploadOptions) -> Result<()> {
        if self
            .metadata
            .paperless
            .as_ref()
            .is_some_and(|receipt| receipt.upload_options.is_none())
        {
            self.update_metadata(|metadata| {
                metadata
                    .paperless
                    .as_mut()
                    .expect("receipt exists")
                    .upload_options = Some(options);
            })?;
        }
        Ok(())
    }

    pub fn record_paperless_task(&mut self, task_id: String) -> Result<()> {
        if self.metadata.paperless.is_some() {
            self.update_metadata(|metadata| {
                let receipt = metadata.paperless.as_mut().expect("receipt exists");
                receipt.task_id = Some(task_id);
                receipt.last_error = None;
            })?;
        }
        Ok(())
    }

    pub fn record_paperless_attempt(&mut self) -> Result<()> {
        self.update_metadata(|metadata| {
            let receipt = metadata.paperless.as_mut().expect("receipt exists");
            receipt.upload_attempts = receipt.upload_attempts.saturating_add(1);
            receipt.last_attempt_at = Some(now_iso());
            receipt.task_id = None;
            receipt.last_error = None;
        })
    }

    pub fn record_paperless_document(&mut self, document_id: u64) -> Result<()> {
        if self.metadata.paperless.is_some() {
            self.update_metadata(|metadata| {
                let receipt = metadata.paperless.as_mut().expect("receipt exists");
                receipt.document_id = Some(document_id);
                receipt.last_error = None;
            })?;
        }
        Ok(())
    }

    pub fn record_paperless_error(&mut self, error: &str) -> Result<()> {
        let error: String = error.chars().take(500).collect();
        if self
            .metadata
            .paperless
            .as_ref()
            .is_some_and(|r| r.last_error.as_deref() == Some(error.as_str()))
        {
            return Ok(());
        }
        if self.metadata.paperless.is_some() {
            self.update_metadata(|metadata| {
                let receipt = metadata.paperless.as_mut().expect("receipt exists");
                receipt.last_error = Some(error);
            })?;
        }
        Ok(())
    }

    pub fn retry_paperless(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.metadata.completed
                && self
                    .metadata
                    .paperless
                    .as_ref()
                    .is_some_and(|r| r.document_id.is_none()),
            "no pending Paperless delivery"
        );
        let pdf = crate::pdf::assemble_pdf_with_dpi(
            &self.load_pages()?,
            self.metadata.job_config.resolution,
        )?;
        crate::atomic_file::write(&self.pdf_path(), &pdf)?;
        self.update_metadata(|m| {
            let receipt = m.paperless.as_mut().expect("checked receipt");
            receipt.task_id = None;
            receipt.document_id = None;
            receipt.last_error = None;
            receipt.upload_attempts = 0;
            receipt.last_attempt_at = None;
        })
    }

    pub fn pdf_path(&self) -> PathBuf {
        self.dir.join("document.pdf")
    }

    pub fn add_file(&mut self, filename: &str, content: &[u8], parameters: Value) -> Result<()> {
        if self.metadata.completed {
            return Err(UploadError::Complete.into());
        }
        // The scanner controls this name. Keep it a single JPEG basename and protect
        // our own metadata files, even when the output directory is relative.
        if filename.len() > 255
            || filename.starts_with('.')
            || !filename
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || !filename.to_ascii_lowercase().ends_with(".jpg")
                && !filename.to_ascii_lowercase().ends_with(".jpeg")
        {
            return Err(PathTraversalError.into());
        }
        let file_path = self.dir.join(filename);
        // is_safe_path normalises ".." components without requiring the file to exist,
        // which canonicalize() cannot do (it requires an existing path).
        if !is_safe_path(&self.dir, &file_path) {
            return Err(PathTraversalError.into());
        }
        if self.metadata.files.iter().any(|f| f.filename == filename) {
            if std::fs::read(&file_path)? == content {
                return Ok(()); // scanner retry after a lost response
            }
            return Err(UploadError::Conflict.into());
        }
        if content.len() > MAX_PAGE_BYTES
            || self.metadata.files.len() >= MAX_BATCH_PAGES
            || self.metadata.files.iter().map(|f| f.size).sum::<u64>() + content.len() as u64
                > MAX_BATCH_BYTES
        {
            return Err(UploadError::LimitExceeded.into());
        }
        crate::atomic_file::write(&file_path, content)?;
        self.update_metadata(|metadata| {
            metadata.files.push(FileEntry {
                filename: filename.to_string(),
                received_at: now_iso(),
                parameters,
                size: content.len() as u64,
            })
        })
    }

    pub fn complete(&mut self) -> Result<()> {
        if self.metadata.completed {
            return Ok(());
        }
        if self.metadata.files.is_empty() {
            return self.update_metadata(|metadata| {
                metadata.completed = true;
                metadata.paperless = None;
            });
        }
        let pdf = if self.consume_path.is_some() || self.metadata.paperless.is_some() {
            let pages = self.load_pages()?;
            Some(crate::pdf::assemble_pdf_with_dpi(
                &pages,
                self.metadata.job_config.resolution,
            )?)
        } else {
            None
        };
        if self.metadata.paperless.is_some() {
            let path = self.pdf_path();
            crate::atomic_file::write(&path, pdf.as_ref().expect("PDF was assembled"))?;
        }
        if let Some(ref consume_path) = self.consume_path.clone() {
            self.deliver_pdf(consume_path, pdf.as_ref().expect("PDF was assembled"))?;
        }
        self.update_metadata(|metadata| metadata.completed = true)?;
        Ok(())
    }

    fn deliver_pdf(&self, consume_path: &Path, pdf: &[u8]) -> Result<()> {
        // Blank-page filtering is intentionally left to Paperless-NGX (consume pipeline).
        // The fi-7300NX does not support blankPageSkip in hardware and we avoid
        // duplicating post-processing that the consumer already handles.
        let base = self.output_filename_base();
        let sidecar = self.build_sidecar(&base)?;

        // Write JSON sidecar first — present before PDF triggers Paperless consumption.
        // A pre-consume script can read {basename}.json to set title/tags via the API.
        let sidecar_path = consume_path.join(format!("{base}.json"));
        let sidecar_tmp = consume_path.join(format!(".{base}.json.tmp"));
        std::fs::write(&sidecar_tmp, serde_json::to_string_pretty(&sidecar)?)?;
        std::fs::rename(sidecar_tmp, sidecar_path)?;

        let pdf_path = consume_path.join(format!("{base}.pdf"));
        if !pdf_path.exists() {
            let pdf_tmp = consume_path.join(format!(".{base}.pdf.tmp"));
            std::fs::write(&pdf_tmp, pdf)?;
            std::fs::rename(pdf_tmp, pdf_path)?;
        }

        tracing::info!(
            event = "batch.consume_delivered",
            batch_id = %self.id,
            pages = self.metadata.files.len(),
            "PDF + JSON sidecar delivered to consume folder"
        );
        Ok(())
    }

    fn load_pages(&self) -> Result<Vec<Vec<u8>>> {
        self.metadata
            .files
            .iter()
            .map(|f| std::fs::read(self.dir.join(&f.filename)).map_err(anyhow::Error::from))
            .collect()
    }

    /// Returns a filesystem-safe `<jobname>_<timestamp>` base suitable for `.pdf` / `.json`.
    fn output_filename_base(&self) -> String {
        let safe_name: String = self
            .metadata
            .job_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // Fall back to batch ID if job name consists entirely of non-alphanumeric chars.
        let safe_name = if safe_name.is_empty() {
            self.id.clone()
        } else {
            safe_name
        };
        format!("{safe_name}_{}", self.id)
    }

    fn build_sidecar(&self, base: &str) -> Result<Value> {
        // Normalise job name to a slug for the Paperless tag (e.g. "My Job" → "my-job").
        let tag: String = self
            .metadata
            .job_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let created_date = crate::scan_date::local_scan_date(
            &self.metadata.created_at,
            crate::scan_date::configured_timezone()?,
        )?;
        Ok(serde_json::json!({
            "title": format!("{} {created_date}", self.metadata.job_name),
            "created": created_date,
            "tags": ["scan", tag],
            "correspondent": null,
            "custom_fields": {
                "batch_id": &self.id,
                "pages": self.metadata.files.len(),
                "resolution": self.metadata.job_config.resolution,
                "pixel_format": &self.metadata.job_config.pixel_format,
                "source": &self.metadata.job_config.source,
                "scanner_model": self.metadata.scanner.model.as_deref().unwrap_or(""),
                "filename_base": base,
            }
        }))
    }

    pub fn metadata(&self) -> &BatchMetadata {
        &self.metadata
    }

    pub fn created_at(&self) -> &str {
        &self.metadata.created_at
    }

    fn flush_metadata(&self) -> Result<()> {
        self.write_metadata(&self.metadata)
    }

    fn write_metadata(&self, metadata: &BatchMetadata) -> Result<()> {
        crate::atomic_file::write(
            &self.dir.join("metadata.json"),
            &serde_json::to_vec(metadata)?,
        )
    }

    fn update_metadata(&mut self, update: impl FnOnce(&mut BatchMetadata)) -> Result<()> {
        let mut next = self.metadata.clone();
        update(&mut next);
        if next == self.metadata {
            return Ok(());
        }
        self.write_metadata(&next)?;
        self.metadata = next;
        Ok(())
    }
}

/// Returns false if `path` escapes `base` via `..` components.
pub fn is_safe_path(base: &Path, path: &Path) -> bool {
    // Normalize without requiring the path to exist
    let mut normalized = PathBuf::new();
    for component in path.components() {
        use std::path::Component::*;
        match component {
            ParentDir => {
                if !normalized.pop() {
                    return false; // tried to escape root
                }
            }
            CurDir => {}
            c => normalized.push(c),
        }
    }
    normalized.starts_with(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn make_job(dir: &Path) -> Job {
        Job {
            output_path: dir.to_path_buf(),
            consume_path: None,
            job_info: json!({"name": "test", "job_id": 0, "color": "#4D4D4D", "type": 0, "job_setting": {}, "hierarchy_list": null}),
            scan_settings: json!({}),
        }
    }

    #[test]
    fn consume_sidecar_uses_berlin_scan_date_at_midnight() {
        let root = tempfile::tempdir().unwrap();
        let mut batch = Batch::create(&make_job(root.path()), ScannerInfo::default()).unwrap();
        for (utc, expected) in [
            ("2026-07-01T22:30:00Z", "2026-07-02"),
            ("2026-01-01T23:30:00Z", "2026-01-02"),
        ] {
            batch.metadata.created_at = utc.into();
            let sidecar = batch.build_sidecar("test").unwrap();
            assert_eq!(sidecar["created"], expected);
            assert_eq!(sidecar["title"], format!("test {expected}"));
        }
    }

    #[test]
    fn empty_paperless_batch_completes_without_document_or_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        batch.complete().unwrap();
        assert!(batch.metadata().completed);
        assert!(batch.metadata().paperless.is_none());
        assert!(!batch.pdf_path().exists());
    }
    #[test]
    fn unchanged_paperless_error_does_not_rewrite_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        batch.record_paperless_error("same failure").unwrap();
        let path = tmp.path().join(&batch.id).join("metadata.json");
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        batch.record_paperless_error("same failure").unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().modified().unwrap(),
            modified
        );
    }

    fn block_metadata_replace(batch: &Batch) {
        let metadata = batch.dir.join("metadata.json");
        fs::rename(&metadata, batch.dir.join("metadata.backup")).unwrap();
        fs::create_dir(&metadata).unwrap();
    }

    fn restore_metadata(batch: &Batch) {
        fs::remove_dir(batch.dir.join("metadata.json")).unwrap();
        fs::rename(
            batch.dir.join("metadata.backup"),
            batch.dir.join("metadata.json"),
        )
        .unwrap();
    }

    #[test]
    fn test_create_batch_makes_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        assert!(tmp.path().join(&batch.id).is_dir());
    }

    #[test]
    fn test_create_writes_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        let meta_path = tmp.path().join(&batch.id).join("metadata.json");
        assert!(meta_path.exists());
        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(meta_path).unwrap()).unwrap();
        assert_eq!(meta["job_name"], "test");
        assert_eq!(meta["completed"], false);
    }

    #[test]
    fn test_add_file_writes_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.add_file("scan.jpg", b"fakeimage", json!({})).unwrap();
        let file = tmp.path().join(&batch.id).join("scan.jpg");
        assert_eq!(fs::read(file).unwrap(), b"fakeimage");
    }

    #[test]
    fn test_add_file_records_in_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch
            .add_file("page1.jpg", b"data", json!({"key": "val"}))
            .unwrap();
        assert_eq!(batch.metadata().files.len(), 1);
        assert_eq!(batch.metadata().files[0].filename, "page1.jpg");
    }

    #[test]
    fn failed_page_metadata_save_can_be_retried() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        block_metadata_replace(&batch);

        assert!(batch.add_file("page.jpg", b"image", json!({})).is_err());
        assert!(batch.metadata().files.is_empty());
        assert!(
            fs::read_dir(&batch.dir).unwrap().all(|entry| {
                let name = entry.unwrap().file_name();
                !name.to_string_lossy().starts_with(".metadata.json.")
            }),
            "failed replacement must remove its temporary file"
        );
        restore_metadata(&batch);

        batch.add_file("page.jpg", b"image", json!({})).unwrap();
        let reopened = Batch::resume(&job, &batch.id).unwrap().unwrap();
        assert_eq!(reopened.metadata().files.len(), 1);
        assert_eq!(fs::read(batch.dir.join("page.jpg")).unwrap(), b"image");
    }

    #[test]
    fn failed_paperless_receipt_save_keeps_memory_and_disk_in_sync() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        block_metadata_replace(&batch);

        assert!(batch.record_paperless_task("task-1".to_string()).is_err());
        assert!(
            batch
                .metadata()
                .paperless
                .as_ref()
                .unwrap()
                .task_id
                .is_none()
        );
        restore_metadata(&batch);

        batch.record_paperless_task("task-1".to_string()).unwrap();
        let saved: BatchMetadata =
            serde_json::from_slice(&fs::read(batch.dir.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(saved.paperless.unwrap().task_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn paperless_upload_options_survive_reopen_before_first_attempt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        let options = PaperlessUploadOptions {
            tag_ids: vec![7, 9],
            batch_id_field_id: Some(11),
        };
        batch
            .enable_paperless_with_options(options.clone())
            .unwrap();

        let saved = Batch::resume(&job, &batch.id).unwrap().unwrap();
        assert_eq!(
            saved
                .metadata()
                .paperless
                .as_ref()
                .unwrap()
                .upload_options
                .as_ref(),
            Some(&options)
        );
    }

    #[test]
    fn legacy_paperless_options_are_pinned_once_before_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        let path = batch.dir.join("metadata.json");
        let mut old: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old["paperless"]
            .as_object_mut()
            .unwrap()
            .remove("upload_options");
        crate::atomic_file::write(&path, &serde_json::to_vec(&old).unwrap()).unwrap();
        let mut legacy = Batch::resume(&job, &batch.id).unwrap().unwrap();
        let first = PaperlessUploadOptions {
            tag_ids: vec![7],
            batch_id_field_id: Some(11),
        };
        legacy.ensure_paperless_options(first.clone()).unwrap();
        legacy
            .ensure_paperless_options(PaperlessUploadOptions {
                tag_ids: vec![99],
                batch_id_field_id: Some(88),
            })
            .unwrap();
        let saved = Batch::resume(&job, &batch.id).unwrap().unwrap();
        assert_eq!(
            saved
                .metadata()
                .paperless
                .as_ref()
                .unwrap()
                .upload_options
                .as_ref(),
            Some(&first)
        );
    }

    #[test]
    fn failed_completion_metadata_save_stays_retryable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        block_metadata_replace(&batch);

        assert!(batch.complete().is_err());
        assert!(!batch.metadata().completed);
        restore_metadata(&batch);

        batch.complete().unwrap();
        let saved: BatchMetadata =
            serde_json::from_slice(&fs::read(batch.dir.join("metadata.json")).unwrap()).unwrap();
        assert!(saved.completed);
    }

    #[test]
    fn retrying_completion_rebuilds_pdf_when_pages_changed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        let jpeg = include_bytes!("../tests/fixtures/scan_page.jpg");
        batch.add_file("first.jpg", jpeg, json!({})).unwrap();
        block_metadata_replace(&batch);
        assert!(batch.complete().is_err());
        restore_metadata(&batch);
        batch.add_file("second.jpg", jpeg, json!({})).unwrap();
        batch.complete().unwrap();
        let pdf = lopdf::Document::load_mem(&fs::read(batch.pdf_path()).unwrap()).unwrap();
        assert_eq!(pdf.get_pages().len(), 2);
    }

    #[test]
    fn test_add_file_rejects_path_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        let result = batch.add_file("../escape.txt", b"evil", json!({}));
        assert!(result.is_err());
        assert!(!tmp.path().join("escape.txt").exists());
    }

    #[test]
    fn test_add_file_rejects_internal_metadata_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut batch = Batch::create(&make_job(tmp.path()), ScannerInfo::default()).unwrap();
        for name in ["metadata.json", ".metadata.json"] {
            assert!(batch.add_file(name, b"image", json!({})).is_err());
        }
        assert!(batch.metadata.files.is_empty());
    }

    #[test]
    fn test_add_file_retry_does_not_duplicate_page() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut batch = Batch::create(&make_job(tmp.path()), ScannerInfo::default()).unwrap();
        batch.add_file("page.jpg", b"first", json!({})).unwrap();
        assert!(batch.add_file("page.jpg", b"second", json!({})).is_err());
        assert_eq!(batch.metadata.files.len(), 1);
        assert_eq!(
            fs::read(tmp.path().join(&batch.id).join("page.jpg")).unwrap(),
            b"first"
        );
    }

    #[test]
    fn test_delivery_failure_does_not_mark_batch_completed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut job = make_job(tmp.path());
        job.consume_path = Some(tmp.path().join("missing-consume-directory"));
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch
            .add_file(
                "page.jpg",
                include_bytes!("../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        assert!(batch.complete().is_err());
        assert!(!batch.metadata.completed);
        let persisted: BatchMetadata = serde_json::from_slice(
            &fs::read(tmp.path().join(&batch.id).join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert!(!persisted.completed);
    }

    #[test]
    fn test_rejects_excessive_page_count() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut batch = Batch::create(&make_job(tmp.path()), ScannerInfo::default()).unwrap();
        for i in 0..100 {
            batch
                .add_file(&format!("page{i}.jpg"), b"x", json!({}))
                .unwrap();
        }
        assert!(batch.add_file("page100.jpg", b"x", json!({})).is_err());
    }

    #[test]
    fn test_rejects_page_over_32_megabytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut batch = Batch::create(&make_job(tmp.path()), ScannerInfo::default()).unwrap();
        assert!(
            batch
                .add_file("huge.jpg", &vec![0; 32 * 1024 * 1024 + 1], json!({}))
                .is_err()
        );
        assert!(batch.metadata.files.is_empty());
    }

    #[test]
    fn test_paperless_batch_stages_pdf_and_receipt_before_acknowledgement() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        batch
            .add_file(
                "page.jpg",
                include_bytes!("../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        batch.complete().unwrap();
        assert!(fs::read(batch.pdf_path()).unwrap().starts_with(b"%PDF"));
        let reopened = Batch::open_for_paperless(&job, &batch.id).unwrap().unwrap();
        assert!(reopened.metadata().completed);
        assert!(
            reopened
                .metadata()
                .paperless
                .as_ref()
                .unwrap()
                .task_id
                .is_none()
        );
    }

    #[test]
    fn test_complete_sets_flag() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        assert!(!batch.metadata().completed);
        batch.complete().unwrap();
        assert!(batch.metadata().completed);
    }

    #[test]
    fn test_complete_persists_to_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.complete().unwrap();
        let meta: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(tmp.path().join(&batch.id).join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["completed"], true);
    }

    #[test]
    fn test_no_orphan_tmp_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.add_file("x.jpg", b"x", json!({})).unwrap();
        assert!(!tmp.path().join(&batch.id).join(".metadata.json").exists());
    }

    #[test]
    fn test_is_safe_path() {
        let base = Path::new("/output/batch");
        assert!(is_safe_path(base, Path::new("/output/batch/file.jpg")));
        assert!(!is_safe_path(
            base,
            Path::new("/output/batch/../escape.txt")
        ));
        assert!(!is_safe_path(base, Path::new("/etc/passwd")));
    }

    #[test]
    fn test_now_iso_is_utc() {
        // Timestamps stored in metadata must be UTC so age calculations are correct
        // regardless of server timezone or DST transitions.
        let ts = now_iso();
        assert!(
            ts.ends_with("+00:00") || ts.ends_with('Z'),
            "now_iso() must produce a UTC timestamp, got: {ts}"
        );
    }
}
