use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};

const HASH_PREFIX: &str = "$argon2id$v=19$m=19456,t=2,p=1$";

#[derive(Clone, Default)]
pub enum ScannerAuthMode {
    #[default]
    None,
    Userpass(Arc<ScannerCredentials>),
}

#[derive(Clone, Default)]
pub struct ScannerAuthStore(Arc<RwLock<ScannerAuthMode>>);

impl ScannerAuthStore {
    pub fn new(mode: ScannerAuthMode) -> Self {
        Self(Arc::new(RwLock::new(mode)))
    }
    pub fn replace(&self, mode: ScannerAuthMode) {
        *self.0.write().unwrap_or_else(|error| error.into_inner()) = mode;
    }
    fn snapshot(&self) -> ScannerAuthMode {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    pub fn wire_type(&self) -> &'static str {
        self.snapshot().wire_type()
    }
    pub fn requires_login(&self) -> bool {
        self.snapshot().requires_login()
    }
    pub fn probe_matches(&self, body: &[u8]) -> (bool, bool) {
        self.snapshot().probe_matches(body)
    }
    pub fn login(&self, body: &[u8]) -> Option<String> {
        self.snapshot().login(body)
    }
    pub fn session_valid(&self, token: &str) -> bool {
        self.snapshot().session_valid(token)
    }
    pub fn revoke(&self, token: &str) {
        self.snapshot().revoke(token);
    }
}

pub struct ScannerCredentials {
    username: String,
    password: Vec<u8>,
    password_hash: Option<String>,
    pepper: Vec<u8>,
    sessions: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    tokens: HashMap<String, Instant>,
    failed_attempts: VecDeque<Instant>,
    in_flight: usize,
}

const SESSION_LIFETIME: Duration = Duration::from_secs(8 * 60 * 60);
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILURES: usize = 5;
const MAX_SESSIONS: usize = 32;

impl ScannerAuthMode {
    pub fn export_hashed_config(
        &self,
        pepper: &[u8],
    ) -> anyhow::Result<crate::config::ScannerAuthConfig> {
        match self {
            Self::None => Ok(crate::config::ScannerAuthConfig {
                mode: "none".into(),
                username: None,
                password_hash: None,
            }),
            Self::Userpass(credentials) => {
                anyhow::ensure!(
                    credentials.password_hash.is_none(),
                    "scanner password already hashed"
                );
                let password = std::str::from_utf8(&credentials.password)
                    .context("scanner password is not UTF-8")?;
                Ok(crate::config::ScannerAuthConfig {
                    mode: "userpass".into(),
                    username: Some(credentials.username.clone()),
                    password_hash: Some(hash_legacy_password(password, pepper)?),
                })
            }
        }
    }
    pub fn from_env() -> anyhow::Result<Self> {
        let mode = match std::env::var("NX_BOSS_SCANNER_AUTH_MODE") {
            Ok(mode) => mode,
            Err(std::env::VarError::NotPresent) => "none".into(),
            Err(std::env::VarError::NotUnicode(_)) => {
                bail!("NX_BOSS_SCANNER_AUTH_MODE must be UTF-8")
            }
        };
        let username = std::env::var("NX_BOSS_SCANNER_USERNAME").ok();
        let password_file = std::env::var_os("NX_BOSS_SCANNER_PASSWORD_FILE").map(PathBuf::from);
        Self::from_values(&mode, username.as_deref(), password_file.as_deref())
    }

