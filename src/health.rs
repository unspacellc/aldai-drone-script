use crate::config::{AgentConfig, load_config};
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn healthcheck() -> Result<()> {
    let config = load_config()?;
    check_local_deployment(&config)?;
    Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?
        .get(format!("{}/health", config.api_base_url))
        .send()
        .context("API /health is unreachable")?
        .error_for_status()
        .context("API /health returned an error status")?;
    println!("Healthcheck passed.");
    Ok(())
}

pub fn check_local_deployment(config: &AgentConfig) -> Result<()> {
    if !config.watch_dir.exists() {
        bail!(
            "watch directory does not exist: {}",
            config.watch_dir.display()
        );
    }
    if !config.watch_dir.is_dir() {
        bail!(
            "watch path is not a directory: {}",
            config.watch_dir.display()
        );
    }
    let _ = fs::read_dir(&config.watch_dir).with_context(|| {
        format!(
            "watch directory is not readable: {}",
            config.watch_dir.display()
        )
    })?;
    check_heartbeat(config)
}

pub fn check_heartbeat(config: &AgentConfig) -> Result<()> {
    if !config.heartbeat_file.exists() {
        bail!(
            "heartbeat file does not exist: {}",
            config.heartbeat_file.display()
        );
    }
    let contents = fs::read_to_string(&config.heartbeat_file).with_context(|| {
        format!(
            "failed to read heartbeat file {}",
            config.heartbeat_file.display()
        )
    })?;
    let timestamp: u64 = contents
        .trim()
        .parse()
        .context("heartbeat file does not contain a Unix timestamp")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let age = now.saturating_sub(timestamp);
    if age > config.heartbeat_max_age_secs {
        bail!(
            "heartbeat is stale ({age}s old, max {}s)",
            config.heartbeat_max_age_secs
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn config_for_paths(
        watch_dir: &std::path::Path,
        heartbeat_file: &std::path::Path,
    ) -> AgentConfig {
        AgentConfig {
            api_key: "ysk_test".to_string(),
            dock_id: "dock".to_string(),
            watch_dir: watch_dir.to_path_buf(),
            heartbeat_file: heartbeat_file.to_path_buf(),
            heartbeat_max_age_secs: 120,
            ..AgentConfig::default()
        }
    }

    #[test]
    fn local_deployment_passes_with_fresh_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat = dir.path().join("heartbeat");
        fs::write(
            &heartbeat,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .to_string(),
        )
        .unwrap();
        let config = config_for_paths(dir.path(), &heartbeat);
        check_local_deployment(&config).unwrap();
    }

    #[test]
    fn local_deployment_rejects_missing_watch_dir() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat = dir.path().join("heartbeat");
        fs::write(
            &heartbeat,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .to_string(),
        )
        .unwrap();
        let config = config_for_paths(&dir.path().join("missing"), &heartbeat);
        assert!(
            check_local_deployment(&config)
                .unwrap_err()
                .to_string()
                .contains("watch directory does not exist")
        );
    }

    #[test]
    fn heartbeat_rejects_stale_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat = dir.path().join("heartbeat");
        fs::write(&heartbeat, "1").unwrap();
        let config = config_for_paths(dir.path(), &heartbeat);
        assert!(
            check_heartbeat(&config)
                .unwrap_err()
                .to_string()
                .contains("heartbeat is stale")
        );
    }
}
