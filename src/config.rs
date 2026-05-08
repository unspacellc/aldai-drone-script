use crate::{CONFIG_VERSION, DEFAULT_CONFIG_PATH, UPLOAD_DIR};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AgentConfig {
    pub config_version: u32,
    pub api_base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub dock_id: String,
    pub watch_dir: PathBuf,
    pub heartbeat_file: PathBuf,
    pub heartbeat_interval_secs: u64,
    pub heartbeat_max_age_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            api_base_url: "https://api.unspace.com".to_string(),
            api_key: String::new(),
            dock_id: String::new(),
            watch_dir: PathBuf::from(UPLOAD_DIR),
            heartbeat_file: PathBuf::from("/tmp/unspace.heartbeat"),
            heartbeat_interval_secs: 15,
            heartbeat_max_age_secs: 120,
        }
    }
}

impl AgentConfig {
    pub fn initial(api_key: String, dock_id: String) -> Self {
        Self {
            api_key,
            dock_id,
            ..Self::default()
        }
    }

    pub fn validate(&mut self) -> Result<()> {
        self.validate_with_api_key_override(env::var("UNSPACE_API_KEY").ok().as_deref())
    }

    pub fn validate_with_api_key_override(&mut self, api_key_override: Option<&str>) -> Result<()> {
        if self.config_version != CONFIG_VERSION {
            bail!(
                "Unsupported config_version {}. Only version {} is supported.",
                self.config_version,
                CONFIG_VERSION
            );
        }
        self.api_base_url = self.api_base_url.trim_end_matches('/').to_string();
        if self.api_base_url.is_empty() {
            bail!("api_base_url is required");
        }
        if self.dock_id.trim().is_empty() {
            bail!("dock_id is required");
        }
        if let Some(api_key) = api_key_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            self.api_key = api_key.to_string();
        }
        if self.api_key.trim().is_empty() {
            bail!("API key is required in config api_key or UNSPACE_API_KEY");
        }
        if !self.api_key.starts_with("ysk_") {
            bail!("API key must start with ysk_");
        }
        validate_nonzero("heartbeat_interval_secs", self.heartbeat_interval_secs)?;
        validate_nonzero("heartbeat_max_age_secs", self.heartbeat_max_age_secs)?;
        Ok(())
    }

    pub fn redacted_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        value["api_key"] = if self.api_key.is_empty() {
            json!("")
        } else {
            json!("<redacted>")
        };
        Ok(value)
    }
}

pub fn config_path() -> PathBuf {
    env::var("UNSPACE_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH))
}

pub fn load_config() -> Result<AgentConfig> {
    load_config_from(&config_path())
}

pub fn load_config_from(path: &Path) -> Result<AgentConfig> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    let raw: Value = serde_json::from_str(&contents).context("config is not valid JSON")?;
    match raw.get("config_version").and_then(Value::as_u64) {
        Some(1) => {}
        Some(other) => bail!("Unsupported config_version {other}. Only version 1 is supported."),
        None => bail!("config_version is required and must be 1"),
    }
    let mut config: AgentConfig =
        serde_json::from_value(raw).context("config schema is invalid")?;
    config.validate()?;
    Ok(config)
}

pub fn write_config(path: &Path, config: &AgentConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(config)?;
    fs::write(path, format!("{serialized}\n"))
        .with_context(|| format!("failed to write config at {}", path.display()))
}

pub fn apply_config_value(config: &mut AgentConfig, key: &str, value: &str) -> Result<()> {
    match key {
        "api_base_url" => config.api_base_url = value.to_string(),
        "api_key" => config.api_key = value.to_string(),
        "dock_id" => config.dock_id = value.to_string(),
        "watch_dir" => config.watch_dir = PathBuf::from(value),
        "heartbeat_file" => config.heartbeat_file = PathBuf::from(value),
        "heartbeat_interval_secs" => config.heartbeat_interval_secs = value.parse()?,
        "heartbeat_max_age_secs" => config.heartbeat_max_age_secs = value.parse()?,
        "config_version" => bail!("config_version cannot be changed with config set"),
        _ => bail!("unsupported config key '{key}'"),
    }
    Ok(())
}

fn validate_nonzero(field: &str, value: u64) -> Result<()> {
    if value == 0 {
        bail!("{field} must be greater than 0");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn valid_config() -> AgentConfig {
        AgentConfig::initial("ysk_test".to_string(), "dock".to_string())
    }

    #[test]
    fn validation_rejects_missing_api_key() {
        let mut config = AgentConfig {
            api_key: String::new(),
            ..valid_config()
        };
        assert!(
            config
                .validate_with_api_key_override(None)
                .unwrap_err()
                .to_string()
                .contains("API key is required")
        );
    }

    #[test]
    fn validation_applies_api_key_override() {
        let mut config = AgentConfig {
            api_key: String::new(),
            ..valid_config()
        };
        config
            .validate_with_api_key_override(Some("ysk_from_env"))
            .unwrap();
        assert_eq!(config.api_key, "ysk_from_env");
    }

    #[test]
    fn config_value_set_rejects_removed_future_fields() {
        let mut config = valid_config();
        assert!(
            apply_config_value(&mut config, "future_upload_field", "value")
                .unwrap_err()
                .to_string()
                .contains("unsupported config key")
        );
    }

    #[test]
    fn load_config_rejects_unsupported_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"config_version":2}"#).unwrap();
        assert!(
            load_config_from(&path)
                .unwrap_err()
                .to_string()
                .contains("Unsupported config_version 2")
        );
    }

    #[test]
    fn load_config_requires_version_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{}"#).unwrap();
        assert!(
            load_config_from(&path)
                .unwrap_err()
                .to_string()
                .contains("config_version is required")
        );
    }

    #[test]
    fn redacted_config_hides_api_key() {
        let config = valid_config();
        assert_eq!(
            config.redacted_value().unwrap()["api_key"],
            json!("<redacted>")
        );
    }

    #[test]
    fn write_and_load_config_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.json");
        let config = valid_config();
        write_config(&path, &config).unwrap();
        assert_eq!(load_config_from(&path).unwrap(), config);
    }
}
