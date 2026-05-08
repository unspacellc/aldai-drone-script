pub mod cli;
pub mod config;
pub mod health;
pub mod service;

pub const CONFIG_VERSION: u32 = 1;
pub const DEFAULT_CONFIG_PATH: &str = "/etc/unspace/config.json";
pub const PID_FILE: &str = "/var/run/unspace.pid";
pub const SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/unspace.service";
pub const UPLOAD_DIR: &str = "/var/lib/unspace/uploads";
pub const QUEUE_DIR: &str = "/var/lib/unspace/queue";
