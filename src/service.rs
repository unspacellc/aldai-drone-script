use crate::config::{AgentConfig, config_path, load_config, write_config};
use crate::{PID_FILE, QUEUE_DIR, SYSTEMD_UNIT_PATH, UPLOAD_DIR};
use anyhow::{Context, Result, bail};
use signal_hook::consts::signal::SIGHUP;
use signal_hook::flag as signal_flag;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub api_key: String,
    pub dock_id: String,
    pub config_path: PathBuf,
}

pub fn install(options: InstallOptions) -> Result<()> {
    let mut config = AgentConfig::initial(options.api_key, options.dock_id);
    config.validate()?;
    write_config(&options.config_path, &config)?;

    fs::create_dir_all(UPLOAD_DIR).context("failed to create upload directory")?;
    fs::create_dir_all(QUEUE_DIR).context("failed to create queue directory")?;
    create_service_user_best_effort();
    let _ = Command::new("chown")
        .args(["-R", "unspace:unspace", "/var/lib/unspace"])
        .status();

    fs::write(SYSTEMD_UNIT_PATH, systemd_unit(&options.config_path))
        .context("failed to write systemd unit")?;
    exec_system("systemctl", &["daemon-reload"])?;
    exec_system("systemctl", &["enable", "--now", "unspace"])?;
    println!("Unspace service installed and started.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let _ = Command::new("systemctl").args(["stop", "unspace"]).status();
    let _ = Command::new("systemctl")
        .args(["disable", "unspace"])
        .status();
    remove_if_exists(Path::new(SYSTEMD_UNIT_PATH))?;
    remove_if_exists(&config_path())?;
    remove_if_exists(Path::new(PID_FILE))?;
    exec_system("systemctl", &["daemon-reload"])?;
    println!("Unspace service uninstalled.");
    Ok(())
}

pub fn watch() -> Result<()> {
    let mut config = load_config()?;
    write_pid_file()?;
    let reload = Arc::new(AtomicBool::new(false));
    signal_flag::register(SIGHUP, Arc::clone(&reload))
        .context("failed to register SIGHUP handler")?;
    info!(watch_dir = %config.watch_dir.display(), "watch service started");
    let mut heartbeat_elapsed = config.heartbeat_interval_secs;

    loop {
        if reload.swap(false, Ordering::Relaxed) {
            match load_config() {
                Ok(new_config) => {
                    info!(old_watch_dir = %config.watch_dir.display(), new_watch_dir = %new_config.watch_dir.display(), "config reloaded");
                    config = new_config;
                }
                Err(error) => {
                    warn!(error = %error, "failed to reload config; keeping previous config")
                }
            }
        }

        if heartbeat_elapsed >= config.heartbeat_interval_secs {
            write_heartbeat(&config.heartbeat_file)?;
            heartbeat_elapsed = 0;
        }

        thread::sleep(Duration::from_secs(1));
        heartbeat_elapsed += 1;
    }
}

pub fn signal_running_service() -> Result<bool> {
    signal_running_service_from(Path::new(PID_FILE))
}

pub fn signal_running_service_from(pid_file: &Path) -> Result<bool> {
    let pid = match fs::read_to_string(pid_file) {
        Ok(pid) => pid.trim().to_string(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("failed to read PID file"),
    };
    if pid.is_empty() {
        return Ok(false);
    }
    let status = Command::new("kill")
        .args(["-HUP", &pid])
        .status()
        .context("failed to execute kill -HUP")?;
    Ok(status.success())
}

pub fn write_heartbeat(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create heartbeat directory {}", parent.display())
        })?;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    fs::write(path, now.to_string())
        .with_context(|| format!("failed to write heartbeat {}", path.display()))
}

pub fn systemd_unit(config_path: &Path) -> String {
    format!(
        r#"[Unit]
Description=Unspace Drone Dock CLI
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=unspace
Group=unspace
Environment=UNSPACE_CONFIG_PATH={}
ExecStartPre=/usr/local/bin/unspace healthcheck
ExecStart=/usr/local/bin/unspace watch
ExecStartPost=/usr/local/bin/unspace healthcheck
Restart=always
RestartSec=5
StartLimitIntervalSec=300
StartLimitBurst=5
WatchdogSec=60
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=true
ReadWritePaths=/tmp /var/lib/unspace/uploads /var/lib/unspace/queue
StandardOutput=journal
StandardError=journal
SyslogIdentifier=unspace

[Install]
WantedBy=multi-user.target
"#,
        config_path.display()
    )
}

pub fn exec_system(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to execute {program}"))?;
    if !status.success() {
        bail!("{program} exited with status {status}");
    }
    Ok(())
}

fn write_pid_file() -> Result<()> {
    fs::write(PID_FILE, std::process::id().to_string()).context("failed to write PID file")
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn create_service_user_best_effort() {
    let _ = Command::new("groupadd")
        .args(["--system", "unspace"])
        .status();
    let _ = Command::new("useradd")
        .args([
            "--system",
            "--gid",
            "unspace",
            "--home-dir",
            "/var/lib/unspace",
            "--shell",
            "/usr/sbin/nologin",
            "unspace",
        ])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn systemd_unit_contains_hardening_and_config_path() {
        let unit = systemd_unit(Path::new("/tmp/unspace-config.json"));
        assert!(unit.contains("Environment=UNSPACE_CONFIG_PATH=/tmp/unspace-config.json"));
        assert!(unit.contains("NoNewPrivileges=true"));
        assert!(unit.contains("ProtectSystem=full"));
        assert!(
            unit.contains("ReadWritePaths=/tmp /var/lib/unspace/uploads /var/lib/unspace/queue")
        );
        assert!(unit.contains("ExecStart=/usr/local/bin/unspace watch"));
    }

    #[test]
    fn write_heartbeat_creates_parent_and_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let heartbeat = dir.path().join("nested/heartbeat");
        write_heartbeat(&heartbeat).unwrap();
        let timestamp: u64 = fs::read_to_string(heartbeat).unwrap().parse().unwrap();
        assert!(timestamp > 0);
    }

    #[test]
    fn signal_running_service_returns_false_when_pid_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!signal_running_service_from(&dir.path().join("missing.pid")).unwrap());
    }
}
