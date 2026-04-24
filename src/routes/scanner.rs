use axum::{
    Json,
    extract::{Query, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::batch::now_iso;
use crate::state::AppState;

pub async fn heartbeat() -> Json<Value> {
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

pub async fn device(Json(_payload): Json<DevicePayload>) -> Json<Value> {
    Json(json!({ "system_time": now_iso(), "server_version": "2.6.0.4" }))
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct AuthQuery {
    pub auth_token: Option<String>,
}

pub async fn get_authorization(Query(_q): Query<AuthQuery>) -> Json<Value> {
    Json(json!({ "auth_type": "none", "auth_token": "" }))
}

pub async fn post_authorization(State(state): State<AppState>) -> Json<Value> {
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
    Json(state.config.jobs[q.job_id].scan_settings.clone())
}

pub async fn delete_accesstoken() -> Json<Value> {
    Json(json!({ "CharSet": null, "Parameters": [], "MediaType": "application/json" }))
}
