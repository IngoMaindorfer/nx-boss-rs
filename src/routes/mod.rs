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
        let cookie_lang = parts
            .headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|cookies| {
                cookies.split(';').find_map(|cookie| {
                    let (name, value) = cookie.trim().split_once('=')?;
                    (name == "ui_lang" && matches!(value, "de" | "en")).then_some(value)
                })
            });
        let lang = cookie_lang
            .map(str::to_owned)
            .unwrap_or_else(|| crate::lock!(state.lang).clone());
        localized.translations = crate::translations::for_lang(&lang);
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

// Custom headers (like HX-Request) cannot be sent cross-origin without a CORS preflight
// that the server must explicitly allow. Requiring this header for all UI mutations
// blocks cross-site form submissions without a per-request token.
async fn csrf_check(req: Request, next: Next) -> Response {
    let is_mutation = matches!(req.method().as_str(), "POST" | "PUT" | "PATCH" | "DELETE");
    let is_scanner = req.uri().path().starts_with("/NmWebService/");

    let is_auth = req.uri().path() == "/login";
    if is_mutation && !is_scanner && !is_auth {
        let hx = req
            .headers()
            .get("hx-request")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !hx.eq_ignore_ascii_case("true") && !admin::secure_origin(req.headers()) {
            return StatusCode::FORBIDDEN.into_response();
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

fn admin_cookie(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == "__Host-nx-admin" && !value.is_empty()).then_some(value)
        })
}

async fn admin_session_gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path.starts_with("/NmWebService/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if path.starts_with("/static/") || path.starts_with("/language/") {
        return next.run(req).await;
    }
    let Some(ref auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth.configured() {
        if matches!(
            path,
            "/setup" | "/passkey/setup/start" | "/passkey/setup/finish"
        ) {
            return next.run(req).await;
        }
        return if req.method() == axum::http::Method::GET {
            Redirect::to("/setup").into_response()
        } else {
            StatusCode::UNAUTHORIZED.into_response()
        };
    }
    if matches!(
        path,
        "/login" | "/passkey/login/start" | "/passkey/login/finish"
    ) {
        return next.run(req).await;
    }
    if admin_cookie(req.headers()).is_some_and(|token| auth.session_valid(token)) {
        return next.run(req).await;
    }
    if req.method() == axum::http::Method::GET {
        Redirect::to("/login").into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
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
    if state.scanner_auth_experiment.is_some()
        && path.starts_with("/NmWebService/")
        && !matches!(
            path,
            "/NmWebService/heartbeat" | "/NmWebService/device" | "/NmWebService/authorization"
        )
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
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
        .route(
            "/passkey/account/add/finish",
            post(admin::passkey_add_finish),
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
        .route("/NmWebService/device", post(scanner::device))
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
        .layer(middleware::from_fn(csrf_check))
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
pub fn router(state: AppState) -> Router {
    admin_routes()
        .merge(scanner_routes())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            scanner_session_gate,
        ))
        .layer(middleware::from_fn(force_json))
        .layer(middleware::from_fn(csrf_check))
        .layer(middleware::from_fn(browser_security_headers))
        .layer(DefaultBodyLimit::max(34 * 1024 * 1024))
        .layer(middleware::from_fn(request_log))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use serde_json::json;

    use crate::config::{Config, Job};
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
        (TestServer::new(super::router(AppState::new(config))), tmp)
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
    async fn test_csrf_ui_mutation_without_hx_request_is_forbidden() {
        // POST to UI route without HX-Request header must be rejected with 403.
        // This prevents cross-site form submissions — custom headers are blocked by CORS.
        let (server, _tmp) = test_server();
        let resp = server
            .post("/settings")
            .form(&[("archive_after_days", "7"), ("delete_after_days", "30")])
            .await;
        assert_eq!(resp.status_code(), 403);
    }

    #[tokio::test]
    async fn test_csrf_ui_mutation_with_hx_request_is_allowed() {
        let (server, _tmp) = test_server();
        let resp = server
            .post("/settings")
            .add_header("HX-Request", "true")
            .form(&[("archive_after_days", "7"), ("delete_after_days", "30")])
            .await;
        assert_ne!(resp.status_code(), 403);
    }

    #[tokio::test]
    async fn test_csrf_scanner_route_not_affected() {
        // Scanner routes must work without HX-Request (scanner doesn't use HTMX).
        let (server, _tmp) = test_server();
        let resp = server
            .post("/NmWebService/batch")
            .bytes(br#"{"job_id":0}"#.as_ref().into())
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_csrf_get_request_not_affected() {
        // CSRF guard must not block safe (idempotent) GET requests.
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
        let english = server.get("/").add_header("cookie", "ui_lang=en");
        let german = server.get("/").add_header("cookie", "ui_lang=de");
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
