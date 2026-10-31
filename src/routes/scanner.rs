use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use crate::batch::now_iso;
use crate::lock;
use crate::state::AppState;

pub async fn heartbeat(State(state): State<AppState>) -> Json<Value> {
    state.scanner.record_ping();
    debug!(event = "scanner.heartbeat", "heartbeat");
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

pub async fn device(State(state): State<AppState>, Json(payload): Json<DevicePayload>) -> Response {
    if [
        &payload.scanner_name,
        &payload.scanner_model,
        &payload.serial_no,
    ]
    .iter()
    .any(|v| v.len() > 256)
        || [
            &payload.call_timing,
            &payload.scanner_ip,
            &payload.scanner_mac,
        ]
        .iter()
        .any(|v| v.len() > 64)
        || [&payload.scanner_port, &payload.scanner_protocol]
            .iter()
            .any(|v| v.len() > 16)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    info!(event = "scanner.registered", "scanner registered");
    state.scanner.set_info(
        payload.scanner_name,
        payload.scanner_model,
        payload.serial_no,
    );
    Json(json!({ "system_time": now_iso(), "server_version": "2.6.0.4" })).into_response()
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct AuthQuery {
    pub auth_token: Option<String>,
}

pub async fn get_authorization(
    Query(_q): Query<AuthQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if state.scanner_auth_mode.requires_login()
        && headers
            .get("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            != Some("https")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    debug!(
        event = "scanner.authorization_requested",
        "get_authorization"
    );
    let auth_type = state.scanner_auth_mode.wire_type();
    Json(json!({ "auth_type": auth_type, "auth_token": "" })).into_response()
}

pub async fn post_authorization(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let access_token = if state.scanner_auth_mode.requires_login() {
        if headers
            .get("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            != Some("https")
        {
            warn!(
                event = "scanner.insecure_login_rejected",
                "scanner login requires HTTPS"
            );
            return StatusCode::FORBIDDEN.into_response();
        }
        let Ok(permit) = state.auth_slots.clone().try_acquire_owned() else {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        };
        let auth = state.scanner_auth_mode.clone();
        let login = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            auth.login(&body)
        })
        .await;
        let Some(token) = login.ok().flatten() else {
            warn!(event = "scanner.login_rejected", "scanner login rejected");
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":"authentication failed"})),
            )
                .into_response();
        };
        info!(event = "scanner.login_succeeded", "scanner login succeeded");
        token
    } else {
        "unused".to_owned()
    };
    let jobs = lock!(state.jobs);
    info!(
        event = "scanner.jobs_sent",
        job_count = jobs.len(),
        "scanner fetched job list"
    );
    let job_info: Vec<&Value> = jobs.iter().map(|j| &j.job_info).collect();
    Json(json!({
        "access_token": access_token,
        "token_type": "bearer",
        "job_group_name": "nx-boss-rs",
        "job_info": job_info,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct ScanSettingQuery {
    pub job_id: usize,
}

pub async fn scansetting(
    Query(q): Query<ScanSettingQuery>,
    State(state): State<AppState>,
) -> Response {
    let jobs = lock!(state.jobs);
    let Some(job) = jobs
        .iter()
        .find(|j| j.job_info["job_id"].as_u64() == Some(q.job_id as u64))
    else {
        warn!(
            event = "scanner.unknown_job",
            job_id = q.job_id,
            "scansetting: job_id out of range"
        );
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "job not found" })),
        )
            .into_response();
    };
    debug!(
        event = "scanner.scansetting",
        job_id = q.job_id,
        "scansetting requested"
    );
    Json(job.scan_settings.clone()).into_response()
}

