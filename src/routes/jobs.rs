use askama::Template;
use axum::{
    Form,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use super::ui::{JobRow, job_rows, render};
use crate::build_info::BuildInfo;
use crate::config::{
    DEFAULT_COLOR, DEFAULT_JPEG_QUALITY, DEFAULT_PIXEL_FORMAT, DEFAULT_RESOLUTION, DEFAULT_SOURCE,
    Job, KEY_JPEG_QUALITY, KEY_PIXEL_FORMAT, KEY_PIXEL_FORMATS, KEY_RESOLUTION, KEY_SOURCE,
    MAX_JOB_NAME_LEN, MAX_PATH_LEN,
};
use crate::lock;
use crate::state::AppState;
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
    output_path: String,
    consume_path: String,
    resolution: u32,
    jpeg_quality: u8,
    pixel_format: String,
    source: String,
    error: Option<String>,
    t: &'static Translations,
    build: &'static BuildInfo,
}

#[derive(Deserialize)]
pub struct JobFormData {
    pub name: String,
    pub color: String,
    pub output_path: String,
    #[serde(default)]
    pub consume_path: String,
    pub resolution: u32,
    pub jpeg_quality: u8,
    pub pixel_format: String,
    #[serde(default = "default_source")]
    pub source: String,
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

pub async fn jobs_list(State(state): State<AppState>) -> Response {
    let jobs = lock!(state.jobs);
    render(JobsListTpl {
        jobs: job_rows(&jobs),
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn jobs_new(State(state): State<AppState>) -> Response {
    render(JobsFormTpl {
        editing: false,
        job_id: 0,
        name: String::new(),
        color: DEFAULT_COLOR.to_string(),
        output_path: String::new(),
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
    State(state): State<AppState>,
    Form(form): Form<JobFormData>,
) -> Response {
    let new_job = match apply_job_form(&form, state.translations) {
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
    let snapshot = {
        let mut jobs = lock!(state.jobs);
        let id = jobs.len();
        let mut info = new_job.job_info.clone();
        info["job_id"] = serde_json::json!(id);
        jobs.push(Job {
            job_info: info,
            ..new_job
        });
        jobs.clone()
    }; // lock released before disk I/O
    state.persist_config(&snapshot);
    done(&headers, "/jobs")
}

pub async fn jobs_edit(Path(id): Path<usize>, State(state): State<AppState>) -> Response {
    let jobs = lock!(state.jobs);
    let Some(job) = jobs.get(id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    render(JobsFormTpl {
        editing: true,
        job_id: id,
        name: job.name().to_string(),
        color: job.color().to_string(),
        output_path: job.output_path.to_string_lossy().to_string(),
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
    State(state): State<AppState>,
    Form(form): Form<JobFormData>,
) -> Response {
    let updated = match apply_job_form(&form, state.translations) {
        Ok(j) => j,
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
    let snapshot = {
        let mut jobs = lock!(state.jobs);
        if id >= jobs.len() {
            return StatusCode::NOT_FOUND.into_response();
        }
        let mut info = updated.job_info.clone();
        info["job_id"] = serde_json::json!(id);
        jobs[id] = Job {
            job_info: info,
            ..updated
        };
        jobs.clone()
    }; // lock released before disk I/O
    state.persist_config(&snapshot);
    done(&headers, "/jobs")
}

pub async fn jobs_delete(
    Path(id): Path<usize>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let snapshot = {
        let mut jobs = lock!(state.jobs);
        if id >= jobs.len() {
            return StatusCode::NOT_FOUND.into_response();
        }
        jobs.remove(id);
        for (i, job) in jobs.iter_mut().enumerate() {
            job.job_info["job_id"] = serde_json::json!(i);
        }
        jobs.clone()
    }; // lock released before disk I/O
    state.persist_config(&snapshot);
    done(&headers, "/jobs")
}

fn apply_job_form(form: &JobFormData, t: &'static Translations) -> Result<Job, String> {
    if form.name.trim().is_empty() {
        return Err(t.err_name_empty.to_string());
    }
    if form.name.trim().len() > MAX_JOB_NAME_LEN {
        return Err(t
            .err_name_too_long
            .replace("{0}", &MAX_JOB_NAME_LEN.to_string()));
    }
    if form.output_path.trim().len() > MAX_PATH_LEN {
        return Err(t
            .err_path_too_long
            .replace("{0}", &MAX_PATH_LEN.to_string()));
    }
    crate::config::validate_hex_color(form.color.trim()).map_err(|e| e.to_string())?;
    let yaml = job_form_to_yaml(form);
    let parsed = crate::config::Config::parse(&yaml).map_err(|e| e.to_string())?;
    parsed
        .jobs
        .into_iter()
        .next()
        .ok_or_else(|| t.err_invalid_config.to_string())
}

fn job_form_to_yaml(form: &JobFormData) -> String {
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
        output_path: form.output_path.trim().to_string(),
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
        output_path: form.output_path.clone(),
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
    use axum_test::TestServer;
    use serde_json::json;

    use crate::config::{Config, Job};
    use crate::routes::router;
    use crate::state::AppState;

    fn test_server() -> (TestServer, tempfile::TempDir) {
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
        (TestServer::new(router(AppState::new(config))), tmp)
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
            .add_header("HX-Request", "true")
            .await;
        assert!(!server.get("/jobs").await.text().contains("TestJob"));
    }
}
