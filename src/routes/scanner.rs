use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
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

pub async fn device(
    State(state): State<AppState>,
    Json(payload): Json<DevicePayload>,
) -> Json<Value> {
    info!(event = "scanner.registered", "scanner registered");
    state.scanner.set_info(
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

pub async fn get_authorization(
    Query(_q): Query<AuthQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if (state.scanner_auth_mode.requires_login() || state.scanner_auth_experiment.is_some())
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
    let auth_type = state
        .scanner_auth_experiment
        .unwrap_or_else(|| state.scanner_auth_mode.wire_type());
    Json(json!({ "auth_type": auth_type, "auth_token": "" })).into_response()
}

struct AuthProbeMetadata {
    auth_scheme: &'static str,
    body_format: &'static str,
    known_fields: String,
    unknown_field_count: usize,
    schema_fields: String,
    schema_types: String,
    auth_info_user_type: &'static str,
    auth_info_pass_type: &'static str,
}

fn json_value_type(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn auth_scheme(headers: &HeaderMap) -> &'static str {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_whitespace().next())
        .map(|scheme| {
            if scheme.eq_ignore_ascii_case("basic") {
                "basic"
            } else if scheme.eq_ignore_ascii_case("bearer") {
                "bearer"
            } else if scheme.eq_ignore_ascii_case("digest") {
                "digest"
            } else {
                "other"
            }
        })
        .unwrap_or("none")
}

fn canonical_probe_field(key: &str) -> Option<&'static str> {
    match key.to_ascii_lowercase().as_str() {
        "auth_type" => Some("auth_type"),
        "auth_token" => Some("auth_token"),
        "batch_id" => Some("batch_id"),
        "credentials" => Some("credentials"),
        "job_group_code" => Some("job_group_code"),
        "job_id" => Some("job_id"),
        "login_id" | "loginid" | "loginname" | "login_name" => Some("login_id"),
        "password" | "passwd" | "pwd" | "pass" | "loginpassword" | "login_password" => {
            Some("password")
        }
        "pin" => Some("pin"),
        "scanner_info" => Some("scanner_info"),
        "user" | "user_id" | "userid" | "user_name" | "username" => Some("user_name"),
        _ => None,
    }
}

// Temporary device experiment: reveal only schema-like top-level keys. User-supplied
// strings can be JSON keys, so reject plain lowercase words and keys containing
// any scalar request value. Never record the values themselves.
fn safe_unknown_top_level_fields(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return String::new();
    };
    let mut scalar_values = Vec::new();
    let mut pending = vec![(None, value)];
    while let Some((field, current)) = pending.pop() {
        match current {
            Value::String(text) if field != Some("auth_type") => scalar_values.push(
                text.chars()
                    .filter(|ch| ch.is_ascii_alphanumeric())
                    .map(|ch| ch.to_ascii_lowercase())
                    .collect::<String>(),
            ),
            Value::Number(number) => scalar_values.push(number.to_string()),
            Value::Object(map) => {
                pending.extend(map.iter().map(|(key, value)| (Some(key.as_str()), value)))
            }
            Value::Array(array) => pending.extend(array.iter().map(|value| (None, value))),
            _ => {}
        }
    }
    object
        .keys()
        .filter(|key| {
            canonical_probe_field(key).is_none()
                && (6..=32).contains(&key.len())
                && key.starts_with(|c: char| c.is_ascii_lowercase())
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && key.chars().any(|c| c.is_ascii_uppercase() || c == '_')
                && !scalar_values.iter().any(|value| {
                    let normalized_key = key
                        .chars()
                        .filter(|ch| ch.is_ascii_alphanumeric())
                        .map(|ch| ch.to_ascii_lowercase())
                        .collect::<String>();
                    value.len() >= 3 && normalized_key.contains(value)
                })
        })
        .cloned()
        .collect::<Vec<_>>()
        .join(",")
}

fn json_field_names(value: &Value) -> (String, usize) {
    let mut fields = BTreeSet::new();
    let mut unknown_field_count = 0usize;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    let canonical = canonical_probe_field(key);
                    if let Some(field) = canonical {
                        fields.insert(field);
                    } else {
                        unknown_field_count = unknown_field_count.saturating_add(1);
                    }
                    pending.push(value);
                }
            }
            Value::Array(array) => pending.extend(array),
            _ => {}
        }
    }
    (
        fields.into_iter().collect::<Vec<_>>().join(","),
        unknown_field_count,
    )
}

