use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

static DEFAULTS_YAML: &str = include_str!("../defaults.yaml");

#[derive(Debug, Deserialize, Serialize)]
pub struct RawConfig {
    pub jobs: IndexMap<String, RawJob>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct RawJob {
    pub output_path: String,
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

#[derive(Debug, Clone)]
pub struct Config {
    pub jobs: Vec<Job>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(yaml: &str) -> Result<Self> {
        let raw: RawConfig = serde_yaml::from_str(yaml).context("parsing config YAML")?;
        let jobs = raw
            .jobs
            .into_iter()
            .enumerate()
            .map(|(id, (name, job))| Job::parse(id, name, job))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { jobs })
    }

    pub fn save(jobs: &[Job], path: &Path) -> Result<()> {
        let mut map = IndexMap::new();
        for job in jobs {
            let (name, raw) = job.to_raw();
            map.insert(name, raw);
        }
        let raw = RawConfig { jobs: map };
        let yaml = serde_yaml::to_string(&raw).context("serializing config")?;
        std::fs::write(path, yaml).context("writing config file")?;
        Ok(())
    }
}

impl Job {
    pub fn name(&self) -> &str {
        self.job_info["name"].as_str().unwrap_or("")
    }

    pub fn color(&self) -> &str {
        self.job_info["color"].as_str().unwrap_or("#4D4D4D")
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
        self.pf_attr("resolution")
            .and_then(|s| s.parse().ok())
            .unwrap_or(300)
    }

    pub fn jpeg_quality(&self) -> u8 {
        self.pf_attr("jpegQuality")
            .and_then(|s| s.parse().ok())
            .unwrap_or(80)
    }

    pub fn pixel_format(&self) -> &str {
        self.sources()["pixelFormats"]["pixelFormat"]
            .as_str()
            .unwrap_or("rgb24")
    }

    pub fn source(&self) -> &str {
        self.sources()["source"].as_str().unwrap_or("feeder")
    }

    pub fn to_raw(&self) -> (String, RawJob) {
        let mut pf = HashMap::new();
        pf.insert("resolution".to_string(), json!(self.resolution()));
        pf.insert("jpegQuality".to_string(), json!(self.jpeg_quality()));
        pf.insert("pixelFormat".to_string(), json!(self.pixel_format()));
        let mut scan_settings = HashMap::new();
        scan_settings.insert("pixelFormats".to_string(), json!(pf));
        scan_settings.insert("source".to_string(), json!(self.source()));
        (
            self.name().to_string(),
            RawJob {
                output_path: self.output_path.to_string_lossy().to_string(),
                consume_path: self
                    .consume_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                color: Some(self.color().to_string()),
                job_settings: None,
                scan_settings: Some(scan_settings),
            },
        )
    }

    fn parse(id: usize, name: String, raw: RawJob) -> Result<Self> {
        let output_path = PathBuf::from(&raw.output_path);
        if !output_path.is_dir() {
            bail!("output_path {:?} is not a directory", output_path);
        }

        let consume_path = raw
            .consume_path
            .map(|p| {
                let path = PathBuf::from(&p);
                if !path.is_dir() {
                    bail!("consume_path {:?} is not a directory", path);
                }
                Ok(path)
            })
            .transpose()?;

        let job_settings = build_job_settings(raw.job_settings.as_ref());
        let scan_settings = build_scan_settings(raw.scan_settings.as_ref())?;

        let job_info = json!({
            "type": 0,
            "job_id": id,
            "name": name,
            "color": raw.color.unwrap_or_else(|| "#4D4D4D".to_string()),
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

fn update_recursive(dest: &mut Value, src: &HashMap<String, Value>) -> Result<()> {
    for (key, value) in src {
        if let Value::Object(map) = value {
            update_recursive(
                &mut dest[key],
                &map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
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
        | (Value::Number(_), Value::Number(_)) => {
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
    fn test_invalid_output_path() {
        let yaml = "jobs:\n  x:\n    output_path: /nonexistent/path/xyz\n";
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
}
