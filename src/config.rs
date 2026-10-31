use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_JOB_NAME_LEN: usize = 100;
pub const MAX_PATH_LEN: usize = 500;

// Default job values used at creation time and as fallbacks when reading existing jobs.
pub const DEFAULT_COLOR: &str = "#4D4D4D";
pub const DEFAULT_SOURCE: &str = "feeder";
pub const DEFAULT_PIXEL_FORMAT: &str = "rgb24";
pub const DEFAULT_RESOLUTION: u32 = 300;
pub const DEFAULT_JPEG_QUALITY: u8 = 80;

// NmWebService scan_settings JSON keys shared between config parsing and form handling.
pub const KEY_RESOLUTION: &str = "resolution";
pub const KEY_JPEG_QUALITY: &str = "jpegQuality";
pub const KEY_PIXEL_FORMAT: &str = "pixelFormat";
pub const KEY_SOURCE: &str = "source";
pub const KEY_PIXEL_FORMATS: &str = "pixelFormats";

static DEFAULTS_YAML: &str = include_str!("../resources/defaults.yaml");

#[derive(Debug, Deserialize, Serialize)]
pub struct RawConfig {
    pub jobs: IndexMap<String, RawJob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanner_auth: Option<ScannerAuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paperless: Option<PaperlessConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_job_id: Option<usize>,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default = "default_lang")]
    pub lang: String,
}

