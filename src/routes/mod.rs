use crate::state::AppState;
use axum::{
    Router,
    extract::{DefaultBodyLimit, FromRequestParts, MatchedPath, Path, Query, Request, State},
    http::{HeaderValue, StatusCode, header, request::Parts, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post, put},
};
use jobs::{
    jobs_create, jobs_delete, jobs_edit, jobs_expert_get, jobs_expert_post, jobs_list, jobs_new,
    jobs_reorder, jobs_update,
};
use scans::{scans_detail, scans_file, scans_list};
use settings::{
    language_post, paperless_get, paperless_post, scanner_security_get, scanner_security_post,
    settings_get, settings_post,
};
use tracing::Instrument;
use ui::{dashboard, events, scanner_status};

pub struct UiState(pub AppState);

impl FromRequestParts<AppState> for UiState {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let mut localized = state.clone();
        localized.translations = crate::translations::for_lang(request_lang(state, &parts.headers));
        Ok(Self(localized))
    }
}

async fn switch_language(
    Path(lang): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !matches!(lang.as_str(), "de" | "en") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let next = query
        .get("next")
        .map(String::as_str)
        .filter(|path| {
            path.starts_with('/')
                && !path.starts_with("//")
                && !path.contains('\\')
                && !path.chars().any(char::is_control)
        })
        .unwrap_or("/");
    let cookie = format!("ui_lang={lang}; Path=/; Max-Age=31536000; SameSite=Lax; HttpOnly");
    ([(header::SET_COOKIE, cookie)], Redirect::to(next)).into_response()
}

mod admin;
mod api_docs;
mod batch;
#[cfg(test)]
mod e2e_test;
mod image;
pub mod jobs;
mod scanner;
pub mod scans;
pub mod settings;
pub mod ui;

// Scanner sends requests without Content-Type: application/json.
// Force it for all non-multipart requests so Axum's Json extractor accepts them.
// Also normalize any multipart/* subtype to multipart/form-data so axum's
// Multipart extractor accepts it (scanners may send multipart/mixed).
async fn force_json(mut req: Request, next: Next) -> Response {
    let is_scanner = req.uri().path().starts_with("/NmWebService/");
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    match ct.as_deref() {
        Some(s) if s.to_ascii_lowercase().starts_with("multipart/form-data") => {}
        // Pass form submissions through only for UI routes; scanner routes must get JSON.
        Some(s)
            if !is_scanner
                && s.to_ascii_lowercase()
                    .starts_with("application/x-www-form-urlencoded") => {}
        Some(s) if s.to_ascii_lowercase().starts_with("multipart/") => {
            // Preserve the boundary parameter while normalising the subtype.
            let boundary_param = s
                .split(';')
                .skip(1)
                .find(|p| p.trim_start().to_ascii_lowercase().starts_with("boundary"))
                .unwrap_or("");
            let new_ct = format!("multipart/form-data; {}", boundary_param.trim());
            if let Ok(v) = new_ct.parse() {
                req.headers_mut().insert(header::CONTENT_TYPE, v);
            }
        }
        _ => {
            req.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
    }
    next.run(req).await
}

async fn admin_origin_check(req: Request, next: Next) -> Response {
    if matches!(req.method().as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
        && !admin::secure_origin(req.headers())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

fn cookie_value<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(';'))
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name && !value.is_empty()).then_some(value)
        })
}

fn request_lang(state: &AppState, headers: &axum::http::HeaderMap) -> &'static str {
    match cookie_value(headers, "ui_lang") {
        Some("en") => "en",
        Some("de") => "de",
        _ if crate::lock!(state.lang).as_str() == "en" => "en",
        _ => "de",
    }
}

fn admin_cookie(headers: &axum::http::HeaderMap) -> Option<&str> {
    cookie_value(headers, "__Host-nx-admin")
}

