use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};

use crate::batch::{BatchMetadata, valid_batch_id};
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
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let start = tokio::time::Instant::now() + Duration::from_secs(STARTUP_DELAY_SECS);
    let mut interval = tokio::time::interval_at(start, Duration::from_secs(SWEEP_INTERVAL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            _ = interval.tick() => {},
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
        }
        if *shutdown.borrow() {
            break;
        }
        sweep_stale_batches(&batches);
        let jobs_snap = lock!(jobs).clone();
        let cfg = lock!(retention).clone();
        if cfg.archive_after_days == 0 && cfg.delete_after_days == 0 {
            continue;
        }
        // run_once does heavy fs work (dir scan, zstd compression) — run off the async thread.
        // Log panics and continue; losing one sweep must not kill the background task.
        let sweep_shutdown = shutdown.clone();
        if tokio::task::spawn_blocking(move || {
            run_once_impl(&jobs_snap, &cfg, || *sweep_shutdown.borrow());
        })
        .await
        .is_err()
        {
            tracing::error!(
                event = "retention.worker_failed",
                "retention: run_once panicked — skipping sweep"
            );
        }
    }
}

fn sweep_stale_batches(batches: &BatchStore) {
    let now = chrono::Utc::now();
    let mut map = lock!(batches);
    let before = map.len();
    map.retain(|id, batch| {
        let batch = match batch.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(err)) => err.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return true,
        };
        let age_hours = chrono::DateTime::parse_from_rfc3339(batch.created_at())
            .ok()
            .map(|t| now.signed_duration_since(t).num_hours())
            .unwrap_or(0);
        if age_hours >= BATCH_TTL_HOURS {
            warn!(event = "retention.stale_batch", batch_id = %crate::logging::safe_batch_id(id), age_hours, "retention: discarding stale in-memory batch");
            false
        } else {
            true
        }
    });
    let removed = before - map.len();
    if removed > 0 {
        info!(
            event = "retention.swept",
            removed, "retention: swept stale in-memory batches"
        );
    }
}

#[cfg(test)]
pub fn run_once(jobs: &[Job], cfg: &RetentionConfig) {
    run_once_impl(jobs, cfg, || false);
}

