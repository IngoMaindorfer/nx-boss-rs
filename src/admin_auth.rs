#[cfg(test)]
mod tests {
    use super::AdminAuth;

    #[test]
    fn passkey_setup_requires_private_token_and_never_configures_on_invalid_attestation() {
        let dir = tempfile::tempdir().unwrap();
        let auth =
            AdminAuth::open_with_origin(dir.path(), &[0x42; 32], "https://localhost").unwrap();
        assert!(auth.start_passkey_setup("wrong", "admin").is_err());
        let token = std::fs::read_to_string(dir.path().join("admin-setup-token")).unwrap();
        let (flow, _challenge) = auth.start_passkey_setup(&token, "admin").unwrap();
        assert!(
            auth.finish_passkey_setup(&flow, &token, serde_json::json!({}))
                .is_err()
        );
        assert!(
            auth.finish_passkey_setup(&flow, &token, serde_json::json!({}))
                .is_err()
        );
        assert!(!auth.configured());
    }

    #[test]
    fn bootstrap_requires_private_token_and_totp_then_login_rejects_replay() {
        let dir = tempfile::tempdir().unwrap();
        let pepper = [0x5au8; 32];
        let auth = AdminAuth::open(dir.path(), &pepper).unwrap();
        assert!(!auth.configured());
        let token = std::fs::read_to_string(dir.path().join("admin-setup-token")).unwrap();
        assert!(auth.prepare_setup("invalid", "owner").is_err());
        let preview = auth.prepare_setup(token.trim(), "owner").unwrap();
        let secret = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &preview.manual_secret,
        )
        .unwrap();
        let first_code = super::totp(&secret)
            .unwrap()
            .generate(1_700_000_000)
            .to_string();
        assert!(
            auth.complete_setup_at(
                token.trim(),
                "a sufficiently long password",
                "wrong",
                1_700_000_000
            )
            .is_err()
        );
        auth.complete_setup_at(
            token.trim(),
            "a sufficiently long password",
            &first_code,
            1_700_000_000,
        )
        .unwrap();
        assert!(auth.configured());
        let stored = std::fs::read_to_string(dir.path().join("admin-auth.json")).unwrap();
        assert!(!stored.contains("a sufficiently long password"));
        assert!(
            auth.login_at(
                "owner",
                "a sufficiently long password",
                &first_code,
                1_700_000_000
            )
            .is_none()
        );
        let next_code = super::totp(&secret)
            .unwrap()
            .generate(1_700_000_030)
            .to_string();
        assert!(
            auth.login_at("owner", "wrong", &next_code, 1_700_000_030)
                .is_none()
        );
        let session = auth
            .login_at(
                "owner",
                "a sufficiently long password",
                &next_code,
                1_700_000_030,
            )
            .unwrap();
        assert!(auth.session_valid(&session));
        auth.logout(&session);
        assert!(!auth.session_valid(&session));
    }

    #[test]
    fn admin_can_change_account_and_rotate_totp_with_fresh_factors() {
        let dir = tempfile::tempdir().unwrap();
        let auth = AdminAuth::open(dir.path(), &[0x5a; 32]).unwrap();
        let token = std::fs::read_to_string(dir.path().join("admin-setup-token")).unwrap();
        let preview = auth.prepare_setup(&token, "owner").unwrap();
        let old_secret = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &preview.manual_secret,
        )
        .unwrap();
        let code = super::totp(&old_secret)
            .unwrap()
            .generate(1_700_000_000)
            .to_string();
        let session = auth
            .complete_setup_at(
                &token,
                "old sufficiently long password",
                &code,
                1_700_000_000,
            )
            .unwrap();
        let fresh = super::totp(&old_secret)
            .unwrap()
            .generate(1_700_000_030)
            .to_string();
        auth.update_account_at(
            "old sufficiently long password",
            &fresh,
            "newowner",
            "new sufficiently long password",
            1_700_000_030,
        )
        .unwrap();
        assert!(!auth.session_valid(&session));
        assert_eq!(auth.username().as_deref(), Some("newowner"));
        let next = super::totp(&old_secret)
            .unwrap()
            .generate(1_700_000_060)
            .to_string();
        let preview = auth
            .prepare_totp_rotation_at("new sufficiently long password", &next, 1_700_000_060)
            .unwrap();
        let new_secret = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &preview.manual_secret,
        )
        .unwrap();
        let new_code = super::totp(&new_secret)
            .unwrap()
            .generate(1_700_000_060)
            .to_string();
        let rotated_session = auth
            .complete_totp_rotation_at(&new_code, 1_700_000_060)
            .unwrap();
        assert!(auth.session_valid(&rotated_session));
        let old_next = super::totp(&old_secret)
            .unwrap()
            .generate(1_700_000_090)
            .to_string();
        assert!(
            auth.login_at(
                "newowner",
                "new sufficiently long password",
                &old_next,
                1_700_000_090
            )
            .is_none()
        );
    }

    #[test]
    fn setup_totp_bruteforce_is_rate_limited() {
        let dir = tempfile::tempdir().unwrap();
        let auth = AdminAuth::open(dir.path(), &[0x5a; 32]).unwrap();
        let token = std::fs::read_to_string(dir.path().join("admin-setup-token")).unwrap();
        let preview = auth.prepare_setup(&token, "owner").unwrap();
        let secret = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &preview.manual_secret,
        )
        .unwrap();
        let valid = super::totp(&secret)
            .unwrap()
            .generate(1_700_000_000)
            .to_string();
        let wrong = if valid == "000000" {
            "999999"
        } else {
            "000000"
        };
        for _ in 0..5 {
            assert!(
                auth.complete_setup_at(
                    &token,
                    "a sufficiently long password",
                    wrong,
                    1_700_000_000
                )
                .is_err()
            );
        }
        assert!(
            auth.complete_setup_at(
                &token,
                "a sufficiently long password",
                &valid,
                1_700_000_000
            )
            .is_err()
        );
        assert!(!auth.configured());
    }
}
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use totp_rs::{Algorithm, Builder, Totp};
use uuid::Uuid;
use webauthn_rs::prelude::{
    Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Webauthn, WebauthnBuilder,
};

