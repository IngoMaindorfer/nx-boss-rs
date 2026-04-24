use axum::{
    Json,
    extract::{Query, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, info};

use crate::batch::now_iso;
use crate::state::AppState;

pub async fn heartbeat() -> Json<Value> {
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

pub async fn device(Json(payload): Json<DevicePayload>) -> Json<Value> {
    info!(
        scanner_name = %payload.scanner_name,
        scanner_model = %payload.scanner_model,
        scanner_ip = %payload.scanner_ip,
        "scanner registered"
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
    let job_names: Vec<&str> = state
        .config
        .jobs
        .iter()
        .filter_map(|j| j.job_info["name"].as_str())
        .collect();
    info!(jobs = ?job_names, "scanner fetched job list");
    let job_info: Vec<&Value> = state.config.jobs.iter().map(|j| &j.job_info).collect();
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
) -> Json<Value> {
    let job_name = state.config.jobs[q.job_id].job_info["name"]
        .as_str()
        .unwrap_or("?");
    debug!(job_id = q.job_id, job_name, "scansetting requested");
    Json(state.config.jobs[q.job_id].scan_settings.clone())
}

pub async fn delete_accesstoken() -> Json<Value> {
    info!("scanner logged out");
    Json(json!({ "CharSet": null, "Parameters": [], "MediaType": "application/json" }))
}
