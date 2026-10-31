use askama::Template;
use axum::{
    Form, Json,
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
    error: bool,
    lang: &'static str,
    passkey_enabled: bool,
}

#[derive(Template)]
#[template(path = "admin_setup.html")]
struct SetupTpl {
    error: bool,
    lang: &'static str,
}

#[derive(Template)]
#[template(path = "admin_setup_confirm.html")]
struct SetupConfirmTpl {
    token: String,
    manual_secret: String,
    otpauth_url: String,
    error: bool,
    lang: &'static str,
}

#[derive(Template)]
#[template(path = "account.html")]
struct AccountTpl {
    username: String,
    error: bool,
    lang: &'static str,
    passkey_enabled: bool,
    passkey_count: usize,
}

#[derive(Template)]
#[template(path = "account_totp.html")]
struct AccountTotpTpl {
    manual_secret: String,
    otpauth_url: String,
    error: bool,
    lang: &'static str,
}

#[derive(Deserialize)]
pub struct AccountForm {
    password: String,
    code: String,
    username: String,
    new_password: String,
}

#[derive(Deserialize)]
pub struct ReauthForm {
    password: String,
    code: String,
}

#[derive(Deserialize)]
pub struct TotpCompleteForm {
    code: String,
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
    code: String,
}
#[derive(Deserialize)]
pub struct PrepareForm {
    token: String,
    username: String,
}
#[derive(Deserialize)]
pub struct CompleteForm {
    token: String,
    password: String,
    code: String,
}

fn preferred_lang(headers: &HeaderMap) -> &'static str {
    if headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|cookie| cookie.split(';').any(|part| part.trim() == "ui_lang=en"))
    {
        "en"
    } else {
        "de"
    }
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

fn session_cookie(token: &str) -> String {
    format!("__Host-nx-admin={token}; Path=/; Secure; HttpOnly; SameSite=Strict")
}

pub async fn login_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    render(LoginTpl {
        error: false,
        lang: preferred_lang(&headers),
        passkey_enabled: state
            .admin_auth
            .as_ref()
            .is_some_and(|auth| auth.passkey_configured()),
    })
}

#[derive(Deserialize)]
pub struct PasskeySetupStart {
    token: String,
    username: String,
}

#[derive(Deserialize)]
pub struct PasskeyFinish {
    flow: String,
    credential: Value,
    #[serde(default)]
    token: String,
}

fn passkey_challenge(result: anyhow::Result<(String, Value)>) -> Response {
    match result {
        Ok((flow, challenge)) => {
            Json(json!({ "flow": flow, "challenge": challenge })).into_response()
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
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    passkey_challenge(auth.start_passkey_setup(&form.token, &form.username))
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
    let Some(auth) = state.admin_auth else {
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
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    passkey_challenge(auth.start_passkey_login())
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
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match auth.finish_passkey_authentication(&form.flow, None, form.credential) {
        Ok(Some(token)) => (
            [(header::SET_COOKIE, session_cookie(&token))],
            Json(json!({ "ok": true })),
        )
            .into_response(),
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub async fn passkey_stepup_start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth else {
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
    let Some(auth) = state.admin_auth else {
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

pub async fn passkey_add_start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session) = super::admin_cookie(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    passkey_challenge(auth.start_passkey_add(session))
}

pub async fn passkey_add_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<PasskeyFinish>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth else {
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

pub async fn login_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.username.len() > 64 || form.password.len() > 512 || form.code.len() > 16 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let passkey_enabled = auth.passkey_configured();
    let result =
        tokio::task::spawn_blocking(move || auth.login(&form.username, &form.password, &form.code))
            .await;
    match result {
        Ok(Some(token)) => (
            [(header::SET_COOKIE, session_cookie(&token))],
            Redirect::to("/"),
        )
            .into_response(),
        _ => render(LoginTpl {
            error: true,
            lang: preferred_lang(&headers),
            passkey_enabled,
        }),
    }
}

pub async fn logout_post(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(auth) = state.admin_auth
        && let Some(token) = super::admin_cookie(&headers)
    {
        auth.logout(token);
    }
    (
        [(
            header::SET_COOKIE,
            "__Host-nx-admin=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0",
        )],
        Redirect::to("/login"),
    )
        .into_response()
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
        error: false,
        lang: preferred_lang(&headers),
    })
}

pub async fn setup_prepare(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PrepareForm>,
) -> Response {
    if std::env::var("NX_BOSS_LEGACY_ADMIN_SETUP").as_deref() != Ok("1") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match auth.prepare_setup(&form.token, &form.username) {
        Ok(preview) => render(SetupConfirmTpl {
            token: form.token,
            manual_secret: preview.manual_secret,
            otpauth_url: preview.otpauth_url,
            error: false,
            lang: preferred_lang(&headers),
        }),
        Err(_) => render(SetupTpl {
            error: true,
            lang: preferred_lang(&headers),
        }),
    }
}

pub async fn setup_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CompleteForm>,
) -> Response {
    if std::env::var("NX_BOSS_LEGACY_ADMIN_SETUP").as_deref() != Ok("1") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.password.len() > 512 || form.code.len() > 16 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = tokio::task::spawn_blocking(move || {
        auth.complete_setup(&form.token, &form.password, &form.code)
    })
    .await;
    match result {
        Ok(Ok(token)) => (
            [(header::SET_COOKIE, session_cookie(&token))],
            Redirect::to("/"),
        )
            .into_response(),
        _ => render(SetupTpl {
            error: true,
            lang: preferred_lang(&headers),
        }),
    }
}

pub async fn account_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    render(AccountTpl {
        username: auth.username().unwrap_or_default(),
        error: false,
        lang: preferred_lang(&headers),
        passkey_enabled: auth.passkey_configured(),
        passkey_count: auth.passkey_count(),
    })
}

pub async fn account_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AccountForm>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.password.len() > 512 || form.new_password.len() > 512 || form.code.len() > 16 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let display_name = auth.username().unwrap_or_default();
    match tokio::task::spawn_blocking(move || {
        auth.update_account(
            &form.password,
            &form.code,
            &form.username,
            &form.new_password,
        )
    })
    .await
    {
        Ok(Ok(())) => (
            [(header::SET_COOKIE, expired_session_cookie())],
            Redirect::to("/login"),
        )
            .into_response(),
        _ => render(AccountTpl {
            username: display_name,
            error: true,
            lang: preferred_lang(&headers),
            passkey_enabled: false,
            passkey_count: 0,
        }),
    }
}

fn expired_session_cookie() -> &'static str {
    "__Host-nx-admin=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0"
}

pub async fn totp_prepare(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ReauthForm>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.password.len() > 512 || form.code.len() > 16 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match tokio::task::spawn_blocking(move || {
        auth.prepare_totp_rotation(&form.password, &form.code)
    })
    .await
    {
        Ok(Ok(preview)) => render(AccountTotpTpl {
            manual_secret: preview.manual_secret,
            otpauth_url: preview.otpauth_url,
            error: false,
            lang: preferred_lang(&headers),
        }),
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub async fn totp_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<TotpCompleteForm>,
) -> Response {
    if !secure_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if form.code.len() > 16 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(auth) = state.admin_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match auth.complete_totp_rotation(&form.code) {
        Ok(token) => (
            [(header::SET_COOKIE, session_cookie(&token))],
            Redirect::to("/account"),
        )
            .into_response(),
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}
