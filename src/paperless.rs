use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::{Client, Url, multipart};
use serde_json::Value;

use crate::batch::{Batch, PaperlessUploadOptions};
use crate::config::Job;
use crate::config::PaperlessConfig;
use crate::lock;
use crate::state::JobStore;

#[derive(Clone)]
pub struct PaperlessClient {
    http: Client,
    base_url: Url,
    token: String,
    config: PaperlessConfig,
    timezone: chrono_tz::Tz,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Success(u64),
    Failed(String),
    Missing,
}

const RETRY_SETTLE_TIME: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
#[error("Paperless returned HTTP {0}")]
pub struct PaperlessHttpStatus(pub reqwest::StatusCode);

impl PaperlessClient {
    pub fn from_env(config: PaperlessConfig) -> Result<Self> {
        let token = if let Some(ref path) = config.token_file {
            if !path.is_absolute() || !crate::admin_auth::private_regular_file(path)? {
                bail!("Paperless token file must be an absolute private regular file");
            }
            std::fs::read_to_string(path).context("reading Paperless token file")?
        } else {
            std::env::var(&config.token_env).with_context(|| {
                format!(
                    "missing Paperless token environment variable {}",
                    config.token_env
                )
            })?
        };
        Self::new(config, token)
    }

    pub fn new(config: PaperlessConfig, token: String) -> Result<Self> {
        if token.trim().is_empty() {
            bail!("Paperless API token is empty");
        }
        let configured = config.url.trim_end_matches('/');
        let base = configured
            .strip_suffix("/api/documents/post_document")
            .unwrap_or(configured);
        let url = format!("{base}/");
        let base_url = Url::parse(&url).context("invalid Paperless URL")?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            bail!("Paperless URL must be http(s) without credentials, query, or fragment");
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            base_url,
            token,
            timezone: std::env::var("NX_BOSS_TIMEZONE")
                .unwrap_or_else(|_| "Europe/Berlin".into())
                .parse()
                .context("invalid NX_BOSS_TIMEZONE")?,
            config,
        })
    }

    pub async fn upload(
        &self,
        pdf: Vec<u8>,
        title: &str,
        created: &str,
        batch_id: &str,
        options: &PaperlessUploadOptions,
    ) -> Result<String> {
        let mut form = multipart::Form::new()
            .part(
                "document",
                multipart::Part::bytes(pdf)
                    .file_name(format!("{batch_id}.pdf"))
                    .mime_str("application/pdf")?,
            )
            .text("title", title.to_string())
            .text("created", created.to_string());
        for tag_id in &options.tag_ids {
            form = form.text("tags", tag_id.to_string());
        }
        if let Some(field_id) = options.batch_id_field_id {
            form = form.text(
                "custom_fields",
                serde_json::json!({ field_id.to_string(): batch_id }).to_string(),
            );
        }
        let url = self.base_url.join("api/documents/post_document/")?;
        let response = self
            .http
            .post(url)
            .header("Authorization", format!("Token {}", self.token))
            .header("Accept", "application/json; version=10")
            .multipart(form)
            .send()
            .await
            .context("Paperless upload request failed")?;
        if !response.status().is_success() {
            return Err(PaperlessHttpStatus(response.status()).into());
        }
        let task_id: String = response
            .json()
            .await
            .context("invalid Paperless upload response")?;
        uuid::Uuid::parse_str(&task_id).context("Paperless did not return a task UUID")?;
        Ok(task_id)
    }

    pub async fn task_status(&self, task_id: &str) -> Result<TaskStatus> {
        let mut url = self.base_url.join("api/tasks/")?;
        url.query_pairs_mut().append_pair("task_id", task_id);
        let response = self
            .http
            .get(url)
            .header("Authorization", format!("Token {}", self.token))
            .header("Accept", "application/json; version=10")
            .send()
            .await
            .context("Paperless task request failed")?;
        if !response.status().is_success() {
            return Err(PaperlessHttpStatus(response.status()).into());
        }
        let body: Value = response
            .json()
            .await
            .context("invalid Paperless task response")?;
        parse_task_response(&body, task_id)
    }

    /// Search by the batch ID and check the full filename locally: Paperless' `name`
    /// filter is a substring search, not an exact match.
    pub async fn find_batch_task(&self, batch_id: &str) -> Result<Option<(String, TaskStatus)>> {
        let mut url = self.base_url.join("api/tasks/")?;
        url.query_pairs_mut()
            .append_pair("name", batch_id)
            .append_pair("page_size", "100");
        let response = self
            .http
            .get(url)
            .header("Authorization", format!("Token {}", self.token))
            .header("Accept", "application/json; version=10")
            .send()
            .await
            .context("Paperless task search failed")?;
        if !response.status().is_success() {
            return Err(PaperlessHttpStatus(response.status()).into());
        }
        let body: Value = response
            .json()
            .await
            .context("invalid Paperless task search response")?;
        let tasks = body
            .as_array()
            .or_else(|| body.get("results").and_then(Value::as_array))
            .context("Paperless task search has unexpected shape")?;
        if body.get("next").is_some_and(|v| !v.is_null())
            || body
                .get("count")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > tasks.len() as u64)
        {
            bail!("Paperless task search was incomplete");
        }
        let filename = format!("{batch_id}.pdf");
        let mut found: Option<(String, TaskStatus)> = None;
        for task in tasks {
            let task_filename = task
                .pointer("/input_data/filename")
                .and_then(Value::as_str)
                .or_else(|| task.get("task_file_name").and_then(Value::as_str));
            if task_filename != Some(filename.as_str()) {
                continue;
            }
            let id = task
                .get("task_id")
                .and_then(Value::as_str)
                .context("Paperless task is missing its ID")?;
            uuid::Uuid::parse_str(id).context("Paperless task has invalid ID")?;
            let status = parse_task(task)?;
            match (&found, &status) {
                (Some((_, TaskStatus::Success(existing))), TaskStatus::Success(new))
                    if existing != new =>
                {
                    bail!("multiple Paperless documents match one batch");
                }
                (_, TaskStatus::Success(_)) => found = Some((id.to_owned(), status)),
                (None, _) => found = Some((id.to_owned(), status)),
                (Some((_, TaskStatus::Failed(_))), TaskStatus::Pending) => {
                    found = Some((id.to_owned(), status))
                }
                _ => {}
            }
        }
        Ok(found)
    }
}

