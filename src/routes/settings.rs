use askama::Template;
use axum::{Form, extract::State, response::Response};
use serde::Deserialize;

use super::ui::render;
use crate::build_info::BuildInfo;
use crate::config::RetentionConfig;
use crate::lock;
use crate::state::AppState;
use crate::translations::Translations;

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTpl {
    archive_after_days: u32,
    delete_after_days: u32,
    saved: bool,
    error: Option<String>,
    t: &'static Translations,
    build: &'static BuildInfo,
}

#[derive(Deserialize)]
pub struct SettingsFormData {
    pub archive_after_days: u32,
    pub delete_after_days: u32,
}

pub async fn settings_get(State(state): State<AppState>) -> Response {
    let cfg = lock!(state.retention).clone();
    render(SettingsTpl {
        archive_after_days: cfg.archive_after_days,
        delete_after_days: cfg.delete_after_days,
        saved: false,
        error: None,
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn settings_post(
    State(state): State<AppState>,
    Form(form): Form<SettingsFormData>,
) -> Response {
    if form.archive_after_days > 0
        && form.delete_after_days > 0
        && form.delete_after_days <= form.archive_after_days
    {
        return render(SettingsTpl {
            archive_after_days: form.archive_after_days,
            delete_after_days: form.delete_after_days,
            saved: false,
            error: Some(state.translations.err_delete_gt_archive.to_string()),
            t: state.translations,
            build: state.build_info,
        });
    }
    let new_cfg = RetentionConfig {
        archive_after_days: form.archive_after_days,
        delete_after_days: form.delete_after_days,
    };
    *lock!(state.retention) = new_cfg.clone();
    let jobs = lock!(state.jobs).clone();
    state.persist_config(&jobs);
    render(SettingsTpl {
        archive_after_days: new_cfg.archive_after_days,
        delete_after_days: new_cfg.delete_after_days,
        saved: true,
        error: None,
        t: state.translations,
        build: state.build_info,
    })
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;

    use crate::config::{Config, Job};
    use crate::routes::router;
    use crate::state::AppState;

    fn test_server() -> (TestServer, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: serde_json::json!({
                    "name": "TestJob", "job_id": 0, "color": "#4D4D4D",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: serde_json::json!({}),
            }],
            ..Default::default()
        };
        (TestServer::new(router(AppState::new(config))), tmp)
    }

    #[tokio::test]
    async fn test_settings_get_returns_200() {
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/settings").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_settings_post_saves_and_shows_confirmation() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/settings")
            .add_header("HX-Request", "true")
            .form(&[("archive_after_days", "7"), ("delete_after_days", "30")])
            .await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.text().contains("Gespeichert"));
    }

    #[tokio::test]
    async fn test_settings_post_rejects_invalid_thresholds() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/settings")
            .add_header("HX-Request", "true")
            .form(&[("archive_after_days", "30"), ("delete_after_days", "7")])
            .await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.text().contains("Löschfrist"));
    }

    #[tokio::test]
    async fn test_settings_post_both_zero_is_valid() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/settings")
            .add_header("HX-Request", "true")
            .form(&[("archive_after_days", "0"), ("delete_after_days", "0")])
            .await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.text().contains("Gespeichert"));
    }

    #[tokio::test]
    async fn test_settings_post_archive_only_no_delete_is_valid() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/settings")
            .add_header("HX-Request", "true")
            .form(&[("archive_after_days", "7"), ("delete_after_days", "0")])
            .await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.text().contains("Gespeichert"));
    }
}
