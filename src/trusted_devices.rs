use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const TRUST_DAYS: i64 = 30;
pub const MAX_AGE: i64 = TRUST_DAYS * 24 * 60 * 60;

#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}
#[derive(Serialize, Deserialize)]
struct File {
    version: u8,
    devices: Vec<Device>,
}

pub struct TrustedDevices {
    path: PathBuf,
    devices: Vec<Device>,
}

fn token_hash(token: &str) -> Option<String> {
    (token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())).then(|| {
        Sha256::digest(token.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    })
}
fn same_hash(left: &str, right: &str) -> bool {
    crate::security::constant_time_eq(left.as_bytes(), right.as_bytes())
}
fn active(device: &Device, now: DateTime<Utc>) -> bool {
    device.created_at <= now && now < device.expires_at
}

impl TrustedDevices {
    pub fn open(directory: &Path) -> Self {
        let path = directory.join("admin-trusted-devices.json");
        let loaded = (|| -> Result<Vec<Device>> {
            ensure!(
                crate::admin_auth::private_regular_file_with_limit(&path, 64 * 1024)?,
                "missing trusted-device store"
            );
            let file: File = serde_json::from_slice(&std::fs::read(&path)?)?;
            ensure!(
                file.version == 1 && file.devices.len() <= 8,
                "invalid trusted-device store"
            );
            let mut hashes = std::collections::HashSet::new();
            for d in &file.devices {
                ensure!(
                    !d.name.is_empty()
                        && d.name.chars().count() <= 64
                        && !d.name.chars().any(char::is_control)
                        && d.hash.len() == 64
                        && d.hash.bytes().all(|b| b.is_ascii_hexdigit())
                        && hashes.insert(d.hash.clone())
                        && d.expires_at.signed_duration_since(d.created_at)
                            == chrono::Duration::days(TRUST_DAYS)
                        && d.created_at <= d.last_used_at
                        && d.last_used_at < d.expires_at,
                    "invalid trusted device"
                );
            }
            Ok(file.devices)
        })();
        let devices = loaded.unwrap_or_else(|_| {
            tracing::warn!(
                event = "admin.trust_store_unavailable",
                error_class = "storage",
                "trusted devices unavailable; passkey required"
            );
            Vec::new()
        });
        Self { path, devices }
    }
    pub fn forget_in_memory(&mut self) {
        self.devices.clear();
    }
    fn save(&mut self, devices: Vec<Device>) -> Result<()> {
        crate::atomic_file::write(
            &self.path,
            &serde_json::to_vec(&File {
                version: 1,
                devices: devices.clone(),
            })?,
        )?;
        self.devices = devices;
        Ok(())
    }
    pub fn issue(&mut self, token: &str, name: &str, now: DateTime<Utc>) -> Result<String> {
        let name = name.trim();
        ensure!(
            name.chars().count() <= 64 && !name.chars().any(char::is_control),
            "invalid device name"
        );
        let hash = token_hash(token).ok_or_else(|| anyhow::anyhow!("invalid trust token"))?;
        let mut devices: Vec<_> = self
            .devices
            .iter()
            .filter(|d| active(d, now))
            .cloned()
            .collect();
        if devices.len() >= 8 {
            let oldest = devices
                .iter()
                .enumerate()
                .min_by_key(|(_, d)| d.created_at)
                .map(|(i, _)| i)
                .expect("bounded devices");
            devices.remove(oldest);
        }
        devices.push(Device {
            name: if name.is_empty() {
                "Browser".into()
            } else {
                name.into()
            },
            hash: hash.clone(),
            created_at: now,
            expires_at: now + chrono::Duration::days(TRUST_DAYS),
            last_used_at: now,
        });
        self.save(devices)?;
        Ok(hash)
    }
    pub fn valid_token(&self, token: &str, now: DateTime<Utc>) -> bool {
        token_hash(token).is_some_and(|hash| self.valid_hash(&hash, now))
    }
    pub fn valid_hash(&self, hash: &str, now: DateTime<Utc>) -> bool {
        self.devices
            .iter()
            .any(|d| same_hash(&d.hash, hash) && active(d, now))
    }
    pub fn use_token(&mut self, token: &str, now: DateTime<Utc>) -> Result<Option<String>> {
        let Some(hash) = token_hash(token) else {
            return Ok(None);
        };
        let Some(index) = self
            .devices
            .iter()
            .position(|d| same_hash(&d.hash, &hash) && active(d, now))
        else {
            return Ok(None);
        };
        if self.devices[index].last_used_at.date_naive() != now.date_naive() {
            let mut updated = self.devices.clone();
            updated[index].last_used_at = now;
            self.save(updated)?;
        }
        Ok(Some(hash))
    }
    pub fn list(&self, current: Option<&str>) -> Vec<DeviceView> {
        let current = current.and_then(token_hash);
        self.devices
            .iter()
            .map(|d| DeviceView {
                id: d.hash.clone(),
                name: d.name.clone(),
                created_at: d.created_at.to_rfc3339(),
                expires_at: d.expires_at.to_rfc3339(),
                last_used_at: d.last_used_at.to_rfc3339(),
                current: current
                    .as_ref()
                    .is_some_and(|hash| same_hash(hash, &d.hash)),
            })
            .collect()
    }
    pub fn revoke(&mut self, id: Option<&str>) -> Result<Vec<String>> {
        let removed: Vec<_> = self
            .devices
            .iter()
            .filter(|d| id.is_none_or(|id| same_hash(id, &d.hash)))
            .map(|d| d.hash.clone())
            .collect();
        if !removed.is_empty() || id.is_none() {
            let remaining = self
                .devices
                .iter()
                .filter(|d| !removed.contains(&d.hash))
                .cloned()
                .collect();
            self.save(remaining)?;
        }
        Ok(removed)
    }
    pub fn revoke_token(&mut self, token: &str) -> Result<Vec<String>> {
        match token_hash(token) {
            Some(hash) => self.revoke(Some(&hash)),
            None => Ok(Vec::new()),
        }
    }
}
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub expires_at: String,
    pub last_used_at: String,
    pub current: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_devices_have_only_hashes_and_expire_on_day_31() {
        let root = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let token = "7".repeat(64);
        let mut store = TrustedDevices::open(root.path());
        store.issue(&token, "Laptop", now).unwrap();
        let path = root.path().join("admin-trusted-devices.json");
        assert!(!std::fs::read_to_string(&path).unwrap().contains(&token));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mut restored = TrustedDevices::open(root.path());
        assert!(
            restored
                .use_token(&token, now + chrono::Duration::days(29))
                .unwrap()
                .is_some()
        );
        assert!(
            restored
                .use_token(&token, now + chrono::Duration::days(30))
                .unwrap()
                .is_none()
        );
        assert!(!restored.valid_token(&"8".repeat(64), now));
    }
    #[test]
    fn last_used_writes_once_per_utc_day_without_extending_expiration() {
        let root = tempfile::tempdir().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let token = "6".repeat(64);
        let mut store = TrustedDevices::open(root.path());
        store.issue(&token, "Laptop", now).unwrap();
        let path = root.path().join("admin-trusted-devices.json");
        let original = std::fs::read(&path).unwrap();
        store
            .use_token(&token, now + chrono::Duration::hours(2))
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        store
            .use_token(&token, now + chrono::Duration::days(1))
            .unwrap();
        assert_eq!(
            store.devices[0].last_used_at,
            now + chrono::Duration::days(1)
        );
        assert_eq!(
            store.devices[0].expires_at,
            now + chrono::Duration::days(30)
        );
    }
    #[test]
    fn ninth_device_evicts_oldest_and_revocation_persists() {
        let root = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let mut store = TrustedDevices::open(root.path());
        for i in 0..9 {
            store
                .issue(
                    &format!("{i:064x}"),
                    "Browser",
                    now + chrono::Duration::minutes(i),
                )
                .unwrap();
        }
        assert_eq!(store.devices.len(), 8);
        assert!(!store.valid_token(&format!("{:064x}", 0), now + chrono::Duration::minutes(10)));
        store.revoke_token(&format!("{:064x}", 1)).unwrap();
        assert_eq!(TrustedDevices::open(root.path()).devices.len(), 7);
        store.revoke(None).unwrap();
        assert!(TrustedDevices::open(root.path()).devices.is_empty());
    }
    #[test]
    fn broken_or_insecure_files_disable_only_device_trust() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("admin-trusted-devices.json");
        std::fs::write(&path, b"broken").unwrap();
        assert!(TrustedDevices::open(root.path()).devices.is_empty());
        let mut store = TrustedDevices::open(root.path());
        store.issue(&"a".repeat(64), "Laptop", Utc::now()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(TrustedDevices::open(root.path()).devices.is_empty());
        }
    }
}
