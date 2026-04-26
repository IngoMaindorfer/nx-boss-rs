use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};

use crate::batch::BatchMetadata;
use crate::config::{Job, RetentionConfig};
use crate::lock;
use crate::state::{BatchStore, JobStore};

const STARTUP_DELAY_SECS: u64 = 60;
const SWEEP_INTERVAL_SECS: u64 = 3600;
/// In-memory batches not completed within this many hours are removed as stale.
const BATCH_TTL_HOURS: i64 = 24;

pub async fn run_forever(
    jobs: JobStore,
    retention: Arc<Mutex<RetentionConfig>>,
    batches: BatchStore,
) {
    let start = tokio::time::Instant::now() + Duration::from_secs(STARTUP_DELAY_SECS);
    let mut interval = tokio::time::interval_at(start, Duration::from_secs(SWEEP_INTERVAL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        sweep_stale_batches(&batches);
        let jobs_snap = lock!(jobs).clone();
        let cfg = lock!(retention).clone();
        if cfg.archive_after_days == 0 && cfg.delete_after_days == 0 {
            continue;
        }
        // run_once does heavy fs work (dir scan, zstd compression) — run off the async thread.
        // Log panics and continue; losing one sweep must not kill the background task.
        if let Err(e) = tokio::task::spawn_blocking(move || run_once(&jobs_snap, &cfg)).await {
            tracing::error!(error = %e, "retention: run_once panicked — skipping sweep");
        }
    }
}

fn sweep_stale_batches(batches: &BatchStore) {
    let now = chrono::Utc::now();
    let mut map = lock!(batches);
    let before = map.len();
    map.retain(|id, batch| {
        let age_hours = chrono::DateTime::parse_from_rfc3339(batch.created_at())
            .ok()
            .map(|t| now.signed_duration_since(t).num_hours())
            .unwrap_or(0);
        if age_hours >= BATCH_TTL_HOURS {
            warn!(batch_id = %id, age_hours, "retention: discarding stale in-memory batch");
            false
        } else {
            true
        }
    });
    let removed = before - map.len();
    if removed > 0 {
        info!(removed, "retention: swept stale in-memory batches");
    }
}

pub fn run_once(jobs: &[Job], cfg: &RetentionConfig) {
    let now = chrono::Utc::now();
    for job in jobs {
        let Ok(entries) = std::fs::read_dir(&job.output_path) else {
            warn!(path = %job.output_path.display(), "retention: cannot read output_path");
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
            else {
                continue;
            };
            if name.ends_with(".tar.zst") && path.is_file() {
                handle_archive(&path, &name, &job.output_path, cfg, &now);
            } else if path.is_dir() && name.chars().all(|c| c.is_ascii_hexdigit()) {
                handle_batch_dir(&path, &name, &job.output_path, cfg, &now);
            }
        }
    }
}

fn handle_archive(
    path: &Path,
    name: &str,
    output_path: &Path,
    cfg: &RetentionConfig,
    now: &chrono::DateTime<chrono::Utc>,
) {
    if cfg.delete_after_days == 0 {
        return;
    }
    let batch_id = name.trim_end_matches(".tar.zst");
    let age = archive_age_days(batch_id, output_path, path, now);
    if age >= cfg.delete_after_days as i64 {
        match std::fs::remove_file(path) {
            Ok(()) => {
                let _ = std::fs::remove_file(output_path.join(format!("{batch_id}.meta")));
                info!(batch_id, age_days = age, "retention: deleted archive");
            }
            Err(e) => warn!(batch_id, error = %e, "retention: failed to delete archive"),
        }
    }
}

fn handle_batch_dir(
    path: &Path,
    batch_id: &str,
    output_path: &Path,
    cfg: &RetentionConfig,
    now: &chrono::DateTime<chrono::Utc>,
) {
    let Ok(text) = std::fs::read_to_string(path.join("metadata.json")) else {
        return;
    };
    let Ok(meta) = serde_json::from_str::<BatchMetadata>(&text) else {
        return;
    };
    if !meta.completed {
        return;
    }
    let Some(age) = parse_age_days(&meta.created_at, now) else {
        return;
    };

    if cfg.delete_after_days > 0 && age >= cfg.delete_after_days as i64 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => info!(batch_id, age_days = age, "retention: deleted batch"),
            Err(e) => warn!(batch_id, error = %e, "retention: failed to delete batch"),
        }
    } else if cfg.archive_after_days > 0 && age >= cfg.archive_after_days as i64 {
        match archive_batch(path, batch_id, output_path, &meta.created_at) {
            Ok(()) => info!(
                batch_id,
                age_days = age,
                "retention: archived batch → .tar.zst"
            ),
            Err(e) => warn!(batch_id, error = %e, "retention: failed to archive batch"),
        }
    }
}