fn run_once_impl(jobs: &[Job], cfg: &RetentionConfig, should_stop: impl Fn() -> bool) {
    let now = chrono::Utc::now();
    for job in jobs {
        if should_stop() {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&job.output_path) else {
            warn!(
                event = "retention.read_failed",
                "retention: cannot read output_path"
            );
            continue;
        };
        for entry in entries.flatten() {
            if should_stop() {
                break;
            }
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
            else {
                continue;
            };
            if let Some(batch_id) = name.strip_suffix(".tar.zst")
                && path.is_file()
                && valid_batch_id(batch_id)
            {
                handle_archive(&path, &name, &job.output_path, cfg, &now);
            } else if path.is_dir() && valid_batch_id(&name) {
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
                info!(event = "retention.archive_deleted", batch_id = %crate::logging::safe_batch_id(batch_id), age_days = age, "retention: deleted archive");
            }
            Err(_) => {
                warn!(event = "retention.delete_failed", batch_id = %crate::logging::safe_batch_id(batch_id), "retention: failed to delete archive")
            }
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
    if meta
        .paperless
        .as_ref()
        .is_some_and(|p| p.document_id.is_none())
    {
        return;
    }
    let Some(age) = parse_age_days(&meta.created_at, now) else {
        return;
    };

    if cfg.delete_after_days > 0 && age >= cfg.delete_after_days as i64 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => {
                info!(event = "retention.batch_deleted", batch_id = %crate::logging::safe_batch_id(batch_id), age_days = age, "retention: deleted batch")
            }
            Err(_) => {
                warn!(event = "retention.delete_failed", batch_id = %crate::logging::safe_batch_id(batch_id), "retention: failed to delete batch")
            }
        }
    } else if cfg.archive_after_days > 0 && age >= cfg.archive_after_days as i64 {
        match archive_batch(path, batch_id, output_path, &meta.created_at) {
            Ok(()) => info!(
                event = "retention.batch_archived",
                batch_id = %crate::logging::safe_batch_id(batch_id),
                age_days = age,
                "retention: archived batch → .tar.zst"
            ),
            Err(_) => {
                warn!(event = "retention.archive_failed", batch_id = %crate::logging::safe_batch_id(batch_id), "retention: failed to archive batch")
            }
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
    let archive_tmp = output_path.join(format!(
        ".{batch_id}.{}.tar.zst.tmp",
        uuid::Uuid::new_v4().simple()
    ));

    let build_result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&archive_tmp)?;
        let encoder = zstd::Encoder::new(file, 3)?;
        let mut tar = tar::Builder::new(encoder);
        tar.append_dir_all(batch_id, batch_dir)?;
        let encoder = tar.into_inner()?;
        encoder.finish()?.sync_all()?;
        std::fs::rename(&archive_tmp, &archive_path)?;
        Ok(())
    })();
    if build_result.is_err() {
        let _ = std::fs::remove_file(&archive_tmp);
    }
    build_result?;

    // Sidecar preserves original creation date so delete-age is measured from
    // batch creation, not from when archiving ran.
    crate::atomic_file::write(
        &meta_path,
        serde_json::json!({ "created_at": created_at })
            .to_string()
            .as_bytes(),
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

    #[tokio::test]
    async fn shutdown_stops_idle_retention_worker() {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let worker = tokio::spawn(run_forever(
            JobStore::new(vec![]),
            Arc::new(Mutex::new(RetentionConfig::default())),
            BatchStore::default(),
            shutdown_rx,
        ));
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn archives_and_sidecars_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        let dir = root.path().join(id);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("page.jpg"), b"fixture").unwrap();
        archive_batch(&dir, id, root.path(), "2026-10-31T12:00:00Z").unwrap();
        for extension in ["tar.zst", "meta"] {
            let path = root.path().join(format!("{id}.{extension}"));
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn shutdown_stops_retention_between_batches() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ids = [completed_batch(tmp.path()), completed_batch(tmp.path())];
        for id in &ids {
            let path = tmp.path().join(id).join("metadata.json");
            let mut meta: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            meta["created_at"] = json!("2000-01-01T00:00:00+00:00");
            fs::write(path, serde_json::to_vec(&meta).unwrap()).unwrap();
        }
        let checks = std::cell::Cell::new(0);
        run_once_impl(
            &[make_job(tmp.path())],
            &RetentionConfig {
                archive_after_days: 0,
                delete_after_days: 1,
            },
            || {
                checks.set(checks.get() + 1);
                checks.get() >= 3
            },
        );
        assert_eq!(
            ids.iter().filter(|id| tmp.path().join(id).is_dir()).count(),
            1,
            "the active deletion may finish, but the next one must not start"
        );
    }

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
    fn retention_ignores_files_without_a_canonical_batch_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let short_dir = tmp.path().join("deadbeef");
        fs::create_dir(&short_dir).unwrap();
        let batch_id = completed_batch(tmp.path());
        let metadata = fs::read(tmp.path().join(&batch_id).join("metadata.json")).unwrap();
        let mut old: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
        old["created_at"] = json!("2000-01-01T00:00:00+00:00");
        fs::write(
            short_dir.join("metadata.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        let unrelated_archive = tmp.path().join("backup.tar.zst");
        fs::write(&unrelated_archive, b"private backup").unwrap();
        fs::write(
            tmp.path().join("backup.meta"),
            r#"{"created_at":"2000-01-01T00:00:00+00:00"}"#,
        )
        .unwrap();

        run_once(
            &[make_job(tmp.path())],
            &RetentionConfig {
                archive_after_days: 0,
                delete_after_days: 1,
            },
        );
        assert!(short_dir.exists());
        assert_eq!(fs::read(unrelated_archive).unwrap(), b"private backup");
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
    fn test_failed_archive_does_not_publish_partial_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let id = "1234567890abcdef1234567890abcdef";
        let missing = tmp.path().join(id);
        assert!(archive_batch(&missing, id, tmp.path(), "2020-01-01T00:00:00+00:00").is_err());
        assert!(!tmp.path().join(format!("{id}.tar.zst")).exists());
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
    fn test_retention_keeps_old_batch_until_paperless_confirms_document() {
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
        let id = batch.id.clone();
        let meta_path = tmp.path().join(&id).join("metadata.json");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        meta["created_at"] = json!("2000-01-01T00:00:00+00:00");
        fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
        let cfg = RetentionConfig {
            archive_after_days: 1,
            delete_after_days: 1,
        };
        run_once(std::slice::from_ref(&job), &cfg);
        assert!(tmp.path().join(&id).is_dir());
        batch.record_paperless_document(42).unwrap();
        // Reapply the old timestamp because writing the receipt used the in-memory metadata.
        let mut meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        meta["created_at"] = json!("2000-01-01T00:00:00+00:00");
        fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
        run_once(&[job], &cfg);
        assert!(!tmp.path().join(&id).exists());
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