    pub(crate) fn from_values(
        mode: &str,
        username: Option<&str>,
        password_file: Option<&Path>,
    ) -> anyhow::Result<Self> {
        match mode {
            "" | "none" => Ok(Self::None),
            "userpass" => {
                let username = username
                    .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
                    .context("NX_BOSS_SCANNER_USERNAME is required for userpass mode")?;
                let path = password_file
                    .context("NX_BOSS_SCANNER_PASSWORD_FILE is required for userpass mode")?;
                if !path.is_absolute() {
                    bail!("NX_BOSS_SCANNER_PASSWORD_FILE must be an absolute path");
                }
                let metadata =
                    std::fs::symlink_metadata(path).context("scanner password file unavailable")?;
                if !metadata.is_file() || metadata.len() > 4096 {
                    bail!("scanner password file must be a regular file of at most 4096 bytes");
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        bail!("scanner password file must have mode 0600 or 0400");
                    }
                }
                let mut password =
                    std::fs::read(path).context("scanner password file unavailable")?;
                if password.last() == Some(&b'\n') {
                    password.pop();
                    if password.last() == Some(&b'\r') {
                        password.pop();
                    }
                }
                if password.is_empty() || password.len() > 4096 || password.contains(&0) {
                    bail!("scanner password file must contain a nonempty text secret");
                }
                Ok(Self::Userpass(Arc::new(ScannerCredentials {
                    username: username.into(),
                    password,
                    password_hash: None,
                    pepper: Vec::new(),
                    sessions: Mutex::new(SessionState::default()),
                })))
            }
            _ => bail!("NX_BOSS_SCANNER_AUTH_MODE must be none or userpass"),
        }
    }

    pub fn from_hashed_config(
        config: &crate::config::ScannerAuthConfig,
        pepper: &[u8],
    ) -> anyhow::Result<Self> {
        config.validate()?;
        if config.mode == "none" {
            return Ok(Self::None);
        }
        if pepper.len() != 32 {
            bail!("scanner pepper must contain exactly 32 random bytes");
        }
        let hash = config
            .password_hash
            .as_deref()
            .context("missing scanner password hash")?;
        if !hash.starts_with(HASH_PREFIX) || hash.len() > 256 {
            bail!("unsupported scanner password hash parameters");
        }
        PasswordHash::new(hash).map_err(|_| anyhow::anyhow!("invalid scanner password hash"))?;
        Ok(Self::Userpass(Arc::new(ScannerCredentials {
            username: config
                .username
                .clone()
                .context("missing scanner username")?,
            password: Vec::new(),
            password_hash: Some(hash.to_owned()),
            pepper: pepper.to_vec(),
            sessions: Mutex::new(SessionState::default()),
        })))
    }

    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Userpass(_) => "userpass",
        }
    }

    pub fn requires_login(&self) -> bool {
        matches!(self, Self::Userpass(_))
    }

    // Diagnostic only. Never include request values or configured secrets in a log.
    pub fn probe_matches(&self, body: &[u8]) -> (bool, bool) {
        let Self::Userpass(credentials) = self else {
            return (false, false);
        };
        let Ok(payload) = serde_json::from_slice::<serde_json::Value>(body) else {
            return (false, false);
        };
        let user_match = payload
            .get("auth_info_user")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| {
                crate::security::constant_time_eq(credentials.username.as_bytes(), value.as_bytes())
            });
        let pass_match = credentials.password_hash.is_none()
            && payload
                .get("auth_info_pass")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| {
                    crate::security::constant_time_eq(&credentials.password, value.as_bytes())
                });
        (user_match, pass_match)
    }

    pub fn login(&self, body: &[u8]) -> Option<String> {
        let Self::Userpass(credentials) = self else {
            return None;
        };
        let now = Instant::now();
        {
            let mut state = crate::lock!(credentials.sessions);
            state
                .failed_attempts
                .retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
            if state.failed_attempts.len() + state.in_flight >= MAX_FAILURES {
                return None;
            }
            state.in_flight += 1;
        }
        let (user_match, pass_match) = credentials.verify(body);
        let mut state = crate::lock!(credentials.sessions);
        state.in_flight -= 1;
        if !user_match || !pass_match {
            state.failed_attempts.push_back(now);
            return None;
        }
        state.failed_attempts.clear();
        state
            .tokens
            .retain(|_, issued| now.duration_since(*issued) < SESSION_LIFETIME);
        if state.tokens.len() >= MAX_SESSIONS
            && let Some(oldest) = state
                .tokens
                .iter()
                .min_by_key(|(_, issued)| *issued)
                .map(|(token, _)| token.clone())
        {
            state.tokens.remove(&oldest);
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        state.tokens.insert(token.clone(), now);
        Some(token)
    }

    pub fn session_valid(&self, token: &str) -> bool {
        let Self::Userpass(credentials) = self else {
            return false;
        };
        let mut state = crate::lock!(credentials.sessions);
        let Some(issued) = state.tokens.get(token) else {
            return false;
        };
        if issued.elapsed() >= SESSION_LIFETIME {
            state.tokens.remove(token);
            return false;
        }
        true
    }

    pub fn revoke(&self, token: &str) {
        if let Self::Userpass(credentials) = self {
            crate::lock!(credentials.sessions).tokens.remove(token);
        }
    }
}