async fn admin_session_gate(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();
    if path.starts_with("/NmWebService/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if path.starts_with("/static/") || path.starts_with("/language/") {
        return next.run(req).await;
    }
    let Some(ref auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let configured = auth.configured();
    let valid = admin_cookie(req.headers()).is_some_and(|token| auth.session_valid(token));
    let trust = cookie_value(req.headers(), "__Host-nx-trust").map(str::to_owned);
    let mut clear_trust = trust
        .as_ref()
        .is_some_and(|token| !auth.trusted_cookie_valid(token));
    let mut restored = None;
    if configured
        && !valid
        && req.method() == axum::http::Method::GET
        && !clear_trust
        && let Some(token) = trust.as_ref()
    {
        match auth.restore_trusted_session(token) {
            Ok(Some(session)) => {
                let mut parts: Vec<String> = req
                    .headers()
                    .get_all(header::COOKIE)
                    .iter()
                    .filter_map(|h| h.to_str().ok())
                    .flat_map(|h| h.split(';'))
                    .filter(|part| {
                        part.trim()
                            .split_once('=')
                            .is_none_or(|(name, _)| name != "__Host-nx-admin")
                    })
                    .map(|p| p.trim().to_owned())
                    .collect();
                parts.push(format!("__Host-nx-admin={session}"));
                req.headers_mut().remove(header::COOKIE);
                req.headers_mut().insert(
                    header::COOKIE,
                    HeaderValue::from_str(&parts.join("; ")).expect("valid cookie header"),
                );
                restored = Some(session);
            }
            Ok(None) => clear_trust = true,
            Err(_) => {
                tracing::warn!(
                    event = "admin.trust_restore_failed",
                    error_class = "storage",
                    "trusted session unavailable; passkey required"
                );
                clear_trust = true;
            }
        }
    }
    let mut response = if !configured {
        if matches!(
            path.as_str(),
            "/setup" | "/passkey/setup/start" | "/passkey/setup/finish"
        ) {
            next.run(req).await
        } else if req.method() == axum::http::Method::GET {
            Redirect::to("/setup").into_response()
        } else {
            StatusCode::UNAUTHORIZED.into_response()
        }
    } else if matches!(
        path.as_str(),
        "/login" | "/passkey/login/start" | "/passkey/login/finish"
    ) {
        if path == "/login" && restored.is_some() {
            Redirect::to("/").into_response()
        } else {
            next.run(req).await
        }
    } else if valid || restored.is_some() {
        next.run(req).await
    } else if req.method() == axum::http::Method::GET {
        Redirect::to("/login").into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    };
    if let Some(session) = restored {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&admin::session_cookie(&session)).expect("session cookie"),
        );
    }
    if clear_trust
        && !response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|v| v.to_str().is_ok_and(|v| v.starts_with("__Host-nx-trust=")))
    {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_static(admin::clear_trust_cookie()),
        );
    }
    response
}

pub(super) fn scanner_bearer(headers: &axum::http::HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer")
        && !token.is_empty()
        && !token.contains(char::is_whitespace))
    .then_some(token)
}

async fn scanner_session_gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if state.scanner_auth_mode.requires_login()
        && path.starts_with("/NmWebService/")
        && !matches!(
            path,
            "/NmWebService/heartbeat" | "/NmWebService/device" | "/NmWebService/authorization"
        )
    {
        let Some(token) = scanner_bearer(req.headers()) else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        if !state.scanner_auth_mode.session_valid(token) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if req
            .headers()
            .get("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            != Some("https")
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    next.run(req).await
}

async fn health() -> StatusCode {
    StatusCode::OK
}

// Log the registered route pattern, never the URI, query string, headers, or body.
// The generated ID correlates access logs with handler events and the HTTP response.
async fn request_log(request: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("<unmatched>")
        .to_owned();
    let started = std::time::Instant::now();
    let span = tracing::info_span!("request", request_id = %request_id);
    let mut response = next.run(request).instrument(span).await;
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&request_id).expect("UUID is a valid header value"),
    );
    let status = response.status().as_u16();
    if route != "/health" || status != 200 {
        tracing::info!(
            event = "http.request",
            request_id = %request_id,
            http_method = %method,
            http_route = %route,
            http_status = status,
            duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "request completed"
        );
    }
    response
}

