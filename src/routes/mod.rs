use crate::state::AppState;
use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use jobs::{jobs_create, jobs_delete, jobs_edit, jobs_list, jobs_new, jobs_update};
use scans::{scans_detail, scans_file, scans_list};
use settings::{settings_get, settings_post};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;
use ui::{dashboard, scanner_status};

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
        .map(|s| s.to_ascii_lowercase());

    match ct.as_deref() {
        Some(s) if s.starts_with("multipart/form-data") => {}
        // Pass form submissions through only for UI routes; scanner routes must get JSON.
        Some(s) if !is_scanner && s.starts_with("application/x-www-form-urlencoded") => {}
        Some(s) if s.starts_with("multipart/") => {
            // Preserve the boundary parameter while normalising the subtype.
            let boundary_param = s
                .split(';')
                .skip(1)
                .find(|p| p.trim_start().starts_with("boundary"))
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

    if is_mutation && !is_scanner {
        let hx = req
            .headers()
            .get("hx-request")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !hx.eq_ignore_ascii_case("true") {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    next.run(req).await
}

async fn health() -> StatusCode {
    StatusCode::OK
}

pub fn router(state: AppState) -> Router {
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    Router::new()
        .route("/health", get(health))
        // UI
        .route("/", get(dashboard))
        .route("/api/scanner-status", get(scanner_status))
        .route("/jobs", get(jobs_list))
        .route("/jobs/new", get(jobs_new))
        .route("/jobs", post(jobs_create))
        .route("/jobs/{id}/edit", get(jobs_edit))
        .route("/jobs/{id}", post(jobs_update))
        .route("/jobs/{id}", delete(jobs_delete))
        .route("/scans", get(scans_list))
        .route("/scans/{id}", get(scans_detail))
        .route("/scans/{id}/files/{filename}", get(scans_file))
        .route("/settings", get(settings_get))
        .route("/settings", post(settings_post))
        // Scanner protocol
        .route("/NmWebService/heartbeat", get(scanner::heartbeat))
        .route("/NmWebService/device", post(scanner::device))
        .route(
            "/NmWebService/authorization",
            get(scanner::get_authorization),
        )
        .route(
            "/NmWebService/authorization",
            post(scanner::post_authorization),
        )
        .route("/NmWebService/scansetting", get(scanner::scansetting))
        .route("/NmWebService/batch", post(batch::post_batch))
        .route("/NmWebService/batch/{batch_id}", put(batch::put_batch))
        .route("/NmWebService/image", post(image::post_image))
        .route(
            "/NmWebService/accesstoken",
            delete(scanner::delete_accesstoken),
        )
        .layer(middleware::from_fn(force_json))
        .layer(middleware::from_fn(csrf_check))
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024)) // 100 MB
        .layer(trace_layer)
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
}