fn auth_probe_metadata(headers: &HeaderMap, body: &[u8]) -> AuthProbeMetadata {
    let (
        body_format,
        known_fields,
        unknown_field_count,
        schema_fields,
        schema_types,
        auth_info_user_type,
        auth_info_pass_type,
    ) = if body.is_empty() {
        (
            "empty",
            String::new(),
            0,
            String::new(),
            String::new(),
            "missing",
            "missing",
        )
    } else if let Ok(value) = serde_json::from_slice::<Value>(body) {
        let body_format = if value.is_object() {
            "json_object"
        } else {
            "json_other"
        };
        let (known_fields, unknown_field_count) = json_field_names(&value);
        let schema_fields = safe_unknown_top_level_fields(&value);
        let schema_types = schema_fields
            .split(',')
            .filter(|key| !key.is_empty())
            .map(|key| format!("{key}:{}", json_value_type(value.get(key))))
            .collect::<Vec<_>>()
            .join(",");
        let auth_info_user_type = json_value_type(value.get("auth_info_user"));
        let auth_info_pass_type = json_value_type(value.get("auth_info_pass"));
        (
            body_format,
            known_fields,
            unknown_field_count,
            schema_fields,
            schema_types,
            auth_info_user_type,
            auth_info_pass_type,
        )
    } else {
        (
            "non_json",
            String::new(),
            0,
            String::new(),
            String::new(),
            "missing",
            "missing",
        )
    };

    AuthProbeMetadata {
        auth_scheme: auth_scheme(headers),
        body_format,
        known_fields,
        unknown_field_count,
        schema_fields,
        schema_types,
        auth_info_user_type,
        auth_info_pass_type,
    }
}

fn auth_probe_enabled() -> bool {
    std::env::var("NX_BOSS_SCANNER_AUTH_PROBE").as_deref() == Ok("1")
}

pub(super) fn log_scan_json_probe(stage: &'static str, headers: &HeaderMap, value: &Value) {
    if !auth_probe_enabled() {
        return;
    }
    let (known_fields, unknown_field_count) = json_field_names(value);
    info!(
        event = "scanner.scan_probe",
        scan_stage = stage,
        auth_scheme = auth_scheme(headers),
        body_format = if value.is_object() { "json_object" } else { "json_other" },
        known_fields = %known_fields,
        unknown_field_count,
        "scanner scan metadata observed"
    );
}

pub(super) fn log_scan_bytes_probe(stage: &'static str, headers: &HeaderMap, body: &[u8]) {
    if !auth_probe_enabled() {
        return;
    }
    let metadata = auth_probe_metadata(headers, body);
    info!(
        event = "scanner.scan_probe",
        scan_stage = stage,
        auth_scheme = metadata.auth_scheme,
        body_format = metadata.body_format,
        known_fields = %metadata.known_fields,
        unknown_field_count = metadata.unknown_field_count,
        "scanner scan metadata observed"
    );
}

