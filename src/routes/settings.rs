use askama::Template;
use axum::{
    Form,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::UiState;
use super::ui::render;
use crate::build_info::BuildInfo;
use crate::config::{PaperlessConfig, RetentionConfig, ScannerAuthConfig};
use crate::lock;
use crate::state::AppState;
use crate::translations::Translations;

#[derive(Template)]
#[template(path = "scanner_security.html")]
struct ScannerSecurityTpl {
    mode: String,
    username: String,
    has_password: bool,
    error: bool,
    saved: bool,
    lang: &'static str,
}

#[derive(Template)]
#[template(path = "paperless_settings.html")]
struct PaperlessSettingsTpl {
    enabled: bool,
    url: String,
    tags: String,
    field_id: String,
    token_set: bool,
    saved: bool,
    error: Option<&'static str>,
    paused: bool,
    lang: &'static str,
}

#[derive(Deserialize)]
pub struct PaperlessSettingsForm {
    enabled: String,
    url: String,
    tags: String,
    field_id: String,
    new_token: String,
}

fn paperless_view(
    state: &AppState,
    headers: &HeaderMap,
    saved: bool,
    error: Option<&'static str>,
) -> Response {
    let config = lock!(state.paperless_config).clone();
    let config = config.as_ref();
    render(PaperlessSettingsTpl {
        enabled: config.is_some(),
        url: config.map_or_else(String::new, |cfg| cfg.url.clone()),
        tags: config.map_or_else(String::new, |cfg| {
            cfg.tag_ids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        }),
        field_id: config
            .and_then(|cfg| cfg.batch_id_field_id)
            .map_or_else(String::new, |id| id.to_string()),
        paused: config.is_some() && lock!(state.paperless_client).is_none(),
        token_set: config.is_some_and(|cfg| cfg.token_file.is_some() || !cfg.token_env.is_empty()),
        saved,
        error,
        lang: security_lang(headers),
    })
}

pub async fn paperless_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    paperless_view(&state, &headers, false, None)
}

fn message(headers: &HeaderMap, de: &'static str, en: &'static str) -> &'static str {
    if security_lang(headers) == "de" {
        de
    } else {
        en
    }
}

fn confirmation_expired(headers: &HeaderMap) -> Response {
    (StatusCode::UNAUTHORIZED, axum::response::Html(message(headers,
        "Passkey-Bestätigung fehlt oder ist abgelaufen. Zurück zum Formular und erneut bestätigen.",
        "Passkey confirmation is missing or expired. Return to the form and confirm again."))).into_response()
}