const MAX_FAILURES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const IDLE_LIFETIME: Duration = Duration::from_secs(30 * 60);
const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Clone)]
pub struct AdminAuth {
    inner: Arc<Mutex<Inner>>,
    directory: Arc<PathBuf>,
    pepper: Arc<Vec<u8>>,
    webauthn: Arc<Webauthn>,
}

#[derive(Default)]
struct Inner {
    credentials: Option<AdminCredentials>,
    passkey_account: Option<PasskeyAccount>,
    passkey_registrations: HashMap<String, PendingPasskeyRegistration>,
    passkey_authentications: HashMap<String, PendingPasskeyAuthentication>,
    setup_token: Option<String>,
    pending: Option<PendingSetup>,
    pending_totp: Option<PendingTotp>,
    sessions: HashMap<String, Session>,
    failed_attempts: VecDeque<Instant>,
    in_flight: usize,
}

#[derive(Clone, Serialize, Deserialize)]
struct AdminCredentials {
    username: String,
    password_hash: String,
    totp_secret: Vec<u8>,
    #[serde(default)]
    last_counter: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct PasskeyAccount {
    username: String,
    user_id: Uuid,
    keys: Vec<Passkey>,
}

enum RegistrationPurpose {
    Setup,
    Add { session: String },
}

struct PendingPasskeyRegistration {
    created: Instant,
    username: String,
    user_id: Uuid,
    state: PasskeyRegistration,
    purpose: RegistrationPurpose,
}

enum AuthenticationPurpose {
    Login,
    StepUp { session: String },
}

struct PendingPasskeyAuthentication {
    created: Instant,
    state: PasskeyAuthentication,
    purpose: AuthenticationPurpose,
}

struct PendingSetup {
    username: String,
    secret: Vec<u8>,
    created: Instant,
}

struct PendingTotp {
    secret: Vec<u8>,
    created: Instant,
}

struct Session {
    issued: Instant,
    last_seen: Instant,
    stepup_until: Option<Instant>,
}

pub struct SetupPreview {
    pub manual_secret: String,
    pub otpauth_url: String,
}

fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).context("OS random source unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub(crate) fn private_regular_file(path: &Path) -> Result<bool> {
    private_regular_file_with_limit(path, 8192)
}

fn private_regular_file_with_limit(path: &Path, limit: u64) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("reading private auth file"),
    };
    if !metadata.file_type().is_file() || metadata.len() > limit {
        bail!("admin auth file must be a small regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("admin auth file must have mode 0600 or 0400");
        }
    }
    Ok(true)
}

