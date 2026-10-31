use askama::Template;
use axum::{
    Form, Json,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path as FsPath;

use super::UiState;
use super::ui::{JobRow, job_rows, render};
use crate::build_info::BuildInfo;
use crate::config::{
    DEFAULT_COLOR, DEFAULT_JPEG_QUALITY, DEFAULT_PIXEL_FORMAT, DEFAULT_RESOLUTION, DEFAULT_SOURCE,
    Job, KEY_JPEG_QUALITY, KEY_PIXEL_FORMAT, KEY_PIXEL_FORMATS, KEY_RESOLUTION, KEY_SOURCE,
    MAX_JOB_NAME_LEN, MAX_PATH_LEN, RawJob,
};
use crate::lock;
use crate::translations::Translations;

#[derive(Template)]
#[template(path = "jobs_list.html")]
struct JobsListTpl {
    jobs: Vec<JobRow>,
    t: &'static Translations,
    build: &'static BuildInfo,
}

#[derive(Template)]
#[template(path = "jobs_form.html")]
struct JobsFormTpl {
    editing: bool,
    job_id: usize,
    name: String,
    color: String,
    consume_path: String,
    resolution: u32,
    jpeg_quality: u8,
    pixel_format: String,
    source: String,
    error: Option<String>,
    t: &'static Translations,
    build: &'static BuildInfo,
}

#[derive(Template)]
#[template(path = "jobs_expert.html")]
struct JobsExpertTpl {
    job_id: usize,
    name: String,
    job_settings_json: String,
    scan_settings_json: String,
    error: bool,
    t: &'static Translations,
    build: &'static BuildInfo,
}

#[derive(Deserialize)]
pub struct ExpertFormData {
    job_settings_json: String,
    scan_settings_json: String,
}

fn parse_expert_job(existing: &Job, form: &ExpertFormData) -> anyhow::Result<Job> {
    anyhow::ensure!(
        form.job_settings_json.len() <= 64 * 1024 && form.scan_settings_json.len() <= 64 * 1024,
        "expert settings too large"
    );
    let job_settings: HashMap<String, serde_json::Value> =
        serde_json::from_str(&form.job_settings_json)?;
    let scan_settings: HashMap<String, serde_json::Value> =
        serde_json::from_str(&form.scan_settings_json)?;
    let (name, mut raw): (String, RawJob) = existing.to_raw();
    raw.job_settings = Some(job_settings);
    raw.scan_settings = Some(scan_settings);
    let id = existing.job_info["job_id"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("invalid job ID"))?;
    Job::parse(usize::try_from(id)?, name, raw)
}

fn expert_tpl(
    job: &Job,
    error: bool,
    t: &'static Translations,
    build: &'static BuildInfo,
) -> JobsExpertTpl {
    let (_, raw) = job.to_raw();
    JobsExpertTpl {
        job_id: job.job_info["job_id"].as_u64().unwrap_or(0) as usize,
        name: job.name().to_owned(),
        job_settings_json: serde_json::to_string_pretty(&raw.job_settings.unwrap_or_default())
            .unwrap_or_default(),
        scan_settings_json: serde_json::to_string_pretty(&raw.scan_settings.unwrap_or_default())
            .unwrap_or_default(),
        error,
        t,
        build,
    }
}

pub async fn jobs_expert_get(Path(id): Path<usize>, UiState(state): UiState) -> Response {
    let jobs = lock!(state.jobs);
    let Some(job) = jobs
        .iter()
        .find(|job| job.job_info["job_id"].as_u64() == Some(id as u64))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    render(expert_tpl(job, false, state.translations, state.build_info))
}

pub async fn jobs_expert_post(
    Path(id): Path<usize>,
    headers: HeaderMap,
    UiState(state): UiState,
    Form(form): Form<ExpertFormData>,
) -> Response {
    let _change = lock!(state.config_change_lock);
    let mut jobs = lock!(state.jobs).clone();
    let Some(index) = jobs
        .iter()
        .position(|job| job.job_info["job_id"].as_u64() == Some(id as u64))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let updated = match parse_expert_job(&jobs[index], &form) {
        Ok(job) => job,
        Err(_) => {
            return render(expert_tpl(
                &jobs[index],
                true,
                state.translations,
                state.build_info,
            ));
        }
    };
    jobs[index] = updated;
    if state
        .persist_config(&jobs, &lock!(state.retention), *lock!(state.next_job_id))
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    *lock!(state.jobs) = jobs;
    done(&headers, "/jobs")
}

#[derive(Deserialize)]
pub struct JobFormData {
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub consume_path: String,
    pub resolution: u32,
    pub jpeg_quality: u8,
    pub pixel_format: String,
    #[serde(default = "default_source")]
    pub source: String,
}

#[derive(Deserialize)]
pub struct ReorderJobsData {
    expected_order: Vec<usize>,
    order: Vec<usize>,
}

fn default_source() -> String {
    DEFAULT_SOURCE.to_string()
}

fn done(headers: &HeaderMap, url: &'static str) -> Response {
    if headers.contains_key("hx-request") {
        (StatusCode::NO_CONTENT, [("HX-Redirect", url)]).into_response()
    } else {
        Redirect::to(url).into_response()
    }
}

pub async fn jobs_list(UiState(state): UiState) -> Response {
    let jobs = lock!(state.jobs);
    render(JobsListTpl {
        jobs: job_rows(&jobs),
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn jobs_reorder(
    UiState(state): UiState,
    Json(request): Json<ReorderJobsData>,
) -> StatusCode {
    let _change = lock!(state.config_change_lock);
    let snapshot = lock!(state.jobs).clone();
    let Some(current_ids) = snapshot
        .iter()
        .map(|job| {
            job.job_info["job_id"]
                .as_u64()
                .and_then(|id| usize::try_from(id).ok())
        })
        .collect::<Option<Vec<_>>>()
    else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    };
    let current_set: HashSet<_> = current_ids.iter().copied().collect();
    if current_set.len() != current_ids.len() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    if request.expected_order != current_ids {
        return StatusCode::CONFLICT;
    }
    let requested_set: HashSet<_> = request.order.iter().copied().collect();
    if request.order.len() != current_ids.len() || requested_set != current_set {
        return StatusCode::BAD_REQUEST;
    }
    if request.order == current_ids {
        return StatusCode::NO_CONTENT;
    }

    let mut by_id: HashMap<_, _> = current_ids.into_iter().zip(snapshot).collect();
    let mut reordered = Vec::with_capacity(request.order.len());
    for id in request.order {
        let Some(job) = by_id.remove(&id) else {
            return StatusCode::INTERNAL_SERVER_ERROR;
        };
        reordered.push(job);
    }
    let retention = lock!(state.retention).clone();
    if let Err(error) = state.persist_config(&reordered, &retention, *lock!(state.next_job_id)) {
        tracing::error!(
            event = "config.save_failed",
            error_class = crate::logging::error_class(&error),
            "failed to save reordered jobs"
        );
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *lock!(state.jobs) = reordered;
    StatusCode::NO_CONTENT
}

pub async fn jobs_new(UiState(state): UiState) -> Response {
    render(JobsFormTpl {
        editing: false,
        job_id: 0,
        name: String::new(),
        color: DEFAULT_COLOR.to_string(),
        consume_path: String::new(),
        resolution: DEFAULT_RESOLUTION,
        jpeg_quality: DEFAULT_JPEG_QUALITY,
        pixel_format: DEFAULT_PIXEL_FORMAT.to_string(),
        source: DEFAULT_SOURCE.to_string(),
        error: None,
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn jobs_create(
    headers: HeaderMap,
    UiState(state): UiState,
    Form(form): Form<JobFormData>,
) -> Response {
    let new_job = match apply_job_form(&form, &state.default_output_path, state.translations) {
        Ok(j) => j,
        Err(e) => {
            return render(form_to_tpl(
                false,
                0,
                &form,
                Some(e),
                state.translations,
                state.build_info,
            ));
        }
    };
    let _change = lock!(state.config_change_lock);
    let mut snapshot = lock!(state.jobs).clone();
    if snapshot.iter().any(|job| job.name() == new_job.name()) {
        return StatusCode::CONFLICT.into_response();
    }
    let id = *lock!(state.next_job_id);
    let Some(next_id) = id.checked_add(1) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    {
        let mut info = new_job.job_info.clone();
        info["job_id"] = serde_json::json!(id);
        snapshot.push(Job {
            job_info: info,
            ..new_job
        });
    }
    let retention = lock!(state.retention).clone();
    if let Err(e) = state.persist_config(&snapshot, &retention, next_id) {
        tracing::error!(
            event = "config.save_failed",
            error_class = crate::logging::error_class(&e),
            "failed to save config"
        );
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    *lock!(state.jobs) = snapshot;
    *lock!(state.next_job_id) = next_id;
    done(&headers, "/jobs")
}

pub async fn jobs_edit(Path(id): Path<usize>, UiState(state): UiState) -> Response {
    let jobs = lock!(state.jobs);
    let Some(job) = jobs
        .iter()
        .find(|j| j.job_info["job_id"].as_u64() == Some(id as u64))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    render(JobsFormTpl {
        editing: true,
        job_id: id,
        name: job.name().to_string(),
        color: job.color().to_string(),
        consume_path: job
            .consume_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        resolution: job.resolution(),
        jpeg_quality: job.jpeg_quality(),
        pixel_format: job.pixel_format().to_string(),
        source: job.source().to_string(),
        error: None,
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn jobs_update(
    Path(id): Path<usize>,
    headers: HeaderMap,
    UiState(state): UiState,
    Form(form): Form<JobFormData>,
) -> Response {
    let _change = lock!(state.config_change_lock);
    let mut snapshot = lock!(state.jobs).clone();
    let Some(index) = snapshot
        .iter()
        .position(|j| j.job_info["job_id"].as_u64() == Some(id as u64))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let updated = match apply_job_form(&form, &snapshot[index].output_path, state.translations) {
        Ok(parsed) => merge_editable_job(&snapshot[index], parsed),
        Err(e) => {
            return render(form_to_tpl(
                true,
                id,
                &form,
                Some(e),
                state.translations,
                state.build_info,
            ));
        }
    };
    if snapshot
        .iter()
        .enumerate()
        .any(|(i, job)| i != index && job.name() == updated.name())
    {
        return StatusCode::CONFLICT.into_response();
    }
    if updated.output_path != snapshot[index].output_path
        || updated.consume_path != snapshot[index].consume_path
    {
        match job_has_pending_work(&snapshot[index]) {
            Ok(true) => return StatusCode::CONFLICT.into_response(),
            Ok(false) => {}
            Err(e) => {
                tracing::error!(
                    event = "jobs.pending_check_failed",
                    error_class = crate::logging::error_class(&e),
                    "cannot check pending batches before moving job"
                );
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }
    {
        let mut info = updated.job_info.clone();
        info["job_id"] = serde_json::json!(id);
        snapshot[index] = Job {
            job_info: info,
            ..updated
        };
    }
    let retention = lock!(state.retention).clone();
    if let Err(e) = state.persist_config(&snapshot, &retention, *lock!(state.next_job_id)) {
        tracing::error!(
            event = "config.save_failed",
            error_class = crate::logging::error_class(&e),
            "failed to save config"
        );
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    *lock!(state.jobs) = snapshot;
    done(&headers, "/jobs")
}

pub async fn jobs_delete(
    Path(id): Path<usize>,
    headers: HeaderMap,
    UiState(state): UiState,
) -> Response {
    let _change = lock!(state.config_change_lock);
    let mut snapshot = lock!(state.jobs).clone();
    let Some(index) = snapshot
        .iter()
        .position(|j| j.job_info["job_id"].as_u64() == Some(id as u64))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match job_has_pending_work(&snapshot[index]) {
        Ok(true) => return StatusCode::CONFLICT.into_response(),
        Ok(false) => {}
        Err(e) => {
            tracing::error!(
                event = "jobs.pending_check_failed",
                error_class = crate::logging::error_class(&e),
                "cannot check pending batches before deleting job"
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    snapshot.remove(index);
    let retention = lock!(state.retention).clone();
    if let Err(e) = state.persist_config(&snapshot, &retention, *lock!(state.next_job_id)) {
        tracing::error!(
            event = "config.save_failed",
            error_class = crate::logging::error_class(&e),
            "failed to save config"
        );
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    *lock!(state.jobs) = snapshot;
    done(&headers, "/jobs")
}

fn job_has_pending_work(job: &Job) -> anyhow::Result<bool> {
    for entry in std::fs::read_dir(&job.output_path)? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().to_string();
        if crate::batch::Batch::has_pending_work(job, &id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn merge_editable_job(existing: &Job, parsed: Job) -> Job {
    let mut updated = existing.clone();
    updated.job_info["name"] = serde_json::json!(parsed.name());
    updated.job_info["color"] = serde_json::json!(parsed.color());
    updated.consume_path = parsed.consume_path.clone();

    let sources = &mut updated.scan_settings["parameters"]["task"]["actions"]["streams"]["sources"];
    let Some(attrs) = sources[KEY_PIXEL_FORMATS]["attributes"].as_array_mut() else {
        updated.scan_settings = parsed.scan_settings;
        return updated;
    };
    for (key, value) in [
        (KEY_RESOLUTION, parsed.resolution().to_string()),
        (KEY_JPEG_QUALITY, parsed.jpeg_quality().to_string()),
    ] {
        let Some(attribute) = attrs.iter_mut().find(|attr| attr["attribute"] == key) else {
            updated.scan_settings = parsed.scan_settings;
            return updated;
        };
        attribute["values"]["value"] = serde_json::Value::String(value);
    }
    sources[KEY_SOURCE] = serde_json::json!(parsed.source());
    sources[KEY_PIXEL_FORMATS][KEY_PIXEL_FORMAT] = serde_json::json!(parsed.pixel_format());
    updated
}

fn apply_job_form(
    form: &JobFormData,
    output_path: &FsPath,
    t: &'static Translations,
) -> Result<Job, String> {
    if form.name.trim().is_empty() {
        return Err(t.err_name_empty.to_string());
    }
    if form.name.trim().len() > MAX_JOB_NAME_LEN {
        return Err(t
            .err_name_too_long
            .replace("{0}", &MAX_JOB_NAME_LEN.to_string()));
    }
    if output_path.to_string_lossy().len() > MAX_PATH_LEN {
        return Err(t
            .err_path_too_long
            .replace("{0}", &MAX_PATH_LEN.to_string()));
    }
    crate::config::validate_hex_color(form.color.trim()).map_err(|e| e.to_string())?;
    let yaml = job_form_to_yaml(form, output_path);
    let parsed = crate::config::Config::parse(&yaml).map_err(|e| e.to_string())?;
    parsed
        .jobs
        .into_iter()
        .next()
        .ok_or_else(|| t.err_invalid_config.to_string())
}

fn job_form_to_yaml(form: &JobFormData, output_path: &FsPath) -> String {
    use crate::config::{RawConfig, RawJob};
    use indexmap::IndexMap;
    use std::collections::HashMap;

    let mut scan_settings = HashMap::new();
    scan_settings.insert(
        KEY_SOURCE.to_string(),
        serde_json::json!(form.source.trim()),
    );
    let mut pf = HashMap::new();
    pf.insert(
        KEY_RESOLUTION.to_string(),
        serde_json::json!(form.resolution),
    );
    pf.insert(
        KEY_JPEG_QUALITY.to_string(),
        serde_json::json!(form.jpeg_quality),
    );
    pf.insert(
        KEY_PIXEL_FORMAT.to_string(),
        serde_json::json!(form.pixel_format),
    );
    scan_settings.insert(KEY_PIXEL_FORMATS.to_string(), serde_json::json!(pf));

    let raw_job = RawJob {
        output_path: output_path.to_string_lossy().to_string(),
        job_id: None,
        consume_path: if form.consume_path.trim().is_empty() {
            None
        } else {
            Some(form.consume_path.trim().to_string())
        },
        color: Some(form.color.clone()),
        job_settings: None,
        scan_settings: Some(scan_settings),
    };
    let mut jobs = IndexMap::new();
    jobs.insert(form.name.trim().to_string(), raw_job);
    serde_yaml::to_string(&RawConfig {
        jobs,
        scanner_auth: None,
        paperless: None,
        next_job_id: None,
        retention: Default::default(),
        lang: Default::default(),
    })
    .unwrap_or_default()
}

fn form_to_tpl(
    editing: bool,
    job_id: usize,
    form: &JobFormData,
    error: Option<String>,
    t: &'static Translations,
    build: &'static BuildInfo,
) -> JobsFormTpl {
    JobsFormTpl {
        editing,
        job_id,
        error,
        name: form.name.clone(),
        color: form.color.clone(),
        consume_path: form.consume_path.clone(),
        resolution: form.resolution,
        jpeg_quality: form.jpeg_quality,
        pixel_format: form.pixel_format.clone(),
        source: form.source.clone(),
        t,
        build,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::config::{Config, Job};

    #[test]
    fn expert_edit_validates_json_and_preserves_internal_storage_path() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = format!(
            "jobs:\n  Test:\n    output_path: {}\n",
            tmp.path().display()
        );
        let job = Config::parse(&yaml).unwrap().jobs.remove(0);
        let mut raw = job.to_raw().1;
        raw.job_settings
            .get_or_insert_default()
            .insert("duplex".into(), serde_json::json!(true));
        let form = super::ExpertFormData {
            job_settings_json: serde_json::to_string(raw.job_settings.as_ref().unwrap()).unwrap(),
            scan_settings_json: serde_json::to_string(raw.scan_settings.as_ref().unwrap()).unwrap(),
        };
        let updated = super::parse_expert_job(&job, &form).unwrap();
        assert_eq!(updated.output_path, job.output_path);
        assert_eq!(updated.to_raw().1.job_settings.unwrap()["duplex"], true);
        let invalid = super::ExpertFormData {
            scan_settings_json: "{".into(),
            ..form
        };
        assert!(super::parse_expert_job(&job, &invalid).is_err());
        assert!(!job.to_raw().1.job_settings.unwrap().contains_key("duplex"));
    }
    use crate::lock;
    use crate::routes::test_support::TestApp;
    use crate::state::AppState;

    fn test_server() -> (TestApp, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({
                    "name": "TestJob", "job_id": 0, "color": "#4D4D4D",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: json!({}),
            }],
            ..Default::default()
        };
        (TestApp::new(AppState::new(config)), tmp)
    }

    fn reorder_server() -> (TestApp, AppState, tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.yaml");
        let jobs = [(7, "First"), (2, "Second"), (11, "Third")]
            .into_iter()
            .map(|(id, name)| Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({"name": name, "job_id": id, "color": "#4D4D4D"}),
                scan_settings: json!({}),
            })
            .collect();
        let state = AppState::new(Config {
            jobs,
            next_job_id: 12,
            lang: "en".into(),
            retention: crate::config::RetentionConfig {
                archive_after_days: 3,
                delete_after_days: 9,
            },
            ..Default::default()
        })
        .with_config_path(config_path.clone());
        (TestApp::new(state.clone()), state, tmp, config_path)
    }

    #[tokio::test]
    async fn reorder_persists_and_changes_scanner_job_order_without_changing_ids() {
        let (server, state, _tmp, config_path) = reorder_server();
        let response = server
            .post("/jobs/reorder")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .json(&json!({"expected_order": [7, 2, 11], "order": [11, 7, 2]}))
            .await;
        assert_eq!(response.status_code(), 204);
        let ids: Vec<_> = lock!(state.jobs)
            .iter()
            .map(|job| job.job_info["job_id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, [11, 7, 2]);
        let reloaded = Config::load(&config_path).unwrap();
        assert_eq!(
            reloaded.jobs.iter().map(Job::name).collect::<Vec<_>>(),
            ["Third", "First", "Second"]
        );
        assert_eq!(reloaded.next_job_id, 12);
        assert_eq!(reloaded.lang, "en");
        assert_eq!(reloaded.retention.archive_after_days, 3);
        let scanner = server
            .post("/NmWebService/authorization")
            .json(&json!({}))
            .await
            .json::<serde_json::Value>();
        let names: Vec<_> = scanner["job_info"]
            .as_array()
            .unwrap()
            .iter()
            .map(|job| job["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Third", "First", "Second"]);
        assert_eq!(
            server
                .post("/jobs/reorder")
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .add_header("HX-Request", "true")
                .json(&json!({"expected_order": [7, 2, 11], "order": [2, 11, 7]}))
                .await
                .status_code(),
            409,
            "a stale browser must not overwrite the new order"
        );
    }

    #[tokio::test]
    async fn reorder_rejects_stale_or_invalid_orders_without_changing_jobs() {
        let (server, state, _tmp, config_path) = reorder_server();
        for (body, status) in [
            (
                json!({"expected_order": [7, 2, 11], "order": [7, 7, 11]}),
                400,
            ),
            (
                json!({"expected_order": [7, 2, 11], "order": [7, 2, 99]}),
                400,
            ),
            (json!({"expected_order": [7, 2, 11], "order": [7, 2]}), 400),
            (
                json!({"expected_order": [2, 7, 11], "order": [11, 7, 2]}),
                409,
            ),
        ] {
            assert_eq!(
                server
                    .post("/jobs/reorder")
                    .add_header("origin", "https://localhost")
                    .add_header("x-forwarded-proto", "https")
                    .add_header("HX-Request", "true")
                    .json(&body)
                    .await
                    .status_code(),
                status
            );
        }
        assert_eq!(
            lock!(state.jobs).iter().map(Job::name).collect::<Vec<_>>(),
            ["First", "Second", "Third"]
        );
        assert!(!config_path.exists());
        assert_eq!(
            server
                .post("/jobs/reorder")
                .add_header("origin", "https://attacker.invalid")
                .add_header("x-forwarded-proto", "https")
                .json(&json!({"expected_order": [7, 2, 11], "order": [11, 7, 2]}))
                .await
                .status_code(),
            403
        );
    }

    #[tokio::test]
    async fn reorder_failed_persistence_keeps_live_order() {
        let (server, state, tmp, _config_path) = reorder_server();
        let bad_path = tmp.path().join("missing").join("config.yaml");
        let broken_state = AppState::new(Config {
            jobs: lock!(state.jobs).clone(),
            ..Default::default()
        })
        .with_config_path(bad_path);
        let broken_server = TestApp::new(broken_state.clone());
        drop(server);
        assert_eq!(
            broken_server
                .post("/jobs/reorder")
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .add_header("HX-Request", "true")
                .json(&json!({"expected_order": [7, 2, 11], "order": [11, 7, 2]}))
                .await
                .status_code(),
            500
        );
        assert_eq!(
            lock!(broken_state.jobs)
                .iter()
                .map(Job::name)
                .collect::<Vec<_>>(),
            ["First", "Second", "Third"]
        );
    }

    #[tokio::test]
    async fn test_jobs_list_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/jobs").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_jobs_list_shows_job_name() {
        let (server, _tmp) = test_server();
        assert!(server.get("/jobs").await.text().contains("TestJob"));
    }

    #[tokio::test]
    async fn test_jobs_new_form_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/jobs/new").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_jobs_edit_form_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/jobs/0/edit").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_jobs_edit_form_prefills_name() {
        let (server, _tmp) = test_server();
        assert!(server.get("/jobs/0/edit").await.text().contains("TestJob"));
    }

    #[tokio::test]
    async fn job_forms_and_list_do_not_expose_storage_paths() {
        let (server, tmp) = test_server();
        for route in ["/jobs/new", "/jobs/0/edit", "/jobs"] {
            let html = server.get(route).await.text();
            assert!(!html.contains("name=\"output_path\""), "{route}");
            assert!(!html.contains("Ausgabepfad"), "{route}");
            assert!(
                !html.contains(&tmp.path().to_string_lossy().to_string()),
                "{route}"
            );
        }
    }

    #[tokio::test]
    async fn new_job_uses_server_storage_path_without_form_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state =
            AppState::new(Config::default()).with_default_output_path(tmp.path().to_path_buf());
        let server = TestApp::new(state.clone());
        let response = server
            .post("/jobs")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "New"),
                ("color", "#4D4D4D"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
            ])
            .await;
        assert_eq!(response.status_code(), 204);
        assert_eq!(lock!(state.jobs)[0].output_path, tmp.path());
    }

    #[tokio::test]
    async fn submitted_output_path_cannot_move_existing_job() {
        let (server, tmp) = test_server();
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let response = server
            .post("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "TestJob"),
                ("output_path", other.to_str().unwrap()),
                ("color", "#4D4D4D"),
                ("resolution", "300"),
                ("jpeg_quality", "90"),
                ("pixel_format", "rgb24"),
            ])
            .await;
        assert_eq!(response.status_code(), 204);
        let html = server.get("/jobs/0/edit").await.text();
        assert!(html.contains("value=\"90\""));
        assert!(!html.contains(other.to_str().unwrap()));
    }

    #[tokio::test]
    async fn editing_job_keeps_advanced_scanner_settings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let yaml = format!(
            "jobs:\n  Advanced:\n    output_path: {}\n    job_settings:\n      continuous_scan: true\n      job_timeout: 42\n    scan_settings:\n      pixelFormats:\n        automaticDeskew: false\n        resolution: 600\n        jpegQuality: 95\n      source: feederFront\n",
            tmp.path().display()
        );
        let state = AppState::new(Config::parse(&yaml).unwrap());
        let server = TestApp::new(state.clone());
        let response = server
            .post("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "Advanced"),
                ("color", "#123456"),
                ("resolution", "300"),
                ("jpeg_quality", "90"),
                ("pixel_format", "rgb24"),
                ("source", "feeder"),
            ])
            .await;
        assert_eq!(response.status_code(), 204);
        let jobs = lock!(state.jobs);
        let job = &jobs[0];
        assert_eq!(job.job_info["job_setting"]["continuous_scan"], true);
        assert_eq!(job.job_info["job_setting"]["job_timeout"], 42);
        assert_eq!(job.resolution(), 300);
        assert_eq!(job.jpeg_quality(), 90);
        assert_eq!(job.source(), "feeder");
        let attrs = job.scan_settings["parameters"]["task"]["actions"]["streams"]["sources"]["pixelFormats"]["attributes"].as_array().unwrap();
        let deskew = attrs
            .iter()
            .find(|a| a["attribute"] == "automaticDeskew")
            .unwrap();
        assert_eq!(deskew["values"]["value"], "false");
    }

    #[tokio::test]
    async fn test_jobs_edit_unknown_returns_404() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/jobs/99/edit").await.status_code(), 404);
    }

    #[tokio::test]
    async fn test_jobs_update_htmx_returns_hx_redirect_not_303() {
        // htmx follows 303 and swaps the full page HTML into the <form> → double navigation.
        // With HX-Request present the server must return HX-Redirect so htmx does a real navigation.
        let (server, tmp) = test_server();
        let resp = server
            .post("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "UpdatedJob"),
                ("output_path", tmp.path().to_str().unwrap()),
                ("color", "#0000ff"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        assert_eq!(resp.status_code(), 204);
        assert_eq!(
            resp.headers()
                .get("HX-Redirect")
                .and_then(|v| v.to_str().ok()),
            Some("/jobs")
        );
    }

    #[tokio::test]
    async fn test_jobs_create_redirects() {
        let (server, tmp) = test_server();
        let out = tmp.path().join("new");
        std::fs::create_dir_all(&out).unwrap();
        let resp = server
            .post("/jobs")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "NewJob"),
                ("output_path", out.to_str().unwrap()),
                ("color", "#ff0000"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        assert_eq!(resp.status_code(), 204);
        assert_eq!(
            resp.headers()
                .get("HX-Redirect")
                .and_then(|v| v.to_str().ok()),
            Some("/jobs")
        );
    }

    #[tokio::test]
    async fn test_jobs_create_appears_in_list() {
        let (server, tmp) = test_server();
        let out = tmp.path().join("new2");
        std::fs::create_dir_all(&out).unwrap();
        server
            .post("/jobs")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "CreatedJob"),
                ("output_path", out.to_str().unwrap()),
                ("color", "#00ff00"),
                ("resolution", "600"),
                ("jpeg_quality", "90"),
                ("pixel_format", "gray8"),
                ("consume_path", ""),
            ])
            .await;
        assert!(server.get("/jobs").await.text().contains("CreatedJob"));
    }

    #[tokio::test]
    async fn test_jobs_update_redirects() {
        let (server, tmp) = test_server();
        let resp = server
            .post("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "UpdatedJob"),
                ("output_path", tmp.path().to_str().unwrap()),
                ("color", "#0000ff"),
                ("resolution", "600"),
                ("jpeg_quality", "95"),
                ("pixel_format", "gray8"),
                ("consume_path", ""),
            ])
            .await;
        assert_eq!(resp.status_code(), 204);
        assert_eq!(
            resp.headers()
                .get("HX-Redirect")
                .and_then(|v| v.to_str().ok()),
            Some("/jobs")
        );
    }

    #[tokio::test]
    async fn test_jobs_update_reflected_in_list() {
        let (server, tmp) = test_server();
        server
            .post("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "RenamedJob"),
                ("output_path", tmp.path().to_str().unwrap()),
                ("color", "#0000ff"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        assert!(server.get("/jobs").await.text().contains("RenamedJob"));
    }

    #[tokio::test]
    async fn test_jobs_delete_redirects() {
        let (server, _tmp) = test_server();
        let resp = server
            .delete("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .await;
        assert_eq!(resp.status_code(), 204);
        assert_eq!(
            resp.headers()
                .get("HX-Redirect")
                .and_then(|v| v.to_str().ok()),
            Some("/jobs")
        );
    }

    #[tokio::test]
    async fn test_jobs_delete_removes_from_list() {
        let (server, _tmp) = test_server();
        server
            .delete("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .await;
        assert!(!server.get("/jobs").await.text().contains("TestJob"));
    }

    /// Jobs created/updated/deleted must be written to the config file on disk
    /// so they survive a container restart. This requires config_path to be set
    /// (with_config_path) and the file to be writable (not :ro mounted).
    fn test_server_persistent() -> (TestApp, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_file = tmp.path().join("config.yaml");
        std::fs::write(&config_file, "jobs: {}\n").unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({
                    "name": "TestJob", "job_id": 0, "color": "#4D4D4D",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: json!({}),
            }],
            ..Default::default()
        };
        let state = crate::state::AppState::new(config).with_config_path(config_file);
        (TestApp::new(state), tmp)
    }

    #[tokio::test]
    async fn test_jobs_create_persists_to_config_file() {
        let (server, tmp) = test_server_persistent();
        let out = tmp.path().join("newjob");
        std::fs::create_dir_all(&out).unwrap();
        server
            .post("/jobs")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "PersistedJob"),
                ("output_path", out.to_str().unwrap()),
                ("color", "#ff0000"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        let on_disk = std::fs::read_to_string(tmp.path().join("config.yaml")).unwrap();
        assert!(
            on_disk.contains("PersistedJob"),
            "created job must be written to config file; got: {on_disk}"
        );
    }

    #[tokio::test]
    async fn test_jobs_create_reports_persistence_error_without_publishing_job() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = AppState::new(Config::default())
            .with_config_path(tmp.path().join("missing/config.yaml"));
        let server = TestApp::new(state.clone());
        let resp = server
            .post("/jobs")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "New"),
                ("output_path", tmp.path().to_str().unwrap()),
                ("color", "#4D4D4D"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        assert_eq!(resp.status_code(), 500);
        assert!(lock!(state.jobs).is_empty());
    }

    #[tokio::test]
    async fn test_deleting_job_keeps_remaining_scanner_id_stable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let make_job = |name: &str, id: usize| Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name": name, "job_id": id}),
            scan_settings: json!({}),
        };
        let state = AppState::new(Config {
            jobs: vec![make_job("A", 0), make_job("B", 1)],
            ..Default::default()
        });
        let server = TestApp::new(state);
        assert_eq!(
            server
                .delete("/jobs/0")
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .add_header("HX-Request", "true")
                .await
                .status_code(),
            204
        );
        let jobs = server
            .post("/NmWebService/authorization")
            .await
            .json::<serde_json::Value>();
        assert_eq!(jobs["job_info"][0]["name"], "B");
        assert_eq!(jobs["job_info"][0]["job_id"], 1);
    }

    #[tokio::test]
    async fn test_cannot_delete_job_with_pending_paperless_batch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let job = Job {
            output_path: tmp.path().to_path_buf(),
            consume_path: None,
            job_info: json!({"name":"Scan", "job_id":0}),
            scan_settings: json!({}),
        };
        let mut batch =
            crate::batch::Batch::create(&job, crate::batch::ScannerInfo::default()).unwrap();
        batch.enable_paperless().unwrap();
        batch
            .add_file(
                "page.jpg",
                include_bytes!("../../tests/fixtures/scan_page.jpg"),
                json!({}),
            )
            .unwrap();
        batch.complete().unwrap();
        let server = TestApp::new(AppState::new(Config {
            jobs: vec![job],
            ..Default::default()
        }));
        assert_eq!(
            server
                .delete("/jobs/0")
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .add_header("HX-Request", "true")
                .await
                .status_code(),
            409
        );
    }

    #[tokio::test]
    async fn test_output_path_in_submission_cannot_move_open_batch() {
        let (server, tmp) = test_server();
        let response = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id":0}))
            .await;
        assert_eq!(response.status_code(), 200);
        let new_path = tmp.path().join("other");
        std::fs::create_dir(&new_path).unwrap();
        let resp = server
            .post("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "TestJob"),
                ("output_path", new_path.to_str().unwrap()),
                ("color", "#4D4D4D"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        assert_eq!(resp.status_code(), 204);
        assert_eq!(
            server.get("/scans").await.status_code(),
            200,
            "existing scan remains accessible"
        );
    }

    #[tokio::test]
    async fn test_jobs_update_persists_to_config_file() {
        let (server, tmp) = test_server_persistent();
        server
            .post("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "RenamedPersisted"),
                ("output_path", tmp.path().to_str().unwrap()),
                ("color", "#0000ff"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        let on_disk = std::fs::read_to_string(tmp.path().join("config.yaml")).unwrap();
        assert!(
            on_disk.contains("RenamedPersisted"),
            "updated job must be written to config file; got: {on_disk}"
        );
    }

    #[tokio::test]
    async fn test_jobs_delete_persists_to_config_file() {
        let (server, tmp) = test_server_persistent();
        server
            .delete("/jobs/0")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .await;
        let on_disk = std::fs::read_to_string(tmp.path().join("config.yaml")).unwrap();
        assert!(
            !on_disk.contains("TestJob"),
            "deleted job must be removed from config file; got: {on_disk}"
        );
    }
}