fn parse_task_response(body: &Value, task_id: &str) -> Result<TaskStatus> {
    let tasks = body
        .as_array()
        .or_else(|| body.get("results").and_then(Value::as_array))
        .context("Paperless task list has unexpected shape")?;
    let Some(task) = tasks
        .iter()
        .find(|task| task["task_id"].as_str() == Some(task_id))
    else {
        return Ok(TaskStatus::Missing);
    };
    parse_task(task)
}

fn document_id(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .filter(|id| *id > 0)
}

fn parse_task(task: &Value) -> Result<TaskStatus> {
    match task["status"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "success" => {
            // V10 always exposes a list. Accept exactly one reference: the
            // receipt stores one ID, so choosing the first of several would
            // silently lose the rest. Only use the V9 field when V10's list
            // is absent, never when it is present but empty or malformed.
            let id = if let Some(ids) = task.get("related_document_ids") {
                let ids = ids.as_array().context("invalid Paperless document IDs")?;
                if ids.len() != 1 {
                    bail!("Paperless task did not produce exactly one document");
                }
                document_id(&ids[0]).context("invalid Paperless document ID")?
            } else {
                document_id(&task["related_document"])
                    .context("Paperless task succeeded without document ID")?
            };
            Ok(TaskStatus::Success(id))
        }
        "failure" | "failed" => Ok(TaskStatus::Failed(
            task["result_data"]["error_message"]
                .as_str()
                .or_else(|| task["result_data"]["reason"].as_str())
                .or_else(|| task["result"].as_str())
                .unwrap_or("Paperless task failed")
                .chars()
                .take(500)
                .collect(),
        )),
        "revoked" => Ok(TaskStatus::Failed("Paperless task revoked".into())),
        "pending" | "started" => Ok(TaskStatus::Pending),
        _ => bail!("unknown Paperless task status"),
    }
}

fn retry_is_due(last_attempt_at: Option<&str>) -> bool {
    let Some(last_attempt) = last_attempt_at.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
    else {
        return false;
    };
    Utc::now()
        .signed_duration_since(last_attempt)
        .to_std()
        .is_ok_and(|elapsed| elapsed >= RETRY_SETTLE_TIME)
}

fn pending_batches(
    jobs: &[Job],
    shutdown: Option<&tokio::sync::watch::Receiver<bool>>,
) -> Vec<Batch> {
    let mut batches = Vec::new();
    for job in jobs {
        if shutdown.is_some_and(|receiver| *receiver.borrow()) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&job.output_path) else {
            continue;
        };
        for entry in entries.flatten() {
            if shutdown.is_some_and(|receiver| *receiver.borrow()) {
                break;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            match Batch::open_for_paperless(job, &id) {
                Ok(Some(batch)) => batches.push(batch),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(event = "paperless.pending_read_failed", batch_id = %crate::logging::safe_batch_id(&id), error_class = crate::logging::error_class(&e), "cannot read pending Paperless batch")
                }
            }
        }
    }
    batches
}