async fn browser_security_headers(req: Request, next: Next) -> Response {
    let private = !req.uri().path().starts_with("/static/");
    let mut response = next.run(req).await;
    if private {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}

async fn admin_host_check(
    State(allowed_hosts): State<Vec<String>>,
    req: Request,
    next: Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Authority>().ok())
        .map(|authority| authority.host().to_ascii_lowercase());
    if !host.is_some_and(|host| allowed_hosts.iter().any(|allowed| allowed == &host)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

async fn htmx_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!("../../static/htmx.min.js"),
    )
}

async fn auth_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../static/auth.css"),
    )
}

async fn passkey_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!("../../static/passkey.js"),
    )
}

async fn scan_rows_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!("../../static/scan-rows.js"),
    )
}

async fn ui_preferences_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!("../../static/ui-preferences.js"),
    )
}

async fn job_rows_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!("../../static/job-rows.js"),
    )
}

fn admin_routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(admin::login_get))
        .route("/passkey/login/start", post(admin::passkey_login_start))
        .route("/passkey/login/finish", post(admin::passkey_login_finish))
        .route("/passkey/setup/start", post(admin::passkey_setup_start))
        .route("/passkey/setup/finish", post(admin::passkey_setup_finish))
        .route("/passkey/stepup/start", post(admin::passkey_stepup_start))
        .route("/passkey/stepup/finish", post(admin::passkey_stepup_finish))
        .route("/passkey/account/add/start", post(admin::passkey_add_start))
        .route("/passkey/account/remove", post(admin::passkey_remove))
        .route(
            "/passkey/account/add/finish",
            post(admin::passkey_add_finish),
        )
        .route("/account/trusted/remove", post(admin::trusted_remove))
        .route(
            "/account/trusted/remove-all",
            post(admin::trusted_remove_all),
        )
        .route("/logout", post(admin::logout_post))
        .route("/setup", get(admin::setup_get))
        .route("/account", get(admin::account_get))
        .route("/static/htmx.min.js", get(htmx_script))
        .route("/static/auth.css", get(auth_css))
        .route("/static/scan-rows.js", get(scan_rows_script))
        .route("/static/job-rows.js", get(job_rows_script))
        .route("/static/ui-preferences.js", get(ui_preferences_script))
        .route("/static/api-docs.js", get(api_docs::script))
        .route("/static/passkey.js", get(passkey_script))
        .route("/language/{lang}", get(switch_language))
        .route("/", get(dashboard))
        .route("/events", get(events))
        .route("/api-docs", get(api_docs::page))
        .route("/api/openapi.json", get(api_docs::openapi_json))
        .route("/api/scanner-status", get(scanner_status))
        .route("/jobs", get(jobs_list))
        .route("/jobs/new", get(jobs_new))
        .route("/jobs", post(jobs_create))
        .route("/jobs/reorder", post(jobs_reorder))
        .route("/jobs/{id}/edit", get(jobs_edit))
        .route(
            "/jobs/{id}/expert",
            get(jobs_expert_get).post(jobs_expert_post),
        )
        .route("/jobs/{id}", post(jobs_update))
        .route("/jobs/{id}", delete(jobs_delete))
        .route("/scans", get(scans_list))
        .route("/scans/{id}", get(scans_detail))
        .route("/scans/{id}/{action}", post(scans::recover))
        .route("/scans/{id}/files/{filename}", get(scans_file))
        .route("/settings", get(settings_get))
        .route("/settings", post(settings_post))
        .route("/settings/language", post(language_post))
        .route(
            "/settings/scanner",
            get(scanner_security_get).post(scanner_security_post),
        )
        .route(
            "/settings/paperless",
            get(paperless_get).post(paperless_post),
        )
}