impl ScannerCredentials {
    fn verify(&self, body: &[u8]) -> (bool, bool) {
        let Ok(payload) = serde_json::from_slice::<serde_json::Value>(body) else {
            return (false, false);
        };
        let user_match = payload
            .get("auth_info_user")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| {
                crate::security::constant_time_eq(self.username.as_bytes(), value.as_bytes())
            });
        let password = payload
            .get("auth_info_pass")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let pass_match = if let Some(ref hash) = self.password_hash {
            verify_password_hash(password, hash, &self.pepper)
        } else {
            crate::security::constant_time_eq(&self.password, password.as_bytes())
        };
        (user_match, pass_match)
    }
}

pub fn hash_password(password: &str, pepper: &[u8]) -> anyhow::Result<String> {
    if password.len() < 12 || password.len() > 512 {
        bail!("new password must be 12..512 bytes");
    }
    hash_legacy_password(password, pepper)
}

fn hash_legacy_password(password: &str, pepper: &[u8]) -> anyhow::Result<String> {
    if password.is_empty() || password.len() > 512 || pepper.len() != 32 {
        bail!("invalid legacy password or pepper");
    }
    let params = Params::new(19456, 2, 1, Some(32))?;
    let hasher = Argon2::new_with_secret(pepper, Algorithm::Argon2id, Version::V0x13, params)?;
    let hash = hasher
        .hash_password(password.as_bytes())
        .map_err(|_| anyhow::anyhow!("password hashing failed"))?
        .to_string();
    Ok(hash)
}

