use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use crate::batch::now_iso;
use crate::state::AppState;

pub async fn heartbeat(State(state): State<AppState>) -> Json<Value> {
    state.record_ping();
    debug!("heartbeat");
    Json(json!({ "system_time": now_iso() }))
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct DevicePayload {
    pub call_timing: String,
    pub scanner_ip: String,
    pub scanner_mac: String,
    pub scanner_model: String,
    pub scanner_name: String,
    pub scanner_port: String,
    pub scanner_protocol: String,
    pub serial_no: String,
}

pub async fn device(
    State(state): State<AppState>,
    Json(payload): Json<DevicePayload>,
) -> Json<Value> {
    info!(
        scanner_name = %payload.scanner_name,
        scanner_model = %payload.scanner_model,
        serial_no = %payload.serial_no,
        scanner_ip = %payload.scanner_ip,
        "scanner registered"
    );
    state.set_scanner_info(
        payload.scanner_name,
        payload.scanner_model,
        payload.serial_no,
    );
    Json(json!({ "system_time": now_iso(), "server_version": "2.6.0.4" }))
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct AuthQuery {
    pub auth_token: Option<String>,
}

pub async fn get_authorization(Query(_q): Query<AuthQuery>) -> Json<Value> {
    debug!("get_authorization");
    Json(json!({ "auth_type": "none", "auth_token": "" }))
}

pub async fn post_authorization(State(state): State<AppState>) -> Json<Value> {
    let jobs = state.jobs.lock().unwrap();
    let job_names: Vec<&str> = jobs
        .iter()
        .filter_map(|j| j.job_info["name"].as_str())
        .collect();
    info!(jobs = ?job_names, "scanner fetched job list");
    let job_info: Vec<&Value> = jobs.iter().map(|j| &j.job_info).collect();
    Json(json!({
        "access_token": "unused",
        "token_type": "bearer",
        "job_group_name": "nx-boss",
        "job_info": job_info,
    }))
}

#[derive(Deserialize)]
pub struct ScanSettingQuery {
    pub job_id: usize,
}

pub async fn scansetting(
    Query(q): Query<ScanSettingQuery>,
    State(state): State<AppState>,
) -> Response {
    let jobs = state.jobs.lock().unwrap();
    let Some(job) = jobs.get(q.job_id) else {
        warn!(job_id = q.job_id, "scansetting: job_id out of range");
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "job not found" })),
        )
            .into_response();
    };
    let job_name = job.job_info["name"].as_str().unwrap_or("?");
    debug!(job_id = q.job_id, job_name, "scansetting requested");
    Json(job.scan_settings.clone()).into_response()
}

pub async fn delete_accesstoken() -> Json<Value> {
    info!("scanner logged out");
    Json(json!({ "CharSet": null, "Parameters": [], "MediaType": "application/json" }))
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use serde_json::json;

    use crate::config::{Config, Job};
    use crate::routes::router;
    use crate::state::AppState;

    fn test_server_with_job() -> TestServer {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({
                    "name": "TestJob", "job_id": 0, "color": "#4D4D4D",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: json!({ "parameters": {} }),
            }],
            retention: Default::default(),
        };
        TestServer::new(router(AppState::new(config)))
    }

    fn empty_server() -> TestServer {
        let config = Config {
            jobs: vec![],
            retention: Default::default(),
        };
        TestServer::new(router(AppState::new(config)))
    }

    #[tokio::test]
    async fn test_heartbeat_returns_system_time() {
        let server = test_server_with_job();
        let resp = server.get("/NmWebService/heartbeat").await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.json::<serde_json::Value>()["system_time"].is_string());
    }

    #[tokio::test]
    async fn test_scansetting_valid_job_id() {
        let server = test_server_with_job();
        let resp = server.get("/NmWebService/scansetting?job_id=0").await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_scansetting_out_of_bounds_returns_404() {
        let server = test_server_with_job();
        let resp = server.get("/NmWebService/scansetting?job_id=999").await;
        assert_eq!(resp.status_code(), 404);
    }

    #[tokio::test]
    async fn test_scansetting_out_of_bounds_empty_jobs() {
        let server = empty_server();
        let resp = server.get("/NmWebService/scansetting?job_id=0").await;
        assert_eq!(resp.status_code(), 404);
    }
}