fn default_lang() -> String {
    "de".to_string()
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct RawJob {
    pub output_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consume_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_settings: Option<HashMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_settings: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub output_path: PathBuf,
    pub consume_path: Option<PathBuf>,
    pub job_info: Value,
    pub scan_settings: Value,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RetentionConfig {
    /// 0 = disabled. Completed batches older than this are compressed to .tar.zst.
    #[serde(default)]
    pub archive_after_days: u32,
    /// 0 = disabled. Archives (or dirs if archiving is off) older than this are deleted.
    #[serde(default)]
    pub delete_after_days: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PaperlessConfig {
    /// Paperless base URL or the complete /api/documents/post_document/ endpoint.
    pub url: String,
    #[serde(default = "default_paperless_token_env")]
    pub token_env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
    #[serde(default)]
    pub tag_ids: Vec<u32>,
    #[serde(default)]
    pub batch_id_field_id: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScannerAuthConfig {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
}

impl ScannerAuthConfig {
    pub fn validate(&self) -> Result<()> {
        match self.mode.as_str() {
            "none" => Ok(()),
            "userpass" => {
                let username = self.username.as_deref().unwrap_or("");
                if username.is_empty()
                    || username.len() > 128
                    || username.chars().any(char::is_control)
                {
                    bail!(
                        "scanner_auth.username is required and must be at most 128 printable bytes"
                    );
                }
                let hash = self.password_hash.as_deref().unwrap_or("");
                if !hash.starts_with("$argon2id$") || hash.len() > 512 {
                    bail!("scanner_auth.password_hash must be an Argon2id PHC string");
                }
                Ok(())
            }
            _ => bail!("scanner_auth.mode must be none or userpass"),
        }
    }
}

fn default_paperless_token_env() -> String {
    "PAPERLESS_API_TOKEN".to_string()
}

#[derive(Debug, Clone)]
pub struct Config {
    pub jobs: Vec<Job>,
    pub scanner_auth: Option<ScannerAuthConfig>,
    pub paperless: Option<PaperlessConfig>,
    pub next_job_id: usize,
    pub retention: RetentionConfig,
    pub lang: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            jobs: Vec::new(),
            scanner_auth: None,
            paperless: None,
            next_job_id: 0,
            retention: RetentionConfig::default(),
            lang: "de".to_string(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(yaml: &str) -> Result<Self> {
        let raw: RawConfig = serde_yaml::from_str(yaml).context("parsing config YAML")?;
        if let Some(ref auth) = raw.scanner_auth {
            auth.validate()?;
        }
        let mut used: std::collections::HashSet<usize> =
            raw.jobs.values().filter_map(|j| j.job_id).collect();
        if used.len() != raw.jobs.values().filter(|j| j.job_id.is_some()).count() {
            bail!("duplicate job_id in config");
        }
        let mut next_id = 0usize;
        let mut jobs = Vec::new();
        for (name, job) in raw.jobs {
            let id = if let Some(id) = job.job_id {
                id
            } else {
                while used.contains(&next_id) {
                    next_id += 1;
                }
                let id = next_id;
                used.insert(id);
                id
            };
            next_id = next_id.max(id.checked_add(1).context("job_id overflow")?);
            jobs.push(Job::parse(id, name, job)?);
        }
        next_id = next_id.max(raw.next_job_id.unwrap_or(0));
        Ok(Self {
            jobs,
            scanner_auth: raw.scanner_auth,
            paperless: raw.paperless,
            next_job_id: next_id,
            retention: raw.retention,
            lang: raw.lang,
        })
    }

    #[cfg(test)]
    pub fn save(jobs: &[Job], retention: &RetentionConfig, lang: &str, path: &Path) -> Result<()> {
        let next_job_id = jobs
            .iter()
            .filter_map(|j| j.job_info["job_id"].as_u64())
            .max()
            .and_then(|id| usize::try_from(id).ok())
            .and_then(|id| id.checked_add(1))
            .unwrap_or(0);
        Self::save_with_next(jobs, retention, lang, next_job_id, None, None, path)
    }

    pub fn save_with_next(
        jobs: &[Job],
        retention: &RetentionConfig,
        lang: &str,
        next_job_id: usize,
        paperless: Option<&PaperlessConfig>,
        scanner_auth: Option<&ScannerAuthConfig>,
        path: &Path,
    ) -> Result<()> {
        let mut map = IndexMap::new();
        for job in jobs {
            let (name, raw) = job.to_raw();
            map.insert(name, raw);
        }
        let raw = RawConfig {
            jobs: map,
            paperless: paperless.cloned(),
            scanner_auth: scanner_auth.cloned(),
            next_job_id: Some(next_job_id),
            retention: retention.clone(),
            lang: lang.to_string(),
        };
        let yaml = serde_yaml::to_string(&raw).context("serializing config")?;
        crate::atomic_file::write(path, yaml.as_bytes()).context("saving config")?;
        Ok(())
    }
}

impl Job {
    pub fn name(&self) -> &str {
        self.job_info["name"].as_str().unwrap_or("")
    }

    pub fn color(&self) -> &str {
        self.job_info["color"].as_str().unwrap_or(DEFAULT_COLOR)
    }

    fn sources(&self) -> &Value {
        &self.scan_settings["parameters"]["task"]["actions"]["streams"]["sources"]
    }

    fn pf_attr(&self, name: &str) -> Option<&str> {
        self.sources()["pixelFormats"]["attributes"]
            .as_array()?
            .iter()
            .find(|a| a["attribute"].as_str() == Some(name))
            .and_then(|a| a["values"]["value"].as_str())
    }

    pub fn resolution(&self) -> u32 {
        self.pf_attr(KEY_RESOLUTION)
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_RESOLUTION)
    }

    pub fn jpeg_quality(&self) -> u8 {
        self.pf_attr(KEY_JPEG_QUALITY)
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_JPEG_QUALITY)
    }

    pub fn pixel_format(&self) -> &str {
        self.sources()[KEY_PIXEL_FORMATS][KEY_PIXEL_FORMAT]
            .as_str()
            .unwrap_or(DEFAULT_PIXEL_FORMAT)
    }

    pub fn source(&self) -> &str {
        self.sources()[KEY_SOURCE]
            .as_str()
            .unwrap_or(DEFAULT_SOURCE)
    }

    pub fn to_raw(&self) -> (String, RawJob) {
        let job_settings = self.job_info["job_setting"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let scan_settings = self
            .sources()
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        (
            self.name().to_string(),
            RawJob {
                output_path: self.output_path.to_string_lossy().to_string(),
                job_id: self.job_info["job_id"]
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok()),
                consume_path: self
                    .consume_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                color: Some(self.color().to_string()),
                job_settings,
                scan_settings,
            },
        )
    }

    pub(crate) fn parse(id: usize, name: String, raw: RawJob) -> Result<Self> {
        let color = raw.color.clone().unwrap_or_else(|| "#4D4D4D".to_string());
        validate_hex_color(&color).with_context(|| format!("job {name:?}: invalid color"))?;

        let output_path = PathBuf::from(&raw.output_path);
        check_dir_writable(&output_path)
            .with_context(|| format!("job {name:?}: output_path {:?}", output_path))?;

        let consume_path = raw
            .consume_path
            .map(|p| -> Result<PathBuf> {
                let path = PathBuf::from(&p);
                check_dir_writable(&path)
                    .with_context(|| format!("job {name:?}: consume_path {:?}", path))?;
                Ok(path)
            })
            .transpose()?;

        let job_settings = build_job_settings(raw.job_settings.as_ref());
        let scan_settings = build_scan_settings(raw.scan_settings.as_ref())?;

        let job_info = json!({
            "type": 0,
            "job_id": id,
            "name": name,
            "color": color,
            "job_setting": job_settings,
            "hierarchy_list": null,
        });

        Ok(Self {
            output_path,
            consume_path,
            job_info,
            scan_settings,
        })
    }
}

pub fn validate_hex_color(color: &str) -> Result<()> {
    let s = color.trim();
    let valid = s.starts_with('#')
        && (s.len() == 7 || s.len() == 4)
        && s[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        bail!("color must be #RRGGBB or #RGB hex, got: {s:?}");
    }
    Ok(())
}

fn check_dir_writable(path: &Path) -> Result<()> {
    if !path.is_dir() {
        std::fs::create_dir_all(path)
            .with_context(|| format!("cannot create directory {}", path.display()))?;
    }
    let probe = path.join(".nx_boss_write_probe");
    std::fs::write(&probe, b"").with_context(|| format!("{} is not writable", path.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn default_job_settings() -> Value {
    json!({
        "continuous_scan": false,
        "show_message": false,
        "message": null,
        "show_thumbnail": false,
        "show_scan_button": false,
        "auto_logout": false,
        "wait_file_transfer": false,
        "show_transfer_completion": false,
        "metadata_setting": null,
        "job_timeout": 0
    })
}

fn build_job_settings(overrides: Option<&HashMap<String, Value>>) -> Value {
    let mut settings = default_job_settings();
    if let Some(overrides) = overrides {
        for (k, v) in overrides {
            settings[k] = v.clone();
        }
    }
    settings
}

fn default_scan_settings() -> Result<Value> {
    serde_yaml::from_str(DEFAULTS_YAML).context("parsing defaults.yaml")
}

fn build_scan_settings(overrides: Option<&HashMap<String, Value>>) -> Result<Value> {
    let mut settings = default_scan_settings()?;
    if let Some(overrides) = overrides {
        let sources = &mut settings["parameters"]["task"]["actions"]["streams"]["sources"];
        update_recursive(sources, overrides)?;
    }
    Ok(settings)
}

const MAX_OVERRIDE_DEPTH: usize = 20;

fn update_recursive(dest: &mut Value, src: &HashMap<String, Value>) -> Result<()> {
    update_recursive_inner(dest, src, 0)
}

fn update_recursive_inner(
    dest: &mut Value,
    src: &HashMap<String, Value>,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_OVERRIDE_DEPTH {
        bail!("scan_settings nesting too deep (max {MAX_OVERRIDE_DEPTH} levels)");
    }
    for (key, value) in src {
        if let Value::Object(map) = value {
            update_recursive_inner(
                &mut dest[key],
                &map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                depth + 1,
            )?;
            continue;
        }
        if dest.get(key).is_some() {
            coerce_and_set(dest, key, value)?;
        } else if let Some(attrs) = dest["attributes"].as_array() {
            // Find matching attribute entry by name and update its value
            let idx = attrs
                .iter()
                .position(|a| a["attribute"].as_str() == Some(key));
            if let Some(i) = idx {
                let attr_value = value_to_string(value)?;
                dest["attributes"][i]["values"]["value"] = Value::String(attr_value);
            } else {
                bail!("unknown attribute: {key}");
            }
        } else {
            bail!("unknown key: {key}");
        }
    }
    Ok(())
}

fn coerce_and_set(dest: &mut Value, key: &str, value: &Value) -> Result<()> {
    let default = &dest[key];
    match (value, default) {
        // Same type — set directly
        (Value::Bool(_), Value::Bool(_))
        | (Value::String(_), Value::String(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::Array(_), Value::Array(_))
        | (Value::Null, Value::Null) => {
            dest[key] = value.clone();
        }
        // bool → "true"/"false" string
        (Value::Bool(b), Value::String(_)) => {
            dest[key] = Value::String(if *b { "true" } else { "false" }.to_string());
        }
        // int → string
        (Value::Number(n), Value::String(_)) => {
            dest[key] = Value::String(n.to_string());
        }
        _ => bail!("bad type for {key}: got {value}, expected {default}"),
    }
    Ok(())
}

fn value_to_string(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
        _ => bail!("cannot convert {v} to string"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_output_dir() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    #[test]
    fn test_parse_minimal_config() {
        let dir = make_output_dir();
        let yaml = format!(
            "jobs:\n  default:\n    output_path: {}\n",
            dir.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        assert_eq!(config.jobs.len(), 1);
        assert_eq!(config.jobs[0].job_info["name"], "default");
        assert_eq!(config.jobs[0].job_info["job_id"], 0);
    }

    #[test]
    fn test_parse_multiple_jobs() {
        let dir = make_output_dir();
        let p = dir.path().display();
        let yaml =
            format!("jobs:\n  first:\n    output_path: {p}\n  second:\n    output_path: {p}\n");
        let config = Config::parse(&yaml).unwrap();
        assert_eq!(config.jobs.len(), 2);
        assert_eq!(config.jobs[1].job_info["job_id"], 1);
    }

    #[test]
    fn test_job_color_default() {
        let dir = make_output_dir();
        let yaml = format!("jobs:\n  x:\n    output_path: {}\n", dir.path().display());
        let config = Config::parse(&yaml).unwrap();
        assert_eq!(config.jobs[0].job_info["color"], "#4D4D4D");
    }

    #[test]
    fn test_job_custom_color() {
        let dir = make_output_dir();
        let yaml = format!(
            "jobs:\n  x:\n    output_path: {}\n    color: '#ff0000'\n",
            dir.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        assert_eq!(config.jobs[0].job_info["color"], "#ff0000");
    }

    #[test]
    fn test_output_path_created_if_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let new_dir = tmp.path().join("sub/dir");
        assert!(!new_dir.exists());
        let yaml = format!("jobs:\n  x:\n    output_path: {}\n", new_dir.display());
        assert!(Config::parse(&yaml).is_ok(), "missing dir must be created");
        assert!(new_dir.is_dir());
    }

    #[test]
    fn test_invalid_output_path_not_creatable() {
        // /proc/nx_boss_xyz cannot be created — permission denied
        let yaml = "jobs:\n  x:\n    output_path: /proc/nx_boss_xyz\n";
        assert!(Config::parse(yaml).is_err());
    }

    #[test]
    fn test_scan_settings_resolution_override() {
        let dir = make_output_dir();
        let yaml = format!(
            "jobs:\n  quality:\n    output_path: {}\n    scan_settings:\n      pixelFormats:\n        resolution: 600\n",
            dir.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        let sources =
            &config.jobs[0].scan_settings["parameters"]["task"]["actions"]["streams"]["sources"];
        let attrs = sources["pixelFormats"]["attributes"].as_array().unwrap();
        let res = attrs
            .iter()
            .find(|a| a["attribute"] == "resolution")
            .unwrap();
        assert_eq!(res["values"]["value"], "600");
    }

    #[test]
    fn test_job_settings_continuous_scan() {
        let dir = make_output_dir();
        let yaml = format!(
            "jobs:\n  multi:\n    output_path: {}\n    job_settings:\n      continuous_scan: true\n",
            dir.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        assert_eq!(
            config.jobs[0].job_info["job_setting"]["continuous_scan"],
            true
        );
    }

    #[test]
    fn test_save_preserves_lang() {
        let dir = make_output_dir();
        let yaml = format!(
            "lang: en\njobs:\n  x:\n    output_path: {}\n",
            dir.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        assert_eq!(config.lang, "en");

        let cfg_path = dir.path().join("config.yaml");
        Config::save(&config.jobs, &config.retention, &config.lang, &cfg_path).unwrap();

        let saved = std::fs::read_to_string(&cfg_path).unwrap();
        let reloaded = Config::parse(&saved).unwrap();
        assert_eq!(
            reloaded.lang, "en",
            "lang must survive a save/reload round-trip"
        );
    }

    #[test]
    fn test_save_preserves_advanced_job_and_scan_settings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let yaml = format!(
            "jobs:\n  Advanced:\n    output_path: {}\n    job_settings:\n      continuous_scan: true\n      job_timeout: 42\n    scan_settings:\n      pixelFormats:\n        resolution: 600\n        jpegQuality: 95\n      source: feederFront\n",
            tmp.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        let path = tmp.path().join("config.yaml");
        Config::save(&config.jobs, &config.retention, &config.lang, &path).unwrap();
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(
            reloaded.jobs[0].job_info["job_setting"]["continuous_scan"],
            true
        );
        assert_eq!(reloaded.jobs[0].job_info["job_setting"]["job_timeout"], 42);
        assert_eq!(reloaded.jobs[0].resolution(), 600);
        assert_eq!(reloaded.jobs[0].source(), "feederFront");
    }

    #[test]
    fn test_save_preserves_paperless_api_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let yaml = format!(
            "jobs:\n  Scan:\n    output_path: {}\npaperless:\n  url: http://paperless:8000\n  token_env: PAPERLESS_API_TOKEN\n  tag_ids: [7]\n",
            tmp.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        let path = tmp.path().join("config.yaml");
        Config::save_with_next(
            &config.jobs,
            &config.retention,
            &config.lang,
            config.next_job_id,
            config.paperless.as_ref(),
            None,
            &path,
        )
        .unwrap();
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(reloaded.paperless.unwrap().tag_ids, vec![7]);
    }

    #[test]
    fn saving_jobs_preserves_scanner_security_configuration() {
        let tmp = tempfile::TempDir::new().unwrap();
        let yaml = format!(
            "jobs:\n  Scan:\n    output_path: {}\nscanner_auth:\n  mode: userpass\n  username: scanner\n  password_hash: '$argon2id$v=19$m=19456,t=2,p=1$testsalt$testhash'\n",
            tmp.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        let path = tmp.path().join("config.yaml");
        Config::save_with_next(
            &config.jobs,
            &config.retention,
            &config.lang,
            config.next_job_id,
            config.paperless.as_ref(),
            config.scanner_auth.as_ref(),
            &path,
        )
        .unwrap();
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(reloaded.scanner_auth.unwrap().mode, "userpass");
    }

    #[test]
    fn test_bool_to_string_coercion() {
        let dir = make_output_dir();
        // source defaults have string "true"/"false" values; bool overrides must be coerced
        let yaml = format!(
            "jobs:\n  x:\n    output_path: {}\n    scan_settings:\n      pixelFormats:\n        automaticDeskew: false\n",
            dir.path().display()
        );
        let config = Config::parse(&yaml).unwrap();
        let sources =
            &config.jobs[0].scan_settings["parameters"]["task"]["actions"]["streams"]["sources"];
        let attrs = sources["pixelFormats"]["attributes"].as_array().unwrap();
        let attr = attrs
            .iter()
            .find(|a| a["attribute"] == "automaticDeskew")
            .unwrap();
        assert_eq!(attr["values"]["value"], "false");
    }

    #[test]
    fn test_deeply_nested_scan_settings_is_rejected() {
        let dir = make_output_dir();
        // Build a 25-level nested YAML map — exceeds the recursion limit in update_recursive.
        // Without a depth guard this would stack-overflow on pathological config files.
        let mut inner = "  leaf: 1".to_string();
        for i in 0..25 {
            inner = format!("  key{i}:\n  {}", inner.replace('\n', "\n  "));
        }
        let yaml = format!(
            "jobs:\n  x:\n    output_path: {}\n    scan_settings:\n{inner}\n",
            dir.path().display()
        );
        assert!(
            Config::parse(&yaml).is_err(),
            "deeply nested scan_settings must be rejected"
        );
    }
}