pub fn verify_password_hash(password: &str, hash: &str, pepper: &[u8]) -> bool {
    if !hash.starts_with(HASH_PREFIX) || hash.len() > 256 || pepper.len() != 32 {
        return false;
    }
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::new_with_secret(
        pepper,
        Algorithm::Argon2id,
        Version::V0x13,
        Params::default(),
    )
    .is_ok_and(|hasher| hasher.verify_password(password.as_bytes(), &parsed).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_needs_no_credentials() {
        assert_eq!(
            ScannerAuthMode::from_values("none", None, None)
                .unwrap()
                .wire_type(),
            "none"
        );
    }

    #[test]
    fn userpass_requires_both_credential_sources() {
        assert!(ScannerAuthMode::from_values("userpass", None, None).is_err());
        assert!(ScannerAuthMode::from_values("userpass", Some("alice"), None).is_err());
        assert!(
            ScannerAuthMode::from_values("userpass", None, Some(Path::new("/missing"))).is_err()
        );
        assert!(ScannerAuthMode::from_values("other", None, None).is_err());
    }

    #[test]
    fn userpass_loads_private_password_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scanner-password");
        std::fs::write(&path, b"a long example password\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mode = ScannerAuthMode::from_values("userpass", Some("alice"), Some(&path)).unwrap();
        assert_eq!(mode.wire_type(), "userpass");
        assert!(mode.requires_login());
        let ScannerAuthMode::Userpass(credentials) = mode else {
            panic!("wrong mode")
        };
        assert_eq!(credentials.username, "alice");
        assert_eq!(credentials.password, b"a long example password");
    }

    #[test]
    fn legacy_password_migrates_to_verifiable_hash_without_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("password");
        std::fs::write(&path, "temporary-password\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let legacy =
            ScannerAuthMode::from_values("userpass", Some("scanner"), Some(&path)).unwrap();
        let config = legacy.export_hashed_config(&[9u8; 32]).unwrap();
        let serialized = serde_yaml::to_string(&config).unwrap();
        assert!(!serialized.contains("temporary-password"));
        let migrated = ScannerAuthMode::from_hashed_config(&config, &[9u8; 32]).unwrap();
        assert!(
            migrated
                .login(br#"{"auth_info_user":"scanner","auth_info_pass":"temporary-password"}"#)
                .is_some()
        );
        assert!(
            ScannerAuthMode::from_hashed_config(&config, &[8u8; 32])
                .unwrap()
                .login(br#"{"auth_info_user":"scanner","auth_info_pass":"temporary-password"}"#)
                .is_none()
        );
    }

    #[test]
    fn migration_accepts_existing_short_test_password_but_new_hashes_reject_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("password");
        std::fs::write(&path, "1234\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let legacy =
            ScannerAuthMode::from_values("userpass", Some("scanner"), Some(&path)).unwrap();
        let config = legacy.export_hashed_config(&[9u8; 32]).unwrap();
        assert!(hash_password("1234", &[9u8; 32]).is_err());
        assert!(
            ScannerAuthMode::from_hashed_config(&config, &[9u8; 32])
                .unwrap()
                .login(br#"{"auth_info_user":"scanner","auth_info_pass":"1234"}"#)
                .is_some()
        );
    }

    #[test]
    fn userpass_rejects_readable_by_other_users_and_empty_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scanner-password");
        std::fs::write(&path, b"password").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(ScannerAuthMode::from_values("userpass", Some("alice"), Some(&path)).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(&path, b"\n").unwrap();
        assert!(ScannerAuthMode::from_values("userpass", Some("alice"), Some(&path)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn userpass_rejects_symlinked_password_file() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-password");
        let link = dir.path().join("scanner-password");
        std::fs::write(&real, b"private-test-password").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&real, &link).unwrap();
        assert!(ScannerAuthMode::from_values("userpass", Some("alice"), Some(&link)).is_err());
    }

    #[test]
    fn observed_scanner_fields_can_be_compared_without_returning_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scanner-password");
        std::fs::write(&path, b"private-test-password\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mode = ScannerAuthMode::from_values("userpass", Some("alice"), Some(&path)).unwrap();
        assert_eq!(
            mode.probe_matches(
                br#"{"auth_info_user":"alice","auth_info_pass":"private-test-password"}"#
            ),
            (true, true)
        );
        assert_eq!(
            mode.probe_matches(br#"{"auth_info_user":"alice","auth_info_pass":"wrong"}"#),
            (true, false)
        );
        assert_eq!(
            mode.probe_matches(br#"{"auth_info_user":{},"auth_info_pass":null}"#),
            (false, false)
        );
        assert_eq!(mode.probe_matches(b"not json"), (false, false));
    }

    #[test]
    fn login_limits_failures_and_sessions_expire_or_disappear_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scanner-password");
        std::fs::write(&path, b"private-test-password").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mode = ScannerAuthMode::from_values("userpass", Some("alice"), Some(&path)).unwrap();
        let valid = br#"{"auth_info_user":"alice","auth_info_pass":"private-test-password"}"#;
        let invalid = br#"{"auth_info_user":"alice","auth_info_pass":"wrong"}"#;
        for _ in 0..MAX_FAILURES {
            assert!(mode.login(invalid).is_none());
        }
        assert!(mode.login(valid).is_none());
        let ScannerAuthMode::Userpass(credentials) = &mode else {
            panic!("wrong mode")
        };
        crate::lock!(credentials.sessions).failed_attempts.clear();
        let token = mode.login(valid).unwrap();
        assert!(mode.session_valid(&token));
        let restarted =
            ScannerAuthMode::from_values("userpass", Some("alice"), Some(&path)).unwrap();
        assert!(!restarted.session_valid(&token));
        crate::lock!(credentials.sessions)
            .tokens
            .insert(token.clone(), Instant::now() - SESSION_LIFETIME);
        assert!(!mode.session_valid(&token));
    }

    #[test]
    fn hashed_scanner_password_uses_distinct_salts_and_external_pepper() {
        let pepper = [0x5au8; 32];
        let other_pepper = [0x33u8; 32];
        let hash_a = super::hash_password("synthetic-long-password", &pepper).unwrap();
        let hash_b = super::hash_password("synthetic-long-password", &pepper).unwrap();
        assert_ne!(hash_a, hash_b);
        assert!(hash_a.starts_with("$argon2id$"));
        assert!(!hash_a.contains("synthetic-long-password"));
        let config = crate::config::ScannerAuthConfig {
            mode: "userpass".into(),
            username: Some("scanner".into()),
            password_hash: Some(hash_a),
        };
        let good = ScannerAuthMode::from_hashed_config(&config, &pepper).unwrap();
        let bad = ScannerAuthMode::from_hashed_config(&config, &other_pepper).unwrap();
        let payload = br#"{"auth_info_user":"scanner","auth_info_pass":"synthetic-long-password"}"#;
        assert!(good.login(payload).is_some());
        assert!(bad.login(payload).is_none());
    }
}