pub async fn paperless_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PaperlessSettingsForm>,
) -> Response {
    if !super::admin::secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.url.len() > 2048
        || form.tags.len() > 512
        || form.field_id.len() > 16
        || form.new_token.len() > 4096
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let authorized =
        super::admin_cookie(&headers).is_some_and(|session| auth.consume_passkey_stepup(session));
    if !authorized {
        return confirmation_expired(&headers);
    }
    if form.enabled == "yes" {
        let valid_url = reqwest::Url::parse(form.url.trim()).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        });
        let valid_ids = form
            .tags
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .all(|v| v.parse::<u32>().is_ok_and(|id| id > 0))
            && form.tags.split(',').count() <= 64
            && (form.field_id.trim().is_empty()
                || form.field_id.trim().parse::<u32>().is_ok_and(|id| id > 0));
        let error = if !valid_url {
            Some(message(
                &headers,
                "Ungültige Paperless-URL: HTTP(S) ohne Zugangsdaten, Query oder Fragment verwenden.",
                "Invalid Paperless URL: use HTTP(S) without credentials, query or fragment.",
            ))
        } else if !valid_ids {
            Some(message(
                &headers,
                "Ungültige Tag- oder Feld-ID: positive ganze Zahlen verwenden.",
                "Invalid tag or field ID: use positive integers.",
            ))
        } else {
            None
        };
        if error.is_some() {
            return paperless_view(&state, &headers, false, error);
        }
    }
    let task_state = state.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(form.enabled.as_str(), "yes" | "no"),
            "invalid Paperless mode"
        );
        let config = if form.enabled == "yes" {
            let tags = form
                .tags
                .split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()?;
            anyhow::ensure!(tags.len() <= 64, "too many Paperless tags");
            let field_id = if form.field_id.trim().is_empty() {
                None
            } else {
                Some(form.field_id.trim().parse::<u32>()?)
            };
            Some(PaperlessConfig {
                url: form.url.trim().to_owned(),
                token_env: lock!(task_state.paperless_config)
                    .as_ref()
                    .map_or_else(|| "PAPERLESS_API_TOKEN".into(), |cfg| cfg.token_env.clone()),
                token_file: None,
                tag_ids: tags,
                batch_id_field_id: field_id,
            })
        } else {
            None
        };
        task_state.update_paperless(
            config,
            if form.new_token.is_empty() {
                None
            } else {
                Some(&form.new_token)
            },
        )
    })
    .await;
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(error))
            if error
                .downcast_ref::<crate::batch::CorruptBatchError>()
                .is_some() =>
        {
            Some(message(
                &headers,
                "Defekte Scanmetadaten blockieren den Zielwechsel. Scanübersicht prüfen.",
                "Corrupt scan metadata blocks changing the target. Check the scan list.",
            ))
        }
        Ok(Err(error))
            if matches!(
                error.to_string().as_str(),
                "scanner batch still in progress" | "Paperless delivery still pending"
            ) =>
        {
            Some(message(
                &headers,
                "Offene Scans oder ausstehende Zustellungen blockieren den Zielwechsel.",
                "Open scans or pending deliveries block changing the target.",
            ))
        }
        _ => Some(message(
            &headers,
            "Speichern fehlgeschlagen. Token-Konfiguration und privaten Dateispeicher prüfen.",
            "Saving failed. Check the token configuration and private file storage.",
        )),
    };
    paperless_view(&state, &headers, error.is_none(), error)
}

#[derive(Deserialize)]
pub struct ScannerSecurityForm {
    mode: String,
    username: String,
    new_password: String,
}

fn security_lang(headers: &HeaderMap) -> &'static str {
    if headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').any(|part| part.trim() == "ui_lang=en"))
    {
        "en"
    } else {
        "de"
    }
}

fn scanner_security_view(
    state: &AppState,
    headers: &HeaderMap,
    error: bool,
    saved: bool,
) -> Response {
    let current = lock!(state.scanner_auth_config).clone();
    render(ScannerSecurityTpl {
        mode: current.as_ref().map_or_else(
            || state.scanner_auth_mode.wire_type().to_owned(),
            |cfg| cfg.mode.clone(),
        ),
        username: current.and_then(|cfg| cfg.username).unwrap_or_default(),
        has_password: lock!(state.scanner_auth_config)
            .as_ref()
            .and_then(|cfg| cfg.password_hash.as_ref())
            .is_some(),
        error,
        saved,
        lang: security_lang(headers),
    })
}

pub async fn scanner_security_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    scanner_security_view(&state, &headers, false, false)
}

pub async fn scanner_security_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ScannerSecurityForm>,
) -> Response {
    if !super::admin::secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.new_password.len() > 512 || form.username.len() > 128 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let authorized =
        super::admin_cookie(&headers).is_some_and(|session| auth.consume_passkey_stepup(session));
    if !authorized {
        return confirmation_expired(&headers);
    }
    let state_for_write = state.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let password_hash = if form.new_password.is_empty() {
            lock!(state_for_write.scanner_auth_config)
                .as_ref()
                .and_then(|cfg| cfg.password_hash.clone())
        } else {
            anyhow::ensure!(
                (12..=512).contains(&form.new_password.len()),
                "scanner password must be 12..512 bytes"
            );
            Some(auth.hash_secret(&form.new_password)?)
        };
        let config = ScannerAuthConfig {
            mode: form.mode,
            username: if form.username.is_empty() {
                None
            } else {
                Some(form.username)
            },
            password_hash,
        };
        state_for_write.update_scanner_auth(config, auth.pepper())
    })
    .await;
    let saved = matches!(result, Ok(Ok(())));
    scanner_security_view(&state, &headers, !saved, saved)
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTpl {
    archive_after_days: u32,
    delete_after_days: u32,
    saved: bool,
    error: Option<String>,
    t: &'static Translations,
    build: &'static BuildInfo,
    default_lang: String,
}

