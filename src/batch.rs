use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::config::Job;

pub fn now_iso() -> String {
    Local::now().to_rfc3339()
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ScannerInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct BatchMetadata {
    pub job_name: String,
    pub created_at: String,
    pub completed: bool,
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub scanner: ScannerInfo,
    #[serde(default)]
    pub job_config: JobConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub filename: String,
    pub received_at: String,
    pub parameters: Value,
}

#[derive(Debug)]
pub struct Batch {
    pub id: String,
    dir: PathBuf,
    consume_path: Option<PathBuf>,
    metadata: BatchMetadata,
}

impl Batch {
    pub fn create(job: &Job, scanner: ScannerInfo) -> Result<Self> {
        let id = Uuid::now_v6(b"nxboss").simple().to_string();
        let dir = job.output_path.join(&id);
        std::fs::create_dir_all(&dir)?;
        let metadata = BatchMetadata {
            job_name: job.job_info["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
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
        };
        let consume_path = job.consume_path.clone();
        let mut batch = Self {
            id,
            dir,
            consume_path,
            metadata,
        };
        batch.flush_metadata()?;
        Ok(batch)
    }

    pub fn add_file(&mut self, filename: &str, content: &[u8], parameters: Value) -> Result<()> {
        let file_path = self.dir.join(filename);
        // Reject path traversal: resolved parent must equal the batch dir
        if file_path
            .canonicalize()
            .unwrap_or(file_path.clone())
            .parent()
            != Some(self.dir.as_path())
            && !is_safe_path(&self.dir, &self.dir.join(filename))
        {
            bail!("bad filename");
        }
        std::fs::write(&file_path, content)?;
        self.metadata.files.push(FileEntry {
            filename: filename.to_string(),
            received_at: now_iso(),
            parameters,
        });
        self.flush_metadata()
    }

    pub fn complete(&mut self) -> Result<()> {
        self.metadata.completed = true;
        self.flush_metadata()?;
        if let Some(ref consume_path) = self.consume_path.clone() {
            self.deliver_pdf(consume_path)?;
        }
        Ok(())
    }

    fn deliver_pdf(&self, consume_path: &Path) -> Result<()> {
        let pages: Vec<Vec<u8>> = self
            .metadata
            .files
            .iter()
            .map(|f| std::fs::read(self.dir.join(&f.filename)))
            .collect::<Result<_, _>>()?;

        let pdf = crate::pdf::assemble_pdf(&pages)?;

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
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let filename = format!("{safe_name}_{ts}.pdf");
        let dest = consume_path.join(&filename);
        std::fs::write(&dest, pdf)?;
        tracing::info!(
            filename = %filename,
            pages = self.metadata.files.len(),
            "PDF delivered to consume folder"
        );
        Ok(())
    }

    #[allow(dead_code)]
    pub fn metadata(&self) -> &BatchMetadata {
        &self.metadata
    }

    fn flush_metadata(&mut self) -> Result<()> {
        let tmp = self.dir.join(".metadata.json");
        let final_path = self.dir.join("metadata.json");
        std::fs::write(&tmp, serde_json::to_string(&self.metadata)?)?;
        std::fs::rename(&tmp, final_path)?;
        Ok(())
    }
}

/// Returns false if `path` escapes `base` via `..` components.
fn is_safe_path(base: &Path, path: &Path) -> bool {
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
    fn test_add_file_rejects_path_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        let result = batch.add_file("../escape.txt", b"evil", json!({}));
        assert!(result.is_err());
        assert!(!tmp.path().join("escape.txt").exists());
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
}
