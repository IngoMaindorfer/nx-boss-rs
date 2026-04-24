use crate::state::AppState;
use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::header,
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post, put},
};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;
use ui::{
    dashboard, jobs_create, jobs_delete, jobs_edit, jobs_list, jobs_new, jobs_update,
    scanner_status, scans_detail, scans_file, scans_list,
};

mod batch;
mod image;
mod scanner;
pub mod ui;

// Scanner sends requests without Content-Type: application/json.
// Force it for all non-multipart requests so Axum's Json extractor accepts them.
// Also normalize any multipart/* subtype to multipart/form-data so axum's
// Multipart extractor accepts it (scanners may send multipart/mixed).
async fn force_json(mut req: Request, next: Next) -> Response {
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase());

    match ct.as_deref() {
        Some(s) if s.starts_with("multipart/form-data") => {}
        Some(s) if s.starts_with("application/x-www-form-urlencoded") => {}
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
            req.headers_mut()
                .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        }
    }
    next.run(req).await
}

pub fn router(state: AppState) -> Router {
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    Router::new()
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
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024)) // 100 MB
        .layer(trace_layer)
        .with_state(state)
}