#[derive(Deserialize)]
pub struct LanguageForm {
    lang: String,
}

pub async fn language_post(UiState(state): UiState, Form(form): Form<LanguageForm>) -> Response {
    if state.update_language(&form.lang).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    axum::response::Redirect::to("/settings").into_response()
}

#[derive(Deserialize)]
pub struct SettingsFormData {
    pub archive_after_days: u32,
    pub delete_after_days: u32,
}

pub async fn settings_get(UiState(state): UiState) -> Response {
    let cfg = lock!(state.retention).clone();
    render(SettingsTpl {
        archive_after_days: cfg.archive_after_days,
        delete_after_days: cfg.delete_after_days,
        saved: false,
        error: None,
        t: state.translations,
        build: state.build_info,
        default_lang: lock!(state.lang).clone(),
    })
}

pub async fn settings_post(
    UiState(state): UiState,
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
            default_lang: lock!(state.lang).clone(),
        });
    }
    let new_cfg = RetentionConfig {
        archive_after_days: form.archive_after_days,
        delete_after_days: form.delete_after_days,
    };
    let _change = lock!(state.config_change_lock);
    let jobs = lock!(state.jobs).clone();
    if let Err(e) = state.persist_config(&jobs, &new_cfg, *lock!(state.next_job_id)) {
        tracing::error!(
            event = "config.save_failed",
            error_class = crate::logging::error_class(&e),
            "failed to save config"
        );
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    *lock!(state.retention) = new_cfg.clone();
    render(SettingsTpl {
        archive_after_days: new_cfg.archive_after_days,
        delete_after_days: new_cfg.delete_after_days,
        saved: true,
        error: None,
        t: state.translations,
        build: state.build_info,
        default_lang: lock!(state.lang).clone(),
    })
}

#[cfg(test)]
mod tests {

    use crate::config::{Config, Job};
    use crate::routes::test_support::TestApp;
    use crate::state::AppState;

    fn test_server() -> (TestApp, tempfile::TempDir) {
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
        (TestApp::new(AppState::new(config)), tmp)
    }

    #[tokio::test]
    async fn paperless_errors_distinguish_confirmation_url_and_ids() {
        let root = tempfile::tempdir().unwrap();
        let auth = crate::admin_auth::AdminAuth::open(root.path(), &[0x43; 32]).unwrap();
        let session = auth.test_session();
        let mut state = AppState::new(Config::default());
        state.admin_auth = Some(auth.clone());
        let server = axum_test::TestServer::new(crate::routes::admin_router_with_hosts(
            state,
            vec!["localhost".into()],
        ));
        for (confirmed, url, tags, expected, status) in [
            (false, "http://localhost:8000", "", "Bestätigung", 401),
            (true, "file:///private", "", "URL", 200),
            (true, "http://localhost:8000", "wrong", "Tag", 200),
        ] {
            if confirmed {
                auth.test_confirm_stepup(&session);
            }
            let response = server
                .post("/settings/paperless")
                .add_header("host", "localhost")
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .add_header("cookie", format!("__Host-nx-admin={session}"))
                .form(&[
                    ("enabled", "yes"),
                    ("url", url),
                    ("tags", tags),
                    ("field_id", ""),
                    ("new_token", ""),
                ])
                .await;
            assert_eq!(response.status_code(), status);
            assert!(response.text().contains(expected), "missing {expected}");
        }
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
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
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
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
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
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
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
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[("archive_after_days", "7"), ("delete_after_days", "0")])
            .await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.text().contains("Gespeichert"));
    }
}