pub(crate) fn pending_delivery_exists(jobs: &[Job]) -> Result<bool> {
    for job in jobs {
        let entries = std::fs::read_dir(&job.output_path).context("reading scan storage")?;
        for entry in entries {
            let entry = entry.context("reading scan storage entry")?;
            let id = entry.file_name().to_string_lossy().to_string();
            if !crate::batch::valid_batch_id(&id) {
                continue;
            }
            if Batch::open_for_paperless(job, &id)?.is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

async fn process_batch(client: &PaperlessClient, batch: &mut Batch) -> Result<()> {
    batch.ensure_paperless_options(PaperlessUploadOptions {
        tag_ids: client.config.tag_ids.clone(),
        batch_id_field_id: client.config.batch_id_field_id,
    })?;
    let receipt = batch
        .metadata()
        .paperless
        .as_ref()
        .context("missing Paperless receipt")?;
    if let Some(task_id) = receipt.task_id.clone() {
        match client.task_status(&task_id).await? {
            TaskStatus::Success(document_id) => batch.record_paperless_document(document_id)?,
            TaskStatus::Failed(error) => batch.record_paperless_error(&error)?,
            TaskStatus::Missing => reconcile_or_retry(client, batch).await?,
            TaskStatus::Pending => {
                let since = receipt
                    .last_attempt_at
                    .as_deref()
                    .unwrap_or(&batch.metadata().created_at);
                if DateTime::parse_from_rfc3339(since)
                    .map(|time| Utc::now().signed_duration_since(time).num_hours() >= 1)
                    .unwrap_or(true)
                {
                    batch.record_paperless_error(
                        "Paperless task pending for over one hour; inspect or retry manually",
                    )?;
                }
            }
        }
    } else if receipt.upload_attempts > 0 || receipt.last_error.is_some() {
        reconcile_or_retry(client, batch).await?;
    } else {
        upload_batch(client, batch).await?;
    }
    Ok(())
}

async fn reconcile_or_retry(client: &PaperlessClient, batch: &mut Batch) -> Result<()> {
    if let Some((task_id, status)) = client.find_batch_task(&batch.id).await? {
        batch.record_paperless_task(task_id)?;
        match status {
            TaskStatus::Success(document_id) => batch.record_paperless_document(document_id)?,
            TaskStatus::Failed(error) => batch.record_paperless_error(&error)?,
            TaskStatus::Pending | TaskStatus::Missing => {}
        }
        return Ok(());
    }
    let last_attempt_at = batch
        .metadata()
        .paperless
        .as_ref()
        .and_then(|receipt| receipt.last_attempt_at.as_deref());
    if retry_is_due(last_attempt_at) {
        upload_batch(client, batch).await?;
    } else if last_attempt_at.is_none() {
        // Old receipts may have sent a POST before this release. Give Paperless
        // time to expose the task before the first automatic retry.
        batch.record_paperless_attempt()?;
    }
    Ok(())
}

fn local_scan_date(created_at: &str, timezone: chrono_tz::Tz) -> Result<String> {
    Ok(DateTime::parse_from_rfc3339(created_at)?
        .with_timezone(&timezone)
        .format("%Y-%m-%d")
        .to_string())
}

async fn upload_batch(client: &PaperlessClient, batch: &mut Batch) -> Result<()> {
    let pdf = tokio::fs::read(batch.pdf_path())
        .await
        .context("reading staged PDF")?;
    let created = local_scan_date(&batch.metadata().created_at, client.timezone)?;
    let title = format!("{} {}", batch.metadata().job_name, created);
    let options = batch
        .metadata()
        .paperless
        .as_ref()
        .and_then(|receipt| receipt.upload_options.clone())
        .context("missing durable Paperless upload options")?;
    // Durable intent must precede the network side effect. A crash here leaves
    // a harmless attempt that will be reconciled before another POST.
    batch.record_paperless_attempt()?;
    let task_id = client
        .upload(pdf, &title, &created, &batch.id, &options)
        .await?;
    batch.record_paperless_task(task_id)
}

#[cfg(test)]
pub async fn run_once(jobs: &[Job], client: &PaperlessClient) {
    run_once_until_shutdown(jobs, client, None).await;
}

async fn run_once_until_shutdown(
    jobs: &[Job],
    client: &PaperlessClient,
    shutdown: Option<&tokio::sync::watch::Receiver<bool>>,
) {
    if shutdown.is_some_and(|receiver| *receiver.borrow()) {
        return;
    }
    let jobs = jobs.to_vec();
    let scan_shutdown = shutdown.cloned();
    let batches =
        match tokio::task::spawn_blocking(move || pending_batches(&jobs, scan_shutdown.as_ref()))
            .await
        {
            Ok(batches) => batches,
            Err(_) => {
                tracing::error!(
                    event = "paperless.worker_failed",
                    "Paperless scan for pending batches panicked"
                );
                return;
            }
        };
    for mut batch in batches {
        if shutdown.is_some_and(|receiver| *receiver.borrow()) {
            break;
        }
        if let Err(e) = process_batch(client, &mut batch).await {
            tracing::warn!(event = "paperless.delivery_failed", batch_id = %crate::logging::safe_batch_id(&batch.id), error_class = crate::logging::error_class(&e), http_status = crate::logging::http_status(&e), "Paperless delivery failed; will retry");
            let _ = batch.record_paperless_error(&e.to_string());
        }
    }
}

pub async fn run_forever(
    jobs: JobStore,
    client: std::sync::Arc<std::sync::Mutex<Option<PaperlessClient>>>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    delivery_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
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
        let Some(client_snapshot) = lock!(client).clone() else {
            continue;
        };
        let snapshot = lock!(jobs).clone();
        let _delivery = delivery_lock.lock().await;
        run_once_until_shutdown(&snapshot, &client_snapshot, Some(&shutdown)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{Batch, ScannerInfo};
    use axum::{
        Json, Router,
        extract::Multipart,
        http::{HeaderMap, StatusCode},
        routing::{get, post},
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn scan_dates_use_berlin_midnight_in_summer_and_winter() {
        let zone: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        assert_eq!(
            local_scan_date("2026-07-01T22:30:00Z", zone).unwrap(),
            "2026-07-02"
        );
        assert_eq!(
            local_scan_date("2026-01-01T23:30:00Z", zone).unwrap(),
            "2026-01-02"
        );
    }

    #[tokio::test]
    async fn shutdown_waits_for_current_upload_and_stops_before_next_batch() {
        use std::sync::Mutex;
        use tokio::sync::{Notify, oneshot};

        const ID: &str = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        #[derive(Clone)]
        struct UploadGate {
            started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
            release: Arc<Notify>,
            calls: Arc<AtomicUsize>,
        }
        async fn upload(
            axum::extract::State(gate): axum::extract::State<UploadGate>,
        ) -> Json<Value> {
            gate.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = gate.started.lock().unwrap().take() {
                let _ = started.send(());
            }
            gate.release.notified().await;
            Json(json!(ID))
        }

        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                assert!(
                    std::env::var_os("NX_BOSS_REQUIRE_HTTP_TESTS").is_none(),
                    "local HTTP mock is required in CI: {err}"
                );
                return;
            }
            Err(err) => panic!("cannot start mock Paperless server: {err}"),
        };
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = UploadGate {
            started: Arc::new(Mutex::new(Some(started_tx))),
            release: release.clone(),
            calls: calls.clone(),
        };
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/api/documents/post_document/", post(upload))
                    .with_state(gate),
            )
            .into_future(),
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name":"Scan", "job_id":0}),
            scan_settings: json!({}),
        };
        let mut first = Batch::create(&job, ScannerInfo::default()).unwrap();
        first.enable_paperless().unwrap();
        first
            .add_file(
                "page.jpg",
                include_bytes!("../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        first.complete().unwrap();
        let mut second = Batch::create(&job, ScannerInfo::default()).unwrap();
        second.enable_paperless().unwrap();
        second
            .add_file(
                "page.jpg",
                include_bytes!("../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        second.complete().unwrap();
        let client = PaperlessClient::new(
            PaperlessConfig {
                url: format!("http://{address}"),
                token_env: "IGNORED".into(),
                token_file: None,
                tag_ids: vec![],
                batch_id_field_id: None,
            },
            "secret".into(),
        )
        .unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let worker = tokio::spawn(run_forever(
            JobStore::new(vec![job.clone()]),
            std::sync::Arc::new(std::sync::Mutex::new(Some(client))),
            shutdown_rx,
            std::sync::Arc::new(tokio::sync::Mutex::new(())),
        ));
        tokio::time::timeout(Duration::from_secs(3), started_rx)
            .await
            .unwrap()
            .unwrap();
        shutdown_tx.send(true).unwrap();
        tokio::task::yield_now().await;
        assert!(!worker.is_finished(), "in-flight upload must finish first");
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let completed = [first, second]
            .iter()
            .filter(|batch| {
                Batch::open_for_paperless(&job, &batch.id)
                    .unwrap()
                    .unwrap()
                    .metadata()
                    .paperless
                    .as_ref()
                    .unwrap()
                    .task_id
                    .is_some()
            })
            .count();
        assert_eq!(completed, 1, "one batch stays pending for the next startup");
        server.abort();
    }

    #[test]
    fn test_v3_task_status_pagination() {
        let id = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        let response = json!({"count": 1, "results": [{"task_id": id, "status": "SUCCESS", "related_document": "416"}]});
        assert_eq!(
            parse_task_response(&response, id).unwrap(),
            TaskStatus::Success(416)
        );
    }

    #[test]
    fn v10_task_success_uses_lowercase_status_and_related_document_ids() {
        let id = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        let response = json!({"count": 1, "results": [{
            "task_id": id,
            "status": "success",
            "related_document_ids": [416],
            "input_data": {"filename": "batch.pdf"},
            "result_data": {"document_id": 416}
        }]});
        assert_eq!(
            parse_task_response(&response, id).unwrap(),
            TaskStatus::Success(416)
        );
    }

    #[test]
    fn v10_terminal_and_running_statuses_use_real_result_fields() {
        let id = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        for status in ["pending", "started", "PENDING", "STARTED"] {
            let body =
                json!({"results": [{"task_id": id, "status": status, "related_document_ids": []}]});
            assert_eq!(parse_task_response(&body, id).unwrap(), TaskStatus::Pending);
        }
        let failure = json!({"results": [{"task_id": id, "status": "failure",
            "related_document_ids": [], "result_data": {"error_message": "Import abgelehnt"}}]});
        assert_eq!(
            parse_task_response(&failure, id).unwrap(),
            TaskStatus::Failed("Import abgelehnt".into())
        );
        let reason = json!({"results": [{"task_id": id, "status": "failure",
            "related_document_ids": [], "result_data": {"reason": "Ungültige Datei"}}]});
        assert_eq!(
            parse_task_response(&reason, id).unwrap(),
            TaskStatus::Failed("Ungültige Datei".into())
        );
        let revoked =
            json!({"results": [{"task_id": id, "status": "revoked", "related_document_ids": []}]});
        assert_eq!(
            parse_task_response(&revoked, id).unwrap(),
            TaskStatus::Failed("Paperless task revoked".into())
        );
        let old_failure =
            json!({"results": [{"task_id": id, "status": "FAILURE", "result": "legacy error"}]});
        assert_eq!(
            parse_task_response(&old_failure, id).unwrap(),
            TaskStatus::Failed("legacy error".into())
        );
        let unknown =
            json!({"results": [{"task_id": id, "status": "queued", "related_document_ids": []}]});
        assert!(parse_task_response(&unknown, id).is_err());
    }

    #[test]
    fn success_needs_exactly_one_valid_document_reference() {
        let id = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        for ids in [
            json!([]),
            json!([0]),
            json!([-1]),
            json!(["wrong"]),
            json!([416, 417]),
        ] {
            let body = json!({"results": [{"task_id": id, "status": "success",
                "related_document_ids": ids, "result_data": {"document_id": 416}}]});
            assert!(
                parse_task_response(&body, id).is_err(),
                "invalid IDs must not confirm import: {ids}"
            );
        }
        let missing = json!({"results": [{"task_id": id, "status": "success", "result_data": {"document_id": 416}}]});
        assert!(parse_task_response(&missing, id).is_err());
        for legacy in [json!(416), json!("416")] {
            let body = json!({"results": [{"task_id": id, "status": "SUCCESS", "related_document": legacy}]});
            assert_eq!(
                parse_task_response(&body, id).unwrap(),
                TaskStatus::Success(416)
            );
        }
    }

    #[test]
    fn retry_requires_a_valid_elapsed_settle_window() {
        assert!(!retry_is_due(None));
        assert!(!retry_is_due(Some("invalid")));
        assert!(!retry_is_due(Some(&Utc::now().to_rfc3339())));
        assert!(retry_is_due(Some(
            &(Utc::now() - chrono::Duration::minutes(3)).to_rfc3339()
        )));
    }

    #[tokio::test]
    async fn unknown_upload_is_retried_only_after_task_search_and_settle_window() {
        #[derive(Default)]
        struct Mock {
            uploads: AtomicUsize,
            searches: AtomicUsize,
        }
        async fn upload(axum::extract::State(mock): axum::extract::State<Arc<Mock>>) -> StatusCode {
            mock.uploads.fetch_add(1, Ordering::SeqCst);
            StatusCode::SERVICE_UNAVAILABLE
        }
        async fn tasks(axum::extract::State(mock): axum::extract::State<Arc<Mock>>) -> Json<Value> {
            mock.searches.fetch_add(1, Ordering::SeqCst);
            Json(json!({"count": 0, "results": []}))
        }
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                assert!(std::env::var_os("NX_BOSS_REQUIRE_HTTP_TESTS").is_none());
                return;
            }
            Err(err) => panic!("cannot start mock Paperless server: {err}"),
        };
        let address = listener.local_addr().unwrap();
        let mock = Arc::new(Mock::default());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/api/documents/post_document/", post(upload))
                    .route("/api/tasks/", get(tasks))
                    .with_state(mock.clone()),
            )
            .into_future(),
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name":"Scan", "job_id":0}),
            scan_settings: json!({}),
        };
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
        let client = PaperlessClient::new(
            PaperlessConfig {
                url: format!("http://{address}"),
                token_env: "IGNORED".into(),
                token_file: None,
                tag_ids: vec![],
                batch_id_field_id: None,
            },
            "secret".into(),
        )
        .unwrap();
        run_once(std::slice::from_ref(&job), &client).await;
        run_once(std::slice::from_ref(&job), &client).await;
        assert_eq!(mock.uploads.load(Ordering::SeqCst), 1);
        assert_eq!(mock.searches.load(Ordering::SeqCst), 1);
        let path = tmp.path().join(&batch.id).join("metadata.json");
        let mut metadata: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        metadata["paperless"]["last_attempt_at"] =
            json!((Utc::now() - chrono::Duration::minutes(3)).to_rfc3339());
        crate::atomic_file::write(&path, &serde_json::to_vec(&metadata).unwrap()).unwrap();
        run_once(std::slice::from_ref(&job), &client).await;
        assert_eq!(mock.searches.load(Ordering::SeqCst), 2);
        assert_eq!(mock.uploads.load(Ordering::SeqCst), 2);
        let receipt: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(receipt["paperless"]["upload_attempts"], 2);
        server.abort();
    }

    #[tokio::test]
    async fn retry_keeps_original_paperless_tags_and_batch_field_after_config_change() {
        use std::sync::Mutex;

        const TASK_ID: &str = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        #[derive(Default)]
        struct Mock {
            forms: Mutex<Vec<Vec<(String, String)>>>,
        }
        async fn upload(
            axum::extract::State(mock): axum::extract::State<Arc<Mock>>,
            mut multipart: Multipart,
        ) -> (StatusCode, Json<Value>) {
            let mut fields = Vec::new();
            while let Some(field) = multipart.next_field().await.unwrap() {
                let name = field.name().unwrap().to_owned();
                if name != "document" {
                    fields.push((name, field.text().await.unwrap()));
                }
            }
            let mut forms = mock.forms.lock().unwrap();
            forms.push(fields);
            if forms.len() == 1 {
                (StatusCode::SERVICE_UNAVAILABLE, Json(json!("unavailable")))
            } else {
                (StatusCode::OK, Json(json!(TASK_ID)))
            }
        }
        async fn tasks() -> Json<Value> {
            Json(json!({"count": 0, "results": []}))
        }
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                assert!(std::env::var_os("NX_BOSS_REQUIRE_HTTP_TESTS").is_none());
                return;
            }
            Err(err) => panic!("cannot start mock Paperless server: {err}"),
        };
        let address = listener.local_addr().unwrap();
        let mock = Arc::new(Mock::default());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/api/documents/post_document/", post(upload))
                    .route("/api/tasks/", get(tasks))
                    .with_state(mock.clone()),
            )
            .into_future(),
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name": "Scan", "job_id": 0}),
            scan_settings: json!({}),
        };
        let mut batch = Batch::create(&job, ScannerInfo::default()).unwrap();
        batch
            .enable_paperless_with_options(PaperlessUploadOptions {
                tag_ids: vec![7, 9],
                batch_id_field_id: Some(11),
            })
            .unwrap();
        batch
            .add_file(
                "page.jpg",
                include_bytes!("../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        batch.complete().unwrap();
        let make_client = |tag_ids, batch_id_field_id| {
            PaperlessClient::new(
                PaperlessConfig {
                    url: format!("http://{address}"),
                    token_env: "IGNORED".into(),
                    token_file: None,
                    tag_ids,
                    batch_id_field_id,
                },
                "secret".into(),
            )
            .unwrap()
        };
        run_once(
            std::slice::from_ref(&job),
            &make_client(vec![7, 9], Some(11)),
        )
        .await;
        let path = tmp.path().join(&batch.id).join("metadata.json");
        let mut metadata: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        metadata["paperless"]["last_attempt_at"] =
            json!((Utc::now() - chrono::Duration::minutes(3)).to_rfc3339());
        crate::atomic_file::write(&path, &serde_json::to_vec(&metadata).unwrap()).unwrap();
        run_once(std::slice::from_ref(&job), &make_client(vec![99], Some(88))).await;
        let forms = mock.forms.lock().unwrap();
        assert_eq!(forms.len(), 2);
        assert_eq!(forms[0], forms[1]);
        assert_eq!(
            forms[1].iter().filter(|(name, _)| name == "tags").count(),
            2
        );
        assert!(forms[1].contains(&("custom_fields".into(), json!({"11": batch.id}).to_string())));
        server.abort();
    }

    #[tokio::test]
    async fn lost_upload_response_reconciles_task_without_second_post() {
        use std::sync::Mutex;

        const ID: &str = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        #[derive(Default)]
        struct Mock {
            uploads: AtomicUsize,
            filename: Mutex<Option<String>>,
        }
        async fn upload(
            axum::extract::State(mock): axum::extract::State<Arc<Mock>>,
            mut multipart: Multipart,
        ) -> StatusCode {
            while let Some(field) = multipart.next_field().await.unwrap() {
                if field.name() == Some("document") {
                    *mock.filename.lock().unwrap() = field.file_name().map(str::to_owned);
                    break;
                }
            }
            mock.uploads.fetch_add(1, Ordering::SeqCst);
            // The mock accepted the upload but the client cannot use the response.
            StatusCode::SERVICE_UNAVAILABLE
        }
        async fn tasks(axum::extract::State(mock): axum::extract::State<Arc<Mock>>) -> Json<Value> {
            let results =
                mock.filename
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map_or_else(Vec::new, |filename| {
                        vec![json!({
                            "task_id": ID,
                            "status": "success",
                            "input_data": {"filename": filename},
                            "related_document_ids": [42]
                        })]
                    });
            Json(json!({"count": results.len(), "results": results}))
        }
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                assert!(std::env::var_os("NX_BOSS_REQUIRE_HTTP_TESTS").is_none());
                return;
            }
            Err(err) => panic!("cannot start mock Paperless server: {err}"),
        };
        let address = listener.local_addr().unwrap();
        let mock = Arc::new(Mock::default());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/api/documents/post_document/", post(upload))
                    .route("/api/tasks/", get(tasks))
                    .with_state(mock.clone()),
            )
            .into_future(),
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name":"Scan", "job_id":0}),
            scan_settings: json!({}),
        };
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
        let client = PaperlessClient::new(
            PaperlessConfig {
                url: format!("http://{address}"),
                token_env: "IGNORED".into(),
                token_file: None,
                tag_ids: vec![],
                batch_id_field_id: None,
            },
            "secret".into(),
        )
        .unwrap();

        run_once(std::slice::from_ref(&job), &client).await;
        let receipt: Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join(&batch.id).join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["paperless"]["upload_attempts"], 1);
        run_once(std::slice::from_ref(&job), &client).await;
        assert_eq!(mock.uploads.load(Ordering::SeqCst), 1);
        assert!(
            Batch::open_for_paperless(&job, &batch.id)
                .unwrap()
                .is_none()
        );
        server.abort();
    }

    #[test]
    fn test_task_status_does_not_accept_different_task() {
        let response = json!({"results": [{"task_id": "other", "status": "SUCCESS", "related_document": "416"}]});
        assert_eq!(
            parse_task_response(&response, "wanted").unwrap(),
            TaskStatus::Missing
        );
    }

    #[test]
    fn test_client_rejects_url_with_credentials() {
        let config = PaperlessConfig {
            url: "http://user:pass@paperless:8000".into(),
            token_env: "X".into(),
            token_file: None,
            tag_ids: vec![],
            batch_id_field_id: None,
        };
        assert!(PaperlessClient::new(config, "secret".into()).is_err());
    }

    #[tokio::test]
    async fn test_upload_and_task_lookup_against_v3_shape() {
        const ID: &str = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        async fn upload(headers: HeaderMap, mut multipart: Multipart) -> Json<Value> {
            assert_eq!(headers["authorization"], "Token secret");
            assert_eq!(headers["accept"], "application/json; version=10");
            let mut parts = Vec::new();
            while let Some(field) = multipart.next_field().await.unwrap() {
                let name = field.name().unwrap().to_string();
                let data = field.bytes().await.unwrap();
                parts.push((name, data));
            }
            assert!(
                parts
                    .iter()
                    .any(|(name, data)| name == "document" && data.as_ref() == b"%PDF-test")
            );
            assert!(
                parts
                    .iter()
                    .any(|(name, data)| name == "tags" && data.as_ref() == b"7")
            );
            assert!(parts.iter().any(|(name, data)| name == "custom_fields"
                && String::from_utf8_lossy(data).contains("batch-1")));
            Json(json!(ID))
        }
        async fn task(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers["accept"], "application/json; version=10");
            Json(json!({"count":1,"next":null,"results":[{
                "task_id":ID,"status":"success","related_document_ids":[42],
                "input_data":{"filename":"batch-1.pdf"},
                "result_data":{"document_id":42}
            }]}))
        }
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                assert!(
                    std::env::var_os("NX_BOSS_REQUIRE_HTTP_TESTS").is_none(),
                    "local HTTP mock is required in CI: {err}"
                );
                eprintln!("skipping loopback HTTP test: sandbox disallows socket bind");
                return;
            }
            Err(err) => panic!("cannot start mock Paperless server: {err}"),
        };
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/api/documents/post_document/", post(upload))
                    .route("/api/tasks/", get(task)),
            )
            .into_future(),
        );
        let config = PaperlessConfig {
            url: format!("http://{address}/api/documents/post_document/"),
            token_env: "IGNORED".into(),
            token_file: None,
            tag_ids: vec![7],
            batch_id_field_id: Some(11),
        };
        let client = PaperlessClient::new(config, "secret".into()).unwrap();
        let task_id = client
            .upload(
                b"%PDF-test".to_vec(),
                "A scan",
                "2026-09-29",
                "batch-1",
                &PaperlessUploadOptions {
                    tag_ids: vec![7],
                    batch_id_field_id: Some(11),
                },
            )
            .await
            .unwrap();
        assert_eq!(task_id, ID);
        assert_eq!(
            client.task_status(ID).await.unwrap(),
            TaskStatus::Success(42)
        );
        server.abort();
    }

    #[tokio::test]
    async fn test_delivery_worker_records_task_then_document_without_reupload() {
        const ID: &str = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        async fn upload(
            axum::extract::State(calls): axum::extract::State<Arc<AtomicUsize>>,
        ) -> Json<Value> {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(json!(ID))
        }
        async fn task(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers["accept"], "application/json; version=10");
            Json(json!({"count":1,"next":null,"results":[{
                "task_id":ID,"status":"success","related_document_ids":[42],
                "result_data":{"document_id":42}
            }]}))
        }
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                assert!(
                    std::env::var_os("NX_BOSS_REQUIRE_HTTP_TESTS").is_none(),
                    "local HTTP mock is required in CI: {err}"
                );
                return;
            }
            Err(err) => panic!("cannot start mock Paperless server: {err}"),
        };
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/api/documents/post_document/", post(upload))
                    .route("/api/tasks/", get(task))
                    .with_state(calls.clone()),
            )
            .into_future(),
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name":"Scan", "job_id":0}),
            scan_settings: json!({}),
        };
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
        let client = PaperlessClient::new(
            PaperlessConfig {
                url: format!("http://{address}"),
                token_env: "IGNORED".into(),
                token_file: None,
                tag_ids: vec![],
                batch_id_field_id: None,
            },
            "secret".into(),
        )
        .unwrap();
        run_once(std::slice::from_ref(&job), &client).await;
        let queued = Batch::open_for_paperless(&job, &batch.id).unwrap().unwrap();
        assert_eq!(
            queued
                .metadata()
                .paperless
                .as_ref()
                .unwrap()
                .task_id
                .as_deref(),
            Some(ID)
        );
        run_once(std::slice::from_ref(&job), &client).await;
        assert!(
            Batch::open_for_paperless(&job, &batch.id)
                .unwrap()
                .is_none()
        );
        let saved: Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join(&batch.id).join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["paperless"]["document_id"], 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn malformed_success_keeps_saved_task_pending_without_reupload() {
        const ID: &str = "6f421a77-2013-4a1b-a42f-e5b763d80126";
        async fn upload(
            axum::extract::State(calls): axum::extract::State<Arc<AtomicUsize>>,
        ) -> StatusCode {
            calls.fetch_add(1, Ordering::SeqCst);
            StatusCode::CREATED
        }
        async fn task(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers["accept"], "application/json; version=10");
            Json(json!({"count":1,"results":[{
                "task_id":ID,"status":"success","related_document_ids":[42,43]
            }]}))
        }
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                assert!(std::env::var_os("NX_BOSS_REQUIRE_HTTP_TESTS").is_none());
                return;
            }
            Err(err) => panic!("cannot start mock Paperless server: {err}"),
        };
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/api/documents/post_document/", post(upload))
                    .route("/api/tasks/", get(task))
                    .with_state(calls.clone()),
            )
            .into_future(),
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name":"Scan", "job_id":0}),
            scan_settings: json!({}),
        };
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
        batch.record_paperless_task(ID.into()).unwrap();
        let client = PaperlessClient::new(
            PaperlessConfig {
                url: format!("http://{address}"),
                token_env: "IGNORED".into(),
                token_file: None,
                tag_ids: vec![],
                batch_id_field_id: None,
            },
            "secret".into(),
        )
        .unwrap();
        run_once(std::slice::from_ref(&job), &client).await;
        let reopened = Batch::open_for_paperless(&job, &batch.id).unwrap().unwrap();
        let receipt = reopened.metadata().paperless.as_ref().unwrap();
        assert_eq!(receipt.task_id.as_deref(), Some(ID));
        assert_eq!(receipt.document_id, None);
        assert!(receipt.last_error.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }
}