fn scanner_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/NmWebService/heartbeat", get(scanner::heartbeat))
        .route(
            "/NmWebService/device",
            post(scanner::device).layer(axum::extract::DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/NmWebService/authorization",
            get(scanner::get_authorization),
        )
        .route(
            "/NmWebService/authorization",
            post(scanner::post_authorization).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/NmWebService/scansetting", get(scanner::scansetting))
        .route("/NmWebService/batch", post(batch::post_batch))
        .route("/NmWebService/batch/{batch_id}", put(batch::put_batch))
        .route("/NmWebService/image", post(image::post_image))
        .route(
            "/NmWebService/accesstoken",
            delete(scanner::delete_accesstoken),
        )
}

#[cfg(test)]
pub fn admin_router(state: AppState) -> Router {
    admin_router_with_hosts(state, vec!["localhost".into(), "127.0.0.1".into()])
}

pub fn admin_router_with_hosts(state: AppState, allowed_hosts: Vec<String>) -> Router {
    admin_routes()
        .layer(middleware::from_fn(admin_origin_check))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_session_gate,
        ))
        .layer(middleware::from_fn(browser_security_headers))
        .layer(middleware::from_fn_with_state(
            allowed_hosts,
            admin_host_check,
        ))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn(request_log))
        .with_state(state)
}

