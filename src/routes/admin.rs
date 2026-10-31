use askama::Template;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ui::render;
use crate::state::AppState;

#[derive(Template)]
#[template(path = "admin_login.html")]
struct LoginTpl {
    lang: &'static str,
}

#[derive(Template)]
#[template(path = "admin_setup.html")]
struct SetupTpl {
    lang: &'static str,
}

#[derive(Template)]
#[template(path = "account.html")]
struct AccountTpl {
    username: String,
    lang: &'static str,
    passkey_count: usize,
    passkeys: Vec<crate::admin_auth::PasskeyView>,
    devices: Vec<crate::trusted_devices::DeviceView>,
}

pub(super) fn secure_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        == Some("https")
        && origin == format!("https://{host}")
}

pub(super) fn session_cookie(token: &str) -> String {
    format!("__Host-nx-admin={token}; Path=/; Secure; HttpOnly; SameSite=Strict")
}

pub(super) fn clear_trust_cookie() -> &'static str {
    "__Host-nx-trust=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0"
}
fn trust_cookie(token: &str) -> String {
    format!(
        "__Host-nx-trust={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={}",
        crate::trusted_devices::MAX_AGE
    )
}

pub async fn login_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    render(LoginTpl {
        lang: super::request_lang(&state, &headers),
    })
}

#[derive(Deserialize)]
pub struct PasskeySetupStart {
    #[serde(default)]
    name: String,
    token: String,
    username: String,
}

#[derive(Deserialize)]
pub struct PasskeyFinish {
    flow: String,
    credential: Value,
    #[serde(default)]
    trust_device: bool,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    token: String,
}

fn passkey_challenge(result: anyhow::Result<(String, Value)>) -> Response {
    match result {
        Ok((flow, challenge)) => {
            Json(json!({ "flow": flow, "challenge": challenge })).into_response()
        }
        Err(error) if error.is::<crate::admin_auth::LoginRateLimited>() => {
            StatusCode::TOO_MANY_REQUESTS.into_response()
        }
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub async fn passkey_setup_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<PasskeySetupStart>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.token.len() > 128 || form.username.len() > 64 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    passkey_challenge(auth.start_passkey_setup(&form.token, &form.username, &form.name))
}

pub async fn passkey_setup_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<PasskeyFinish>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.flow.len() != 64 || form.token.len() > 128 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match auth.finish_passkey_setup(&form.flow, &form.token, form.credential) {
        Ok(token) => (
            [(header::SET_COOKIE, session_cookie(&token))],
            Json(json!({ "ok": true })),
        )
            .into_response(),
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub async fn passkey_login_start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Only Caddy reaches this listener; Caddy supplies the actual peer in XFF.
    let client = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .and_then(|v| v.trim().parse().ok());
    passkey_challenge(auth.start_passkey_login(client))
}

pub async fn passkey_login_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<PasskeyFinish>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.flow.len() != 64 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match auth.finish_passkey_authentication(&form.flow, None, form.credential) {
        Ok(Some(token)) => {
            let mut response = Json(json!({"ok":true})).into_response();
            if form.trust_device {
                match auth.trust_device(&token, &form.device_name) {
                    Ok(trust) => {
                        response.headers_mut().append(
                            header::SET_COOKIE,
                            trust_cookie(&trust).parse().expect("trust cookie"),
                        );
                    }
                    Err(_) => {
                        auth.logout(&token);
                        return StatusCode::CONFLICT.into_response();
                    }
                }
            }
            response.headers_mut().append(
                header::SET_COOKIE,
                session_cookie(&token).parse().expect("session cookie"),
            );
            response
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub async fn passkey_stepup_start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session) = super::admin_cookie(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    passkey_challenge(auth.start_passkey_stepup(session))
}

pub async fn passkey_stepup_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<PasskeyFinish>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session) = super::admin_cookie(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match auth.finish_passkey_authentication(&form.flow, Some(session), form.credential) {
        Ok(None) => Json(json!({ "ok": true })).into_response(),
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub async fn passkey_add_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<PasskeyName>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session) = super::admin_cookie(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    passkey_challenge(auth.start_passkey_add(session, &form.name))
}

pub async fn passkey_add_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<PasskeyFinish>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session) = super::admin_cookie(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match auth.finish_passkey_add(&form.flow, session, form.credential) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub async fn logout_post(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = state.admin_auth.as_ref().map(|auth| {
        auth.logout_with_trust(
            super::admin_cookie(&headers),
            super::cookie_value(&headers, "__Host-nx-trust"),
        )
    });
    let mut response = if matches!(result, Some(Ok(()))) {
        Redirect::to("/login").into_response()
    } else {
        tracing::warn!(
            event = "admin.logout_failed",
            error_class = "storage",
            "device revocation could not be persisted"
        );
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    };
    response.headers_mut().append(
        header::SET_COOKIE,
        "__Host-nx-admin=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0"
            .parse()
            .expect("cookie"),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        clear_trust_cookie().parse().expect("cookie"),
    );
    response
}

pub async fn setup_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if state
        .admin_auth
        .as_ref()
        .is_some_and(|auth| auth.configured())
    {
        return Redirect::to("/").into_response();
    }
    render(SetupTpl {
        lang: super::request_lang(&state, &headers),
    })
}

pub async fn account_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    render(AccountTpl {
        username: auth.username().unwrap_or_default(),
        lang: super::request_lang(&state, &headers),
        passkey_count: auth.passkey_count(),
        passkeys: auth.passkeys(),
        devices: auth.trusted_devices(super::cookie_value(&headers, "__Host-nx-trust")),
    })
}

#[derive(Deserialize)]
pub struct PasskeyName {
    #[serde(default)]
    name: String,
}
#[derive(Deserialize)]
pub struct PasskeyRemove {
    key_id: String,
}
pub async fn passkey_remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<PasskeyRemove>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.key_id.len() > 4096 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session) = super::admin_cookie(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match auth.remove_passkey(session, &form.key_id) {
        Ok(true) => {
            let mut response = Json(json!({"ok":true})).into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                clear_trust_cookie().parse().expect("cookie"),
            );
            response
        }
        Ok(false) => StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}

#[derive(Deserialize)]
pub struct TrustedRemove {
    device_id: String,
}
pub async fn trusted_remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<TrustedRemove>,
) -> Response {
    if form.device_id.len() != 64 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    revoke_trust(&state, &headers, Some(&form.device_id))
}
pub async fn trusted_remove_all(State(state): State<AppState>, headers: HeaderMap) -> Response {
    revoke_trust(&state, &headers, None)
}
fn revoke_trust(state: &AppState, headers: &HeaderMap, id: Option<&str>) -> Response {
    if !secure_origin(headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session) = super::admin_cookie(headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let current = auth
        .trusted_devices(super::cookie_value(headers, "__Host-nx-trust"))
        .into_iter()
        .find(|d| d.current)
        .map(|d| d.id);
    match auth.revoke_trusted_devices(session, id) {
        Ok(true) => {
            let mut response = Json(json!({"ok":true})).into_response();
            if id.is_none() || current.as_deref() == id {
                response.headers_mut().append(
                    header::SET_COOKIE,
                    clear_trust_cookie().parse().expect("cookie"),
                );
            }
            response
        }
        Ok(false) => StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}