pub async fn delete_accesstoken(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    if let Some(token) = super::scanner_bearer(&headers) {
        state.scanner_auth_mode.revoke(token);
    }
    info!(event = "scanner.logged_out", "scanner logged out");
    Json(json!({ "CharSet": null, "Parameters": [], "MediaType": "application/json" }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::config::{Config, Job};
    use crate::routes::test_support::TestApp;
    use crate::state::AppState;

    fn test_server_with_job() -> TestApp {
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
            ..Default::default()
        };
        TestApp::new(AppState::new(config))
    }

    fn empty_server() -> TestApp {
        let config = Config {
            jobs: vec![],
            ..Default::default()
        };
        TestApp::new(AppState::new(config))
    }

    #[tokio::test]
    async fn device_metadata_and_request_body_are_bounded() {
        let server = empty_server();
        let fields = serde_json::json!({"call_timing":"startup","scanner_ip":"127.0.0.1","scanner_mac":"00:11:22:33:44:55","scanner_model":"test","scanner_name":"x".repeat(512),"scanner_port":"20448","scanner_protocol":"https","serial_no":"test"});
        assert_eq!(
            server
                .post("/NmWebService/device")
                .json(&fields)
                .await
                .status_code(),
            400
        );
        let mut huge = fields;
        huge["scanner_name"] = serde_json::Value::String("x".repeat(17000));
        assert_eq!(
            server
                .post("/NmWebService/device")
                .json(&huge)
                .await
                .status_code(),
            413
        );
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

    #[tokio::test]
    async fn userpass_requires_https_and_valid_credentials_then_enforces_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({"name":"ProtectedJob","job_id":0}),
                scan_settings: json!({}),
            }],
            ..Default::default()
        };
        let tmp_password = tempfile::tempdir().unwrap();
        let password_file = tmp_password.path().join("scanner-password");
        std::fs::write(&password_file, b"test-secret").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let mode = crate::scanner_auth::ScannerAuthMode::from_values(
            "userpass",
            Some("test"),
            Some(&password_file),
        )
        .unwrap();
        let server = TestApp::new(AppState::new(config).with_scanner_auth_mode(mode));

        let offer = server
            .get("/NmWebService/authorization")
            .add_header("x-forwarded-proto", "https")
            .await;
        assert_eq!(offer.status_code(), 200);
        assert_eq!(offer.json::<serde_json::Value>()["auth_type"], "userpass");

        let insecure = server
            .post("/NmWebService/authorization")
            .json(&json!({"auth_info_user":"test","auth_info_pass":"test-secret"}))
            .await;
        assert_eq!(insecure.status_code(), 403);

        let wrong = server
            .post("/NmWebService/authorization")
            .add_header("x-forwarded-proto", "https")
            .json(&json!({"auth_info_user":"test","auth_info_pass":"wrong"}))
            .await;
        assert_eq!(wrong.status_code(), 401);
        let body = wrong.json::<serde_json::Value>();
        assert!(body.get("job_info").is_none());
        assert!(body.get("access_token").is_none());

        let login = server
            .post("/NmWebService/authorization")
            .add_header("x-forwarded-proto", "https")
            .json(&json!({"auth_info_user":"test","auth_info_pass":"test-secret"}))
            .await;
        assert_eq!(login.status_code(), 200);
        let body = login.json::<serde_json::Value>();
        assert_eq!(body["job_info"].as_array().unwrap().len(), 1);
        assert_eq!(body["token_type"], "bearer");
        let token = body["access_token"].as_str().unwrap();
        assert_ne!(token, "unused");
        assert!(token.len() >= 32);

        assert_eq!(
            server
                .get("/NmWebService/scansetting?job_id=0")
                .await
                .status_code(),
            401
        );
        assert_eq!(
            server
                .post("/NmWebService/batch")
                .json(&json!({"job_id":0}))
                .await
                .status_code(),
            401
        );
        assert_eq!(server.post("/NmWebService/image").await.status_code(), 401);
        assert_eq!(
            server
                .put("/NmWebService/batch/not-a-batch")
                .await
                .status_code(),
            401
        );
        assert_eq!(server.get("/health").await.status_code(), 200);

        let valid = server
            .get("/NmWebService/scansetting?job_id=0")
            .add_header("authorization", format!("Bearer {token}"))
            .add_header("x-forwarded-proto", "https")
            .await;
        assert_eq!(valid.status_code(), 200);
        let logout = server
            .delete("/NmWebService/accesstoken")
            .add_header("authorization", format!("Bearer {token}"))
            .add_header("x-forwarded-proto", "https")
            .await;
        assert_eq!(logout.status_code(), 200);
        assert_eq!(
            server
                .get("/NmWebService/scansetting?job_id=0")
                .add_header("authorization", format!("Bearer {token}"))
                .await
                .status_code(),
            401
        );
    }

    #[tokio::test]
    async fn changing_scanner_mode_replaces_live_sessions() {
        let state = AppState::new(Config::default());
        let server = TestApp::new(state.clone());
        assert_eq!(
            server
                .get("/NmWebService/authorization")
                .await
                .json::<serde_json::Value>()["auth_type"],
            "none"
        );
        let pepper = [0x42; 32];
        let config = crate::config::ScannerAuthConfig {
            mode: "userpass".into(),
            username: Some("scanner".into()),
            password_hash: Some(
                crate::scanner_auth::hash_password("a sufficiently long password", &pepper)
                    .unwrap(),
            ),
        };
        state.scanner_auth_mode.replace(
            crate::scanner_auth::ScannerAuthMode::from_hashed_config(&config, &pepper).unwrap(),
        );
        assert_eq!(
            server
                .get("/NmWebService/authorization")
                .add_header("x-forwarded-proto", "https")
                .await
                .json::<serde_json::Value>()["auth_type"],
            "userpass"
        );
        let login = server.post("/NmWebService/authorization").add_header("x-forwarded-proto", "https").json(&json!({"auth_info_user":"scanner","auth_info_pass":"a sufficiently long password"})).await;
        assert_eq!(login.status_code(), 200);
        let token = login.json::<serde_json::Value>()["access_token"]
            .as_str()
            .unwrap()
            .to_owned();
        state.scanner_auth_mode.replace(
            crate::scanner_auth::ScannerAuthMode::from_hashed_config(&config, &pepper).unwrap(),
        );
        assert_eq!(
            server
                .get("/NmWebService/scansetting?job_id=0")
                .add_header("x-forwarded-proto", "https")
                .add_header("authorization", format!("Bearer {token}"))
                .await
                .status_code(),
            401
        );
    }

    #[tokio::test]
    async fn authorization_limits_request_body() {
        let server = test_server_with_job();
        let body = vec![b'x'; 16 * 1024 + 1];
        let resp = server
            .post("/NmWebService/authorization")
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 413);
    }
}