pub fn scanner_router(state: AppState) -> Router {
    scanner_routes()
        .layer(middleware::from_fn_with_state(
            state.clone(),
            scanner_session_gate,
        ))
        .layer(middleware::from_fn(force_json))
        .layer(DefaultBodyLimit::max(34 * 1024 * 1024))
        .layer(middleware::from_fn(request_log))
        .with_state(state)
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use crate::routes::test_support::TestApp;
    use axum_test::TestServer;
    use serde_json::json;

    use crate::config::{Config, Job};
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

    #[test]
    fn language_and_session_parse_separate_cookie_headers() {
        use super::{admin_cookie, request_lang};
        use axum::http::{HeaderValue, header};
        let state = crate::state::AppState::new(Default::default());
        let mut headers = axum::http::HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("__Host-nx-admin=session"),
        );
        headers.append(header::COOKIE, HeaderValue::from_static("ui_lang=en"));
        assert_eq!(admin_cookie(&headers), Some("session"));
        assert_eq!(request_lang(&state, &headers), "en");
    }

    #[tokio::test]
    async fn security_pages_use_server_language_and_explicit_cookie() {
        let state = crate::state::AppState::new(crate::config::Config {
            lang: "en".into(),
            ..Default::default()
        });
        let server = crate::routes::test_support::TestApp::new(state);
        for path in [
            "/login",
            "/account",
            "/settings/scanner",
            "/settings/paperless",
        ] {
            let response = server.get(path).await;
            assert!(
                response.text().contains("lang=\"en\""),
                "server language ignored on {path}"
            );
            let response = server
                .get(path)
                .add_header("cookie", server.cookie("ui_lang=de"))
                .await;
            assert!(
                response.text().contains("lang=\"de\""),
                "cookie ignored on {path}"
            );
        }
    }

    #[tokio::test]
    async fn admin_pages_disable_caching_and_restrict_form_destinations() {
        let (server, _tmp) = test_server();
        for path in ["/", "/scans", "/settings"] {
            let response = server.get(path).await;
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok()),
                Some("private, no-store")
            );
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("form-action 'self'")
            );
        }
    }

    #[tokio::test]
    async fn protected_mutations_require_session_and_same_origin() {
        let root = tempfile::tempdir().unwrap();
        let auth = crate::admin_auth::AdminAuth::open(root.path(), &[0x42; 32]).unwrap();
        let session = auth.test_session();
        let state = AppState::new(Config::default()).with_admin_auth(auth);
        let server = TestServer::new(super::admin_router(state));
        for path in [
            "/settings",
            "/settings/language",
            "/settings/scanner",
            "/settings/paperless",
            "/jobs",
            "/jobs/reorder",
            "/jobs/0",
            "/jobs/0/expert",
            "/logout",
            "/passkey/stepup/start",
            "/passkey/stepup/finish",
            "/passkey/account/add/start",
            "/passkey/account/add/finish",
            "/passkey/account/remove",
            "/account/trusted/remove",
            "/account/trusted/remove-all",
            "/scans/0123456789abcdef0123456789abcdef/complete",
            "/scans/0123456789abcdef0123456789abcdef/discard",
            "/scans/0123456789abcdef0123456789abcdef/retry",
            "/scans/0123456789abcdef0123456789abcdef/assign",
        ] {
            assert_eq!(
                server.post(path).await.status_code(),
                401,
                "missing session: {path}"
            );
            assert_eq!(
                server
                    .post(path)
                    .add_header("cookie", format!("__Host-nx-admin={session}"))
                    .add_header("hx-request", "true")
                    .await
                    .status_code(),
                403,
                "HX alone is insufficient: {path}"
            );
            assert_eq!(
                server
                    .post(path)
                    .add_header("cookie", format!("__Host-nx-admin={session}"))
                    .add_header("origin", "https://attacker.invalid")
                    .add_header("x-forwarded-proto", "https")
                    .await
                    .status_code(),
                403,
                "wrong origin: {path}"
            );
        }
        assert_eq!(
            server
                .delete("/jobs/0")
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .await
                .status_code(),
            401
        );
    }

    #[tokio::test]
    async fn test_scanner_request_no_content_type_accepted_as_json() {
        // Scanners often send no Content-Type; middleware must inject application/json
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .bytes(br#"{"job_id":0}"#.as_ref().into())
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_scanner_request_uppercase_content_type_accepted() {
        // RFC 2616: header field names are case-insensitive; middleware lowercases before matching
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .content_type("Application/Json")
            .bytes(br#"{"job_id":0}"#.as_ref().into())
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_form_post_to_ui_route_not_coerced_to_json() {
        // UI form submissions must keep application/x-www-form-urlencoded so axum Form works
        let (server, _tmp) = test_server();
        let out = _tmp.path().join("sub");
        std::fs::create_dir_all(&out).unwrap();
        let resp = server
            .post("/jobs")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[
                ("name", "FormJob"),
                ("output_path", out.to_str().unwrap()),
                ("color", "#000000"),
                ("resolution", "300"),
                ("jpeg_quality", "80"),
                ("pixel_format", "rgb24"),
                ("consume_path", ""),
            ])
            .await;
        // HX-Redirect means form was parsed correctly (htmx path returns 204 + HX-Redirect)
        assert_eq!(resp.status_code(), 204);
    }

    #[tokio::test]
    async fn test_admin_mutation_without_matching_origin_is_forbidden() {
        let (server, _tmp) = test_server();
        let response = server
            .post("/settings")
            .add_header("origin", "https://attacker.invalid")
            .add_header("x-forwarded-proto", "https")
            .form(&[("archive_after_days", "7"), ("delete_after_days", "30")])
            .await;
        assert_eq!(response.status_code(), 403);
    }

    #[tokio::test]
    async fn test_htmx_does_not_replace_matching_origin() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/settings")
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .add_header("HX-Request", "true")
            .form(&[("archive_after_days", "7"), ("delete_after_days", "30")])
            .await;
        assert_ne!(resp.status_code(), 403);
    }

    #[tokio::test]
    async fn test_scanner_router_accepts_requests_without_admin_origin() {
        // Scanner routes must work without HX-Request (scanner doesn't use HTMX).
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .bytes(br#"{"job_id":0}"#.as_ref().into())
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_authenticated_get_does_not_require_origin() {
        // The real admin router requires a session for GET, but no Origin header.
        let (server, _tmp) = test_server();
        assert_eq!(server.get("/settings").await.status_code(), 200);
    }

    #[tokio::test]
    async fn test_htmx_is_served_locally() {
        let (server, _tmp) = test_server();
        let resp = server.get("/static/htmx.min.js").await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.text().contains("htmx"));
    }

    #[tokio::test]
    async fn test_admin_response_has_browser_security_headers() {
        let (server, _tmp) = test_server();
        let resp = server.get("/").await;
        assert_eq!(resp.status_code(), 200);
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert!(resp.headers().get("content-security-policy").is_some());
    }

    #[tokio::test]
    async fn test_admin_rejects_foreign_host_header() {
        let state = AppState::new(Config::default());
        let server = TestServer::new(super::admin_router(state));
        let resp = server
            .get("/")
            .add_header("host", "attacker.example:10448")
            .await;
        assert_eq!(resp.status_code(), 403);
    }

    #[tokio::test]
    async fn test_admin_accepts_configured_proxy_host() {
        let state = AppState::new(Config::default());
        let server = TestServer::new(super::admin_router_with_hosts(
            state,
            vec!["scanner.example.internal".into()],
        ));
        let resp = server
            .get("/")
            .add_header("host", "scanner.example.internal:10448")
            .await;
        assert_eq!(resp.status_code(), 503);
    }

    #[tokio::test]
    async fn unconfigured_admin_cannot_read_scans_or_change_settings() {
        let dir = tempfile::tempdir().unwrap();
        let auth = crate::admin_auth::AdminAuth::open(dir.path(), &[0x42; 32]).unwrap();
        let state = AppState::new(Config::default()).with_admin_auth(auth);
        let server = TestServer::new(super::admin_router_with_hosts(
            state,
            vec!["localhost".into()],
        ));
        let dashboard = server.get("/").await;
        assert_eq!(dashboard.status_code(), 303);
        assert_eq!(dashboard.headers()["location"], "/setup");
        assert_eq!(server.get("/setup").await.status_code(), 200);
        assert_eq!(server.get("/scans").await.status_code(), 303);
        assert_eq!(
            server
                .post("/settings")
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .add_header("hx-request", "true")
                .await
                .status_code(),
            401
        );
    }

    #[tokio::test]
    async fn configured_admin_requires_session_and_logout_revokes_it() {
        let dir = tempfile::tempdir().unwrap();
        let auth = crate::admin_auth::AdminAuth::open(dir.path(), &[0x42; 32]).unwrap();
        let session = auth.test_session();
        let state = AppState {
            admin_auth: Some(auth),
            ..AppState::new(Config::default())
        };
        let server = TestServer::new(super::admin_router(state));
        let cookie = format!("__Host-nx-admin={session}");
        assert_eq!(
            server
                .get("/")
                .add_header("cookie", &cookie)
                .await
                .status_code(),
            200
        );
        assert_eq!(
            server
                .get("/scans")
                .add_header("cookie", &cookie)
                .await
                .status_code(),
            200
        );
        assert_eq!(
            server
                .post("/logout")
                .add_header("cookie", &cookie)
                .add_header("hx-request", "true")
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .await
                .status_code(),
            303
        );
        assert_eq!(
            server
                .get("/")
                .add_header("cookie", &cookie)
                .await
                .status_code(),
            303
        );
    }

    #[tokio::test]
    async fn test_scanner_listener_exposes_no_admin_routes() {
        let (_server, tmp) = test_server();
        let state = AppState::new(Config {
            jobs: vec![],
            ..Default::default()
        });
        let server = TestServer::new(super::scanner_router(state));
        assert_eq!(server.get("/jobs").await.status_code(), 404);
        assert_eq!(
            server.get("/NmWebService/heartbeat").await.status_code(),
            200
        );
        drop(tmp);
    }

    #[tokio::test]
    async fn test_admin_listener_exposes_no_scanner_routes() {
        let state = AppState::new(Config::default());
        let server = TestServer::new(super::admin_router(state));
        assert_eq!(server.get("/jobs").await.status_code(), 503);
        assert_eq!(
            server.get("/NmWebService/heartbeat").await.status_code(),
            404
        );
    }

    #[tokio::test]
    async fn language_cookie_localizes_each_request_independently() {
        let (server, _tmp) = test_server();
        let english = server
            .get("/")
            .clear_headers()
            .add_header("host", "localhost")
            .add_header("cookie", server.cookie("ui_lang=en"));
        let german = server
            .get("/")
            .clear_headers()
            .add_header("host", "localhost")
            .add_header("cookie", server.cookie("ui_lang=de"));
        let (english, german) = tokio::join!(english, german);
        assert!(english.text().contains("<html lang=\"en\">"));
        assert!(english.text().contains("Recent Scans"));
        assert!(german.text().contains("<html lang=\"de\">"));
        assert!(german.text().contains("Letzte Scans"));
    }

    #[tokio::test]
    async fn language_switch_sets_cookie_and_uses_local_return_path() {
        let (server, _tmp) = test_server();
        let response = server.get("/language/en?next=%2Fscans").await;
        assert_eq!(response.status_code(), 303);
        assert_eq!(response.headers()["location"], "/scans");
        assert!(
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("ui_lang=en")
        );
        let unsafe_response = server.get("/language/de?next=%2F%2Fevil.example").await;
        assert_eq!(unsafe_response.headers()["location"], "/");
    }
}

#[cfg(test)]
mod trusted_route_tests {
    use super::*;
    use crate::{admin_auth::AdminAuth, config::Config};
    use axum_test::TestServer;
    #[tokio::test]
    async fn trust_restores_get_only_and_never_supplies_confirmation() {
        let root = tempfile::tempdir().unwrap();
        let auth = AdminAuth::open(root.path(), &[0x42; 32]).unwrap();
        let original = auth.test_session();
        let trust = auth.trust_device(&original, "Laptop").unwrap();
        let server = TestServer::new(admin_router(
            AppState::new(Config::default()).with_admin_auth(auth.clone()),
        ));
        let cookie = format!("__Host-nx-trust={trust}");
        let post = server
            .post("/settings/language")
            .add_header("cookie", &cookie)
            .add_header("origin", "https://localhost")
            .add_header("x-forwarded-proto", "https")
            .form(&[("lang", "en")])
            .await;
        assert_eq!(post.status_code(), 401);
        let get = server.get("/account").add_header("cookie", &cookie).await;
        assert_eq!(get.status_code(), 200);
        assert!(get.text().contains("Dieses Gerät"));
        assert!(!get.text().contains(&trust));
        let session = get
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|v| v.strip_prefix("__Host-nx-admin="))
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(auth.session_valid(session));
        assert!(!auth.consume_passkey_stepup(session));
        for route in ["/account/trusted/remove", "/account/trusted/remove-all"] {
            let denied = server
                .post(route)
                .add_header("cookie", format!("__Host-nx-admin={session}; {cookie}"))
                .add_header("origin", "https://localhost")
                .add_header("x-forwarded-proto", "https")
                .json(&serde_json::json!({"device_id":auth.trusted_devices(None)[0].id}))
                .await;
            assert_eq!(denied.status_code(), 401);
        }
        auth.test_confirm_stepup(&original);
        assert!(auth.revoke_trusted_devices(&original, None).unwrap());
        assert!(!auth.session_valid(session));
        let revoked = server.get("/account").add_header("cookie", &cookie).await;
        assert_eq!(revoked.status_code(), 303);
        assert!(
            revoked
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .any(|v| v
                    .to_str()
                    .is_ok_and(|v| v.starts_with("__Host-nx-trust=;") && v.contains("Max-Age=0")))
        );
    }
    #[tokio::test]
    async fn unknown_trust_cookie_is_cleared_even_with_valid_session() {
        let root = tempfile::tempdir().unwrap();
        let auth = AdminAuth::open(root.path(), &[0x42; 32]).unwrap();
        let session = auth.test_session();
        let server = TestServer::new(admin_router(
            AppState::new(Config::default()).with_admin_auth(auth),
        ));
        let response = server
            .get("/account")
            .add_header(
                "cookie",
                format!(
                    "__Host-nx-admin={session}; __Host-nx-trust={}",
                    "f".repeat(64)
                ),
            )
            .await;
        assert_eq!(response.status_code(), 200);
        assert!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .any(|v| v
                    .to_str()
                    .is_ok_and(|v| v.contains("__Host-nx-trust=;") && v.contains("Max-Age=0")))
        );
    }
}