pub async fn post_authorization(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if auth_probe_enabled() {
        let metadata = auth_probe_metadata(&headers, &body);
        let (auth_info_user_match, auth_info_pass_match) =
            state.scanner_auth_mode.probe_matches(&body);
        info!(
            event = "scanner.auth_probe",
            auth_scheme = metadata.auth_scheme,
            body_format = metadata.body_format,
            known_fields = %metadata.known_fields,
            unknown_field_count = metadata.unknown_field_count,
            schema_fields = %metadata.schema_fields,
            schema_types = %metadata.schema_types,
            auth_info_user_type = metadata.auth_info_user_type,
            auth_info_pass_type = metadata.auth_info_pass_type,
            auth_info_user_match,
            auth_info_pass_match,
            "scanner authorization metadata observed"
        );
    }
    if state.scanner_auth_experiment.is_some() {
        warn!(
            event = "scanner.diagnostic_login_rejected",
            "diagnostic login rejected without jobs or token"
        );
        return StatusCode::UNAUTHORIZED.into_response();
    }
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
    headers: HeaderMap,
) -> Response {
    log_scan_bytes_probe("scansetting", &headers, &[]);
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
    use axum::http::{HeaderMap, HeaderValue, header};
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
            ..Default::default()
        };
        TestServer::new(router(AppState::new(config)))
    }

    fn empty_server() -> TestServer {
        let config = Config {
            jobs: vec![],
            ..Default::default()
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
        let server = TestServer::new(router(AppState::new(config).with_scanner_auth_mode(mode)));

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
        let server = TestServer::new(router(state.clone()));
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
    async fn authorization_probe_limits_request_body() {
        let server = test_server_with_job();
        let body = vec![b'x'; 16 * 1024 + 1];
        let resp = server
            .post("/NmWebService/authorization")
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 413);
    }

    #[test]
    fn auth_probe_reports_json_field_names_without_credential_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic PRIVATE_BASIC_CREDENTIAL"),
        );
        let body = br#"{"auth_type":"user","credentials":{"user_name":"PRIVATE_USER","password":"PRIVATE_PASSWORD"},"PRIVATE_FIELD_NAME":"PRIVATE_VALUE"}"#;
        let metadata = super::auth_probe_metadata(&headers, body);
        assert_eq!(metadata.auth_scheme, "basic");
        assert_eq!(metadata.body_format, "json_object");
        assert!(metadata.known_fields.contains("user_name"));
        assert!(metadata.known_fields.contains("password"));
        assert_eq!(metadata.unknown_field_count, 1);
        for secret in [
            "PRIVATE_BASIC_CREDENTIAL",
            "PRIVATE_USER",
            "PRIVATE_PASSWORD",
            "PRIVATE_FIELD_NAME",
            "PRIVATE_VALUE",
        ] {
            assert!(!metadata.known_fields.contains(secret));
        }
        assert_eq!(metadata.auth_info_user_type, "missing");
        assert_eq!(metadata.auth_info_pass_type, "missing");
    }

    #[test]
    fn job_code_probe_reports_only_safe_field_names_and_types() {
        let body = br#"{"auth_type":"job","auth_info_job_group_code":"SECRET-CODE","auth_timing":1,"secretCode":"SECRET-CODE","SECRET-CODE":"SECRET-CODE","pin_1234":1234}"#;
        let metadata = super::auth_probe_metadata(&HeaderMap::new(), body);
        assert!(
            metadata
                .schema_types
                .contains("auth_info_job_group_code:string")
        );
        assert!(metadata.schema_types.contains("auth_timing:number"));
        assert!(!metadata.schema_types.contains("SECRET-CODE"));
        assert!(!metadata.schema_types.contains("secretCode"));
        assert!(!metadata.schema_types.contains("pin_1234"));
    }

    #[tokio::test]
    async fn measured_job_offer_is_diagnostic_and_fail_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pepper = [0x42; 32];
        let config = crate::config::ScannerAuthConfig {
            mode: "userpass".into(),
            username: Some("scanner".into()),
            password_hash: Some(
                crate::scanner_auth::hash_password("a sufficiently long password", &pepper)
                    .unwrap(),
            ),
        };
        let state = AppState::new(Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({"name":"TestJob","job_id":0,"color":"#4D4D4D","type":0,"job_setting":{},"hierarchy_list":null}),
                scan_settings: json!({"parameters":{}}),
            }],
            ..Default::default()
        })
        .with_scanner_auth_mode(
            crate::scanner_auth::ScannerAuthMode::from_hashed_config(&config, &pepper).unwrap(),
        )
        .with_scanner_auth_experiment("job");
        let server = TestServer::new(crate::routes::scanner_router(state.clone()));
        assert_eq!(
            server
                .get("/NmWebService/authorization")
                .await
                .status_code(),
            403
        );
        let offer = server
            .get("/NmWebService/authorization")
            .add_header("x-forwarded-proto", "https")
            .await;
        assert_eq!(offer.json::<serde_json::Value>()["auth_type"], "job");
        let login = server
            .post("/NmWebService/authorization")
            .add_header("x-forwarded-proto", "https")
            .json(&json!({"auth_info_user":"scanner","auth_info_pass":"a sufficiently long password"}))
            .await;
        assert_eq!(login.status_code(), 401);
        assert!(!login.text().contains("job_info"));
        assert!(!login.text().contains("access_token"));
        assert_eq!(
            server
                .get("/NmWebService/scansetting?job_id=0")
                .add_header("x-forwarded-proto", "https")
                .await
                .status_code(),
            401
        );
        state
            .scanner_auth_mode
            .replace(crate::scanner_auth::ScannerAuthMode::None);
        assert_eq!(
            server
                .get("/NmWebService/scansetting?job_id=0")
                .add_header("x-forwarded-proto", "https")
                .await
                .status_code(),
            401
        );
    }

    #[test]
    fn auth_probe_classifies_scanner_credential_types_without_values() {
        let body = br#"{"auth_type":"userpass","auth_info_user":"PRIVATE_USER","auth_info_pass":"PRIVATE_PASSWORD","auth_timing":1}"#;
        let metadata = super::auth_probe_metadata(&HeaderMap::new(), body);
        assert_eq!(metadata.auth_info_user_type, "string");
        assert_eq!(metadata.auth_info_pass_type, "string");
        assert!(!metadata.schema_fields.contains("PRIVATE"));
        let body = br#"{"auth_info_user":{"nested":"PRIVATE_USER"},"auth_info_pass":null}"#;
        let metadata = super::auth_probe_metadata(&HeaderMap::new(), body);
        assert_eq!(metadata.auth_info_user_type, "object");
        assert_eq!(metadata.auth_info_pass_type, "null");
    }

    #[test]
    fn auth_probe_distinguishes_non_json_and_missing_basic_header() {
        let metadata =
            super::auth_probe_metadata(&HeaderMap::new(), b"username=alice&password=secret");
        assert_eq!(metadata.auth_scheme, "none");
        assert_eq!(metadata.body_format, "non_json");
        assert!(metadata.known_fields.is_empty());
    }

    #[test]
    fn auth_probe_schema_hides_values_even_if_they_are_json_keys() {
        let body = json!({
            "auth_type": "userpass",
            "scanner_info": {"scanner_ip": "10.0.0.1"},
            "accountName": "PRIVATE_USER",
            "accountSecret": "PRIVATE_PASSWORD",
            "PRIVATE_USER": "PRIVATE_USER",
            "PRIVATE_FIELD_NAME": "anything",
            "longusername": "longusername",
            "short": "value",
        });
        assert_eq!(
            super::safe_unknown_top_level_fields(&body),
            "accountName,accountSecret"
        );
    }
}
