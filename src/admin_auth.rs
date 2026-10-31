use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;
use webauthn_rs::prelude::{
    Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Webauthn, WebauthnBuilder,
};
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
    passkey_account: Option<PasskeyAccount>,
    passkey_registrations: HashMap<String, PendingPasskeyRegistration>,
    passkey_authentications: HashMap<String, PendingPasskeyAuthentication>,
    setup_token: Option<String>,
    sessions: HashMap<String, Session>,
    #[cfg(test)]
    test_configured: bool,
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
struct Session {
    issued: Instant,
    last_seen: Instant,
    stepup_until: Option<Instant>,
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
        match directory.join("admin-auth.json").symlink_metadata() {
            Ok(_) => bail!(
                "Unsupported admin-auth.json found. Back up and remove the obsolete account file before starting; passkeys are required."
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("checking admin-auth.json"),
        }
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
        let setup_token = if passkey_account.is_some() {
            None
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
            Some(token)
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
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
        #[cfg(test)]
        if inner.test_configured {
            return true;
        }
        inner.passkey_account.is_some()
    }
    pub fn username(&self) -> Option<String> {
        crate::lock!(self.inner)
            .passkey_account
            .as_ref()
            .map(|a| a.username.clone())
    }
    pub fn session_valid(&self, token: &str) -> bool {
        valid_session(&mut crate::lock!(self.inner), token)
    }
    #[cfg(test)]
    pub(crate) fn test_session(&self) -> String {
        let mut inner = crate::lock!(self.inner);
        inner.test_configured = true;
        issue_session(&mut inner).unwrap()
    }
    #[cfg(test)]
    pub(crate) fn test_confirm_stepup(&self, session: &str) {
        crate::lock!(self.inner)
            .sessions
            .get_mut(session)
            .unwrap()
            .stepup_until = Some(Instant::now() + Duration::from_secs(60));
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
        if inner.passkey_account.is_some()
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

    pub fn logout(&self, token: &str) {
        crate::lock!(self.inner).sessions.remove(token);
    }

    pub fn hash_secret(&self, password: &str) -> Result<String> {
        crate::scanner_auth::hash_password(password, &self.pepper)
    }

    pub fn pepper(&self) -> &[u8] {
        &self.pepper
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_confirmation_is_one_use_and_logout_revokes_access() {
        let dir = tempfile::tempdir().unwrap();
        let auth = AdminAuth::open(dir.path(), &[0x42; 32]).unwrap();
        let session = auth.test_session();
        assert!(auth.configured());
        assert!(!auth.consume_passkey_stepup(&session));
        auth.test_confirm_stepup(&session);
        assert!(auth.consume_passkey_stepup(&session));
        assert!(!auth.consume_passkey_stepup(&session));
        auth.logout(&session);
        assert!(!auth.session_valid(&session));
    }

    #[test]
    fn obsolete_account_file_fails_with_clear_path_and_does_not_seed_setup() {
        let dir = tempfile::tempdir().unwrap();
        crate::atomic_file::write(&dir.path().join("admin-auth.json"), b"obsolete account")
            .unwrap();
        let result = AdminAuth::open(dir.path(), &[0x42; 32]);
        assert!(matches!(result, Err(ref error) if error.to_string().contains("admin-auth.json")));
        assert!(!dir.path().join("admin-setup-token").exists());
    }

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
}