fn archive_batch(
    batch_dir: &Path,
    batch_id: &str,
    output_path: &Path,
    created_at: &str,
) -> Result<()> {
    let archive_path = output_path.join(format!("{batch_id}.tar.zst"));
    let meta_path = output_path.join(format!("{batch_id}.meta"));

    let file = std::fs::File::create(&archive_path)?;
    // Level 3: good compression ratio with fast encode (~300 MB/s on modern hw)
    let encoder = zstd::Encoder::new(file, 3)?;
    let mut tar = tar::Builder::new(encoder);
    tar.append_dir_all(batch_id, batch_dir)?;
    let encoder = tar.into_inner()?;
    encoder.finish()?;

    // Sidecar preserves original creation date so delete-age is measured from
    // batch creation, not from when archiving ran.
    std::fs::write(
        &meta_path,
        serde_json::json!({ "created_at": created_at }).to_string(),
    )?;

    std::fs::remove_dir_all(batch_dir)?;
    Ok(())
}

fn archive_age_days(
    batch_id: &str,
    output_path: &Path,
    archive_path: &Path,
    now: &chrono::DateTime<chrono::Utc>,
) -> i64 {
    // Prefer sidecar created_at so age is measured from original batch creation
    let sidecar = output_path.join(format!("{batch_id}.meta"));
    if let Ok(text) = std::fs::read_to_string(&sidecar)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(ts) = v["created_at"].as_str()
        && let Some(age) = parse_age_days(ts, now)
    {
        return age;
    }
    // Fallback: archive file mtime
    std::fs::metadata(archive_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            now.signed_duration_since(dt).num_days()
        })
        .unwrap_or(0)
}

fn parse_age_days(ts: &str, now: &chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let created = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some(now.signed_duration_since(created).num_days())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{Batch, ScannerInfo};
    use crate::config::Job;
    use serde_json::json;
    use std::fs;

    fn make_job(dir: &std::path::Path) -> Job {
        Job {
            output_path: dir.to_path_buf(),
            consume_path: None,
            job_info: json!({"name": "t", "job_id": 0, "color": "#fff", "type": 0,
                             "job_setting": {}, "hierarchy_list": null}),
            scan_settings: json!({}),
        }
    }

    fn completed_batch(dir: &std::path::Path) -> String {
        let job = make_job(dir);
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch.complete().unwrap();
        batch.id.clone()
    }

    #[test]
    fn test_run_once_no_op_when_both_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch_id = completed_batch(tmp.path());
        let cfg = RetentionConfig {
            archive_after_days: 0,
            delete_after_days: 0,
        };
        run_once(&[make_job(tmp.path())], &cfg);
        assert!(tmp.path().join(&batch_id).is_dir(), "dir must survive");
    }

    #[test]
    fn test_run_once_does_not_delete_incomplete_batch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = make_job(tmp.path());
        let batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        // Do NOT complete the batch
        let cfg = RetentionConfig {
            archive_after_days: 0,
            delete_after_days: 0,
        };
        run_once(&[make_job(tmp.path())], &cfg);
        assert!(tmp.path().join(&batch.id).is_dir());
    }

    #[test]
    fn test_archive_batch_creates_tar_zst_and_removes_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch_id = completed_batch(tmp.path());
        let dir = tmp.path().join(&batch_id);
        archive_batch(&dir, &batch_id, tmp.path(), "2020-01-01T00:00:00+00:00").unwrap();
        assert!(!dir.exists(), "original dir must be removed");
        assert!(tmp.path().join(format!("{batch_id}.tar.zst")).exists());
        assert!(tmp.path().join(format!("{batch_id}.meta")).exists());
    }

    #[test]
    fn test_run_once_deletes_old_dir_when_delete_threshold_zero_days() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch_id = completed_batch(tmp.path());
        // Backdate the created_at in metadata.json to 100 days ago
        let meta_path = tmp.path().join(&batch_id).join("metadata.json");
        let mut meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
        meta["created_at"] = json!("2000-01-01T00:00:00+00:00");
        fs::write(&meta_path, serde_json::to_string(&meta).unwrap()).unwrap();

        let cfg = RetentionConfig {
            archive_after_days: 0,
            delete_after_days: 1,
        };
        run_once(&[make_job(tmp.path())], &cfg);
        assert!(
            !tmp.path().join(&batch_id).is_dir(),
            "old batch must be deleted"
        );
    }

    #[test]
    fn test_run_once_archives_old_dir_when_archive_threshold_met() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch_id = completed_batch(tmp.path());
        let meta_path = tmp.path().join(&batch_id).join("metadata.json");
        let mut meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
        meta["created_at"] = json!("2000-01-01T00:00:00+00:00");
        fs::write(&meta_path, serde_json::to_string(&meta).unwrap()).unwrap();

        let cfg = RetentionConfig {
            archive_after_days: 1,
            delete_after_days: 0,
        };
        run_once(&[make_job(tmp.path())], &cfg);
        assert!(
            !tmp.path().join(&batch_id).is_dir(),
            "dir must be removed after archive"
        );
        assert!(tmp.path().join(format!("{batch_id}.tar.zst")).exists());
    }

    #[test]
    fn test_run_once_deletes_archive_when_delete_threshold_met() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch_id = completed_batch(tmp.path());
        let dir = tmp.path().join(&batch_id);
        archive_batch(&dir, &batch_id, tmp.path(), "2000-01-01T00:00:00+00:00").unwrap();

        let cfg = RetentionConfig {
            archive_after_days: 1,
            delete_after_days: 1,
        };
        run_once(&[make_job(tmp.path())], &cfg);
        assert!(!tmp.path().join(format!("{batch_id}.tar.zst")).exists());
    }
}