pub fn read_pepper(path: &Path) -> Result<Vec<u8>> {
    if !path.is_absolute() || !private_regular_file(path)? {
        bail!("auth pepper file must be an absolute private regular file");
    }
    let pepper = std::fs::read(path).context("reading auth pepper")?;
    if pepper.len() != 32 {
        bail!("auth pepper must contain exactly 32 bytes");
    }
    Ok(pepper)
}

fn secure_eq(expected: &str, observed: &str) -> bool {
    let mut diff = expected.len() ^ observed.len();
    for (a, b) in expected.bytes().zip(observed.bytes()) {
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

fn prune_passkey_flows(inner: &mut Inner) {
    inner
        .passkey_registrations
        .retain(|_, pending| pending.created.elapsed() < Duration::from_secs(5 * 60));
    inner
        .passkey_authentications
        .retain(|_, pending| pending.created.elapsed() < Duration::from_secs(5 * 60));
}

fn valid_session(inner: &mut Inner, token: &str) -> bool {
    let Some(session) = inner.sessions.get_mut(token) else {
        return false;
    };
    if session.issued.elapsed() >= SESSION_LIFETIME || session.last_seen.elapsed() >= IDLE_LIFETIME
    {
        inner.sessions.remove(token);
        return false;
    }
    session.last_seen = Instant::now();
    true
}

fn issue_session(inner: &mut Inner) -> Result<String> {
    inner.sessions.retain(|_, session| {
        session.issued.elapsed() < SESSION_LIFETIME && session.last_seen.elapsed() < IDLE_LIFETIME
    });
    if inner.sessions.len() >= 16
        && let Some(oldest) = inner
            .sessions
            .iter()
            .min_by_key(|(_, session)| session.issued)
            .map(|(token, _)| token.clone())
    {
        inner.sessions.remove(&oldest);
    }
    let token = random_hex()?;
    let now = Instant::now();
    inner.sessions.insert(
        token.clone(),
        Session {
            issued: now,
            last_seen: now,
            stepup_until: None,
        },
    );
    Ok(token)
}

pub fn totp(secret: &[u8]) -> Result<Totp> {
    Ok(Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_step_duration(30)
        .with_skew(1)
        .with_secret(secret.to_vec())
        .with_account_name("admin")
        .with_issuer(Some("nx-boss-rs"))
        .build()?)
}

impl AdminAuth {
    #[cfg(test)]
    pub fn open(directory: &Path, pepper: &[u8]) -> Result<Self> {
        Self::open_with_origin(directory, pepper, "https://localhost")
    }

    pub fn open_with_origin(directory: &Path, pepper: &[u8], origin: &str) -> Result<Self> {
        if pepper.len() != 32 {
            bail!("admin pepper must contain exactly 32 random bytes");
        }
        let url = Url::parse(origin).context("invalid admin WebAuthn origin")?;
        if url.scheme() != "https"
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("admin WebAuthn origin must be an HTTPS origin");
        }
        let rp_id = url
            .domain()
            .context("admin WebAuthn origin needs a DNS name")?;
        let webauthn = WebauthnBuilder::new(rp_id, &url)
            .context("invalid WebAuthn relying party")?
            .rp_name("nx-boss")
            .build()
            .context("building WebAuthn relying party")?;
        let auth_path = directory.join("admin-auth.json");
        let passkey_path = directory.join("admin-passkeys.json");
        let setup_path = directory.join("admin-setup-token");
        let passkey_account = if private_regular_file_with_limit(&passkey_path, 64 * 1024)? {
            let bytes = std::fs::read(&passkey_path).context("reading admin passkeys")?;
            let account: PasskeyAccount =
                serde_json::from_slice(&bytes).context("parsing admin passkeys")?;
            if account.username.len() < 3
                || account.username.len() > 64
                || account.keys.is_empty()
                || account.keys.len() > 8
            {
                bail!("invalid admin passkey configuration");
            }
            Some(account)
        } else {
            None
        };
        let (credentials, setup_token) = if private_regular_file(&auth_path)? {
            if passkey_account.is_some() {
                bail!("both legacy admin credentials and passkeys are configured");
            }
            let bytes = std::fs::read(&auth_path).context("reading admin auth")?;
            let credentials: AdminCredentials =
                serde_json::from_slice(&bytes).context("parsing admin auth")?;
            if credentials.username.len() < 3
                || credentials.username.len() > 64
                || credentials.totp_secret.len() != 20
                || !credentials.password_hash.starts_with("$argon2id$")
            {
                bail!("invalid admin auth configuration");
            }
            (Some(credentials), None)
        } else if passkey_account.is_some() {
            (None, None)
        } else {
            let token = if private_regular_file(&setup_path)? {
                std::fs::read_to_string(&setup_path).context("reading admin setup token")?
            } else {
                let token = random_hex()?;
                crate::atomic_file::write(&setup_path, token.as_bytes())?;
                token
            };
            if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("invalid admin setup token");
            }
            (None, Some(token))
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                credentials,
                passkey_account,
                setup_token,
                ..Inner::default()
            })),
            directory: Arc::new(directory.to_path_buf()),
            pepper: Arc::new(pepper.to_vec()),
            webauthn: Arc::new(webauthn),
        })
    }

    pub fn configured(&self) -> bool {
        let inner = crate::lock!(self.inner);
        inner.credentials.is_some() || inner.passkey_account.is_some()
    }

    pub fn passkey_configured(&self) -> bool {
        crate::lock!(self.inner).passkey_account.is_some()
    }

    pub fn passkey_count(&self) -> usize {
        crate::lock!(self.inner)
            .passkey_account
            .as_ref()
            .map_or(0, |account| account.keys.len())
    }

    pub fn start_passkey_setup(
        &self,
        token: &str,
        username: &str,
    ) -> Result<(String, serde_json::Value)> {
        if username.len() < 3 || username.len() > 64 || username.chars().any(char::is_control) {
            bail!("invalid admin username");
        }
        let mut inner = crate::lock!(self.inner);
        if inner.credentials.is_some()
            || inner.passkey_account.is_some()
            || !inner
                .setup_token
                .as_deref()
                .is_some_and(|expected| secure_eq(expected, token))
        {
            bail!("invalid admin setup token");
        }
        prune_passkey_flows(&mut inner);
        if inner.passkey_registrations.len() >= 16 {
            bail!("too many pending passkey registrations");
        }
        let user_id = Uuid::new_v4();
        let (challenge, state) = self
            .webauthn
            .start_passkey_registration(user_id, username, username, None)
            .context("starting passkey registration")?;
        let flow = random_hex()?;
        inner.passkey_registrations.insert(
            flow.clone(),
            PendingPasskeyRegistration {
                created: Instant::now(),
                username: username.to_owned(),
                user_id,
                state,
                purpose: RegistrationPurpose::Setup,
            },
        );
        Ok((flow, serde_json::to_value(challenge)?))
    }

    pub fn finish_passkey_setup(
        &self,
        flow: &str,
        token: &str,
        credential: serde_json::Value,
    ) -> Result<String> {
        let mut inner = crate::lock!(self.inner);
        let pending = inner
            .passkey_registrations
            .remove(flow)
            .context("unknown passkey registration")?;
        if pending.created.elapsed() >= Duration::from_secs(5 * 60)
            || !matches!(pending.purpose, RegistrationPurpose::Setup)
            || inner.credentials.is_some()
            || inner.passkey_account.is_some()
            || !inner
                .setup_token
                .as_deref()
                .is_some_and(|expected| secure_eq(expected, token))
        {
            bail!("passkey setup expired or unauthorized");
        }
        let response: RegisterPublicKeyCredential =
            serde_json::from_value(credential).context("invalid passkey registration response")?;
        let key = self
            .webauthn
            .finish_passkey_registration(&response, &pending.state)
            .context("passkey registration verification failed")?;
        let account = PasskeyAccount {
            username: pending.username,
            user_id: pending.user_id,
            keys: vec![key],
        };
        self.persist_passkeys(&account)?;
        inner.passkey_account = Some(account);
        inner.setup_token = None;
        inner.passkey_registrations.clear();
        let _ = std::fs::remove_file(self.directory.join("admin-setup-token"));
        issue_session(&mut inner)
    }

    pub fn start_passkey_login(&self) -> Result<(String, serde_json::Value)> {
        let mut inner = crate::lock!(self.inner);
        prune_passkey_flows(&mut inner);
        if inner.passkey_authentications.len() >= 16 {
            bail!("too many pending passkey logins");
        }
        let account = inner
            .passkey_account
            .as_ref()
            .context("passkeys not configured")?;
        let (challenge, state) = self
            .webauthn
            .start_passkey_authentication(&account.keys)
            .context("starting passkey authentication")?;
        let flow = random_hex()?;
        inner.passkey_authentications.insert(
            flow.clone(),
            PendingPasskeyAuthentication {
                created: Instant::now(),
                state,
                purpose: AuthenticationPurpose::Login,
            },
        );
        Ok((flow, serde_json::to_value(challenge)?))
    }

    pub fn start_passkey_stepup(&self, session: &str) -> Result<(String, serde_json::Value)> {
        let mut inner = crate::lock!(self.inner);
        prune_passkey_flows(&mut inner);
        if !valid_session(&mut inner, session) || inner.passkey_authentications.len() >= 16 {
            bail!("admin session unavailable");
        }
        let account = inner
            .passkey_account
            .as_ref()
            .context("passkeys not configured")?;
        let (challenge, state) = self
            .webauthn
            .start_passkey_authentication(&account.keys)
            .context("starting passkey confirmation")?;
        let flow = random_hex()?;
        inner.passkey_authentications.insert(
            flow.clone(),
            PendingPasskeyAuthentication {
                created: Instant::now(),
                state,
                purpose: AuthenticationPurpose::StepUp {
                    session: session.to_owned(),
                },
            },
        );
        Ok((flow, serde_json::to_value(challenge)?))
    }

    pub fn finish_passkey_authentication(
        &self,
        flow: &str,
        session: Option<&str>,
        credential: serde_json::Value,
    ) -> Result<Option<String>> {
        let mut inner = crate::lock!(self.inner);
        let pending = inner
            .passkey_authentications
            .remove(flow)
            .context("unknown passkey challenge")?;
        if pending.created.elapsed() >= Duration::from_secs(5 * 60) {
            bail!("passkey challenge expired");
        }
        match &pending.purpose {
            AuthenticationPurpose::Login if session.is_some() => bail!("unexpected session"),
            AuthenticationPurpose::StepUp { session: expected }
                if session != Some(expected.as_str()) || !valid_session(&mut inner, expected) =>
            {
                bail!("admin session mismatch");
            }
            _ => {}
        }
        let response: PublicKeyCredential =
            serde_json::from_value(credential).context("invalid passkey assertion")?;
        let result = self
            .webauthn
            .finish_passkey_authentication(&response, &pending.state)
            .context("passkey assertion verification failed")?;
        let mut updated = inner
            .passkey_account
            .as_ref()
            .context("passkeys not configured")?
            .clone();
        let key = updated
            .keys
            .iter_mut()
            .find(|key| key.cred_id() == result.cred_id())
            .context("unknown passkey credential")?;
        if key.update_credential(&result) == Some(true) {
            self.persist_passkeys(&updated)?;
            inner.passkey_account = Some(updated);
        }
        match pending.purpose {
            AuthenticationPurpose::Login => Ok(Some(issue_session(&mut inner)?)),
            AuthenticationPurpose::StepUp { session } => {
                let active = inner
                    .sessions
                    .get_mut(&session)
                    .context("session expired")?;
                active.stepup_until = Some(Instant::now() + Duration::from_secs(60));
                Ok(None)
            }
        }
    }

    pub fn consume_passkey_stepup(&self, session: &str) -> bool {
        let mut inner = crate::lock!(self.inner);
        if !valid_session(&mut inner, session) {
            return false;
        }
        inner
            .sessions
            .get_mut(session)
            .and_then(|active| active.stepup_until.take())
            .is_some_and(|until| Instant::now() < until)
    }

    pub fn start_passkey_add(&self, session: &str) -> Result<(String, serde_json::Value)> {
        if !self.consume_passkey_stepup(session) {
            bail!("passkey confirmation required");
        }
        let mut inner = crate::lock!(self.inner);
        prune_passkey_flows(&mut inner);
        if inner.passkey_registrations.len() >= 16 {
            bail!("too many pending passkey registrations");
        }
        let account = inner
            .passkey_account
            .as_ref()
            .context("passkeys not configured")?;
        if account.keys.len() >= 8 {
            bail!("passkey limit reached");
        }
        let exclude = account
            .keys
            .iter()
            .map(|key| key.cred_id().clone())
            .collect();
        let (challenge, state) = self.webauthn.start_passkey_registration(
            account.user_id,
            &account.username,
            &account.username,
            Some(exclude),
        )?;
        let username = account.username.clone();
        let user_id = account.user_id;
        let flow = random_hex()?;
        inner.passkey_registrations.insert(
            flow.clone(),
            PendingPasskeyRegistration {
                created: Instant::now(),
                username,
                user_id,
                state,
                purpose: RegistrationPurpose::Add {
                    session: session.to_owned(),
                },
            },
        );
        Ok((flow, serde_json::to_value(challenge)?))
    }

    pub fn finish_passkey_add(
        &self,
        flow: &str,
        session: &str,
        credential: serde_json::Value,
    ) -> Result<()> {
        let mut inner = crate::lock!(self.inner);
        let pending = inner
            .passkey_registrations
            .remove(flow)
            .context("unknown passkey registration")?;
        if pending.created.elapsed() >= Duration::from_secs(5 * 60)
            || !matches!(&pending.purpose, RegistrationPurpose::Add { session: expected } if expected == session)
            || !valid_session(&mut inner, session)
        {
            bail!("passkey registration expired or unauthorized");
        }
        let response: RegisterPublicKeyCredential =
            serde_json::from_value(credential).context("invalid passkey registration response")?;
        let key = self
            .webauthn
            .finish_passkey_registration(&response, &pending.state)?;
        let mut updated = inner
            .passkey_account
            .as_ref()
            .context("passkeys not configured")?
            .clone();
        if updated.user_id != pending.user_id
            || updated.keys.len() >= 8
            || updated
                .keys
                .iter()
                .any(|existing| existing.cred_id() == key.cred_id())
        {
            bail!("duplicate or stale passkey registration");
        }
        updated.keys.push(key);
        self.persist_passkeys(&updated)?;
        inner.passkey_account = Some(updated);
        Ok(())
    }

    fn persist_passkeys(&self, account: &PasskeyAccount) -> Result<()> {
        let bytes = serde_json::to_vec(account).context("serializing admin passkeys")?;
        crate::atomic_file::write(&self.directory.join("admin-passkeys.json"), &bytes)
    }

    pub fn username(&self) -> Option<String> {
        let inner = crate::lock!(self.inner);
        inner
            .passkey_account
            .as_ref()
            .map(|account| account.username.clone())
            .or_else(|| {
                inner
                    .credentials
                    .as_ref()
                    .map(|credentials| credentials.username.clone())
            })
    }

    pub fn prepare_setup(&self, token: &str, username: &str) -> Result<SetupPreview> {
        if username.len() < 3 || username.len() > 64 || username.chars().any(char::is_control) {
            bail!("invalid admin username");
        }
        let mut inner = crate::lock!(self.inner);
        if inner.credentials.is_some()
            || inner.passkey_account.is_some()
            || !inner
                .setup_token
                .as_deref()
                .is_some_and(|expected| secure_eq(expected, token))
        {
            bail!("invalid admin setup token");
        }
        let mut secret = vec![0u8; 20];
        getrandom::fill(&mut secret).context("OS random source unavailable")?;
        let manual_secret = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret);
        let otpauth_url = Builder::new()
            .with_algorithm(Algorithm::SHA1)
            .with_digits(6)
            .with_step_duration(30)
            .with_secret(secret.clone())
            .with_account_name(username)
            .with_issuer(Some("nx-boss-rs"))
            .build()?
            .to_url()?;
        inner.pending = Some(PendingSetup {
            username: username.to_owned(),
            secret: secret.clone(),
            created: Instant::now(),
        });
        Ok(SetupPreview {
            manual_secret,
            otpauth_url,
        })
    }

    pub fn complete_setup(&self, token: &str, password: &str, code: &str) -> Result<String> {
        self.complete_setup_at(token, password, code, now_secs()?)
    }

    fn complete_setup_at(
        &self,
        token: &str,
        password: &str,
        code: &str,
        time: u64,
    ) -> Result<String> {
        let pending = {
            let mut inner = crate::lock!(self.inner);
            let now = Instant::now();
            inner
                .failed_attempts
                .retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
            if inner.failed_attempts.len() + inner.in_flight >= MAX_FAILURES {
                bail!("admin setup rate limit reached");
            }
            if inner.credentials.is_some()
                || inner.passkey_account.is_some()
                || !inner
                    .setup_token
                    .as_deref()
                    .is_some_and(|expected| secure_eq(expected, token))
            {
                bail!("invalid admin setup token");
            }
            let pending = inner.pending.as_ref().context("TOTP setup not prepared")?;
            if pending.created.elapsed() >= Duration::from_secs(10 * 60) {
                bail!("TOTP setup expired");
            }
            let username = pending.username.clone();
            let secret = pending.secret.clone();
            let counter = match totp(&secret)?.check(code, time) {
                Some(counter) => counter,
                None => {
                    inner.failed_attempts.push_back(now);
                    bail!("invalid TOTP code");
                }
            };
            (username, secret, counter)
        };
        let hash = crate::scanner_auth::hash_password(password, &self.pepper)?;
        let credentials = AdminCredentials {
            username: pending.0,
            password_hash: hash,
            totp_secret: pending.1,
            last_counter: pending.2,
        };
        let mut inner = crate::lock!(self.inner);
        if inner.credentials.is_some()
            || inner.passkey_account.is_some()
            || !inner
                .setup_token
                .as_deref()
                .is_some_and(|expected| secure_eq(expected, token))
        {
            bail!("admin already configured");
        }
        self.persist(&credentials)?;
        inner.credentials = Some(credentials);
        inner.setup_token = None;
        inner.pending = None;
        let session_token = random_hex()?;
        let now = Instant::now();
        inner.sessions.insert(
            session_token.clone(),
            Session {
                issued: now,
                last_seen: now,
                stepup_until: None,
            },
        );
        let _ = std::fs::remove_file(self.directory.join("admin-setup-token"));
        Ok(session_token)
    }

    pub fn login(&self, username: &str, password: &str, code: &str) -> Option<String> {
        self.login_at(username, password, code, now_secs().ok()?)
    }

    fn login_at(&self, username: &str, password: &str, code: &str, time: u64) -> Option<String> {
        let credentials = {
            let mut inner = crate::lock!(self.inner);
            let now = Instant::now();
            inner
                .failed_attempts
                .retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
            if inner.failed_attempts.len() + inner.in_flight >= MAX_FAILURES {
                return None;
            }
            let credentials = inner.credentials.clone()?;
            inner.in_flight += 1;
            credentials
        };
        let password_ok = crate::scanner_auth::verify_password_hash(
            password,
            &credentials.password_hash,
            &self.pepper,
        );
        let counter = totp(&credentials.totp_secret)
            .ok()
            .and_then(|totp| totp.check(code, time));
        let mut inner = crate::lock!(self.inner);
        inner.in_flight -= 1;
        let valid = inner.credentials.as_ref().is_some_and(|current| {
            secure_eq(&credentials.username, &current.username)
                && secure_eq(&credentials.password_hash, &current.password_hash)
                && current.totp_secret == credentials.totp_secret
        }) && secure_eq(&credentials.username, username)
            && password_ok
            && counter.is_some_and(|value| {
                value
                    > inner
                        .credentials
                        .as_ref()
                        .map_or(u64::MAX, |current| current.last_counter)
            });
        if !valid {
            inner.failed_attempts.push_back(Instant::now());
            return None;
        }
        let mut updated = inner.credentials.as_ref()?.clone();
        updated.last_counter = counter?;
        if self.persist(&updated).is_err() {
            return None;
        }
        inner.credentials = Some(updated);
        inner.failed_attempts.clear();
        inner.sessions.retain(|_, session| {
            session.issued.elapsed() < SESSION_LIFETIME
                && session.last_seen.elapsed() < IDLE_LIFETIME
        });
        if inner.sessions.len() >= 16
            && let Some(oldest) = inner
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.issued)
                .map(|(token, _)| token.clone())
        {
            inner.sessions.remove(&oldest);
        }
        let token = random_hex().ok()?;
        let now = Instant::now();
        inner.sessions.insert(
            token.clone(),
            Session {
                issued: now,
                last_seen: now,
                stepup_until: None,
            },
        );
        Some(token)
    }

    pub fn session_valid(&self, token: &str) -> bool {
        let mut inner = crate::lock!(self.inner);
        let Some(session) = inner.sessions.get_mut(token) else {
            return false;
        };
        if session.issued.elapsed() >= SESSION_LIFETIME
            || session.last_seen.elapsed() >= IDLE_LIFETIME
        {
            inner.sessions.remove(token);
            return false;
        }
        session.last_seen = Instant::now();
        true
    }

    pub fn logout(&self, token: &str) {
        crate::lock!(self.inner).sessions.remove(token);
    }

    pub fn reauthenticate(&self, password: &str, code: &str) -> bool {
        self.reauthenticate_at(password, code, now_secs().unwrap_or(0))
    }

    pub fn hash_secret(&self, password: &str) -> Result<String> {
        crate::scanner_auth::hash_password(password, &self.pepper)
    }

    pub fn pepper(&self) -> &[u8] {
        &self.pepper
    }

    fn reauthenticate_at(&self, password: &str, code: &str, time: u64) -> bool {
        let Some(username) = self.username() else {
            return false;
        };
        let Some(session) = self.login_at(&username, password, code, time) else {
            return false;
        };
        self.logout(&session);
        true
    }

    pub fn update_account(
        &self,
        password: &str,
        code: &str,
        new_username: &str,
        new_password: &str,
    ) -> Result<()> {
        self.update_account_at(password, code, new_username, new_password, now_secs()?)
    }

    fn update_account_at(
        &self,
        password: &str,
        code: &str,
        new_username: &str,
        new_password: &str,
        time: u64,
    ) -> Result<()> {
        if new_username.len() < 3
            || new_username.len() > 64
            || new_username.chars().any(char::is_control)
        {
            bail!("invalid admin username");
        }
        if !new_password.is_empty() && !(12..=512).contains(&new_password.len()) {
            bail!("new admin password must be 12..512 bytes");
        }
        if !self.reauthenticate_at(password, code, time) {
            bail!("admin reauthentication failed");
        }
        let hash = if new_password.is_empty() {
            None
        } else {
            Some(crate::scanner_auth::hash_password(
                new_password,
                &self.pepper,
            )?)
        };
        let mut inner = crate::lock!(self.inner);
        let mut updated = inner
            .credentials
            .as_ref()
            .context("admin not configured")?
            .clone();
        updated.username = new_username.to_owned();
        if let Some(hash) = hash {
            updated.password_hash = hash;
        }
        self.persist(&updated)?;
        inner.credentials = Some(updated);
        inner.sessions.clear();
        inner.pending_totp = None;
        Ok(())
    }

    pub fn prepare_totp_rotation(&self, password: &str, code: &str) -> Result<SetupPreview> {
        self.prepare_totp_rotation_at(password, code, now_secs()?)
    }

    fn prepare_totp_rotation_at(
        &self,
        password: &str,
        code: &str,
        time: u64,
    ) -> Result<SetupPreview> {
        if !self.reauthenticate_at(password, code, time) {
            bail!("admin reauthentication failed");
        }
        let mut secret = vec![0u8; 20];
        getrandom::fill(&mut secret).context("OS random source unavailable")?;
        let manual_secret = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret);
        let username = self.username().context("admin not configured")?;
        let otpauth_url = Builder::new()
            .with_algorithm(Algorithm::SHA1)
            .with_digits(6)
            .with_step_duration(30)
            .with_secret(secret.clone())
            .with_account_name(username)
            .with_issuer(Some("nx-boss-rs"))
            .build()?
            .to_url()?;
        crate::lock!(self.inner).pending_totp = Some(PendingTotp {
            secret,
            created: Instant::now(),
        });
        Ok(SetupPreview {
            manual_secret,
            otpauth_url,
        })
    }

    pub fn complete_totp_rotation(&self, code: &str) -> Result<String> {
        self.complete_totp_rotation_at(code, now_secs()?)
    }

    fn complete_totp_rotation_at(&self, code: &str, time: u64) -> Result<String> {
        let mut inner = crate::lock!(self.inner);
        let now = Instant::now();
        inner
            .failed_attempts
            .retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
        if inner.failed_attempts.len() + inner.in_flight >= MAX_FAILURES {
            bail!("TOTP rotation rate limit reached");
        }
        let pending = inner
            .pending_totp
            .as_ref()
            .context("TOTP rotation not prepared")?;
        if pending.created.elapsed() >= Duration::from_secs(10 * 60) {
            bail!("TOTP rotation expired");
        }
        let secret = pending.secret.clone();
        let counter = match totp(&secret)?.check(code, time) {
            Some(counter) => counter,
            None => {
                inner.failed_attempts.push_back(now);
                bail!("invalid new TOTP code");
            }
        };
        let mut updated = inner
            .credentials
            .as_ref()
            .context("admin not configured")?
            .clone();
        updated.totp_secret = secret;
        updated.last_counter = counter;
        self.persist(&updated)?;
        inner.credentials = Some(updated);
        inner.pending_totp = None;
        inner.sessions.clear();
        let session = random_hex()?;
        let now = Instant::now();
        inner.sessions.insert(
            session.clone(),
            Session {
                issued: now,
                last_seen: now,
                stepup_until: None,
            },
        );
        Ok(session)
    }

    fn persist(&self, credentials: &AdminCredentials) -> Result<()> {
        let bytes = serde_json::to_vec(credentials).context("serializing admin auth")?;
        crate::atomic_file::write(&self.directory.join("admin-auth.json"), &bytes)
    }
}

fn now_secs() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
