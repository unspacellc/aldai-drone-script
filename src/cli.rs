use crate::DEFAULT_CONFIG_PATH;
use crate::config::{apply_config_value, config_path, load_config, load_config_from, write_config};
use crate::health;
use crate::service::{self, InstallOptions};
use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "unspace",
    version = env!("CARGO_PKG_VERSION"),
    about = "Unspace drone dock ingest agent"
)]
pub struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    /// Install the systemd service and write the initial config.
    Install(InstallArgs),
    /// Stop and remove the systemd service and config.
    Uninstall,
    /// Run the long-lived watcher service.
    Watch,
    /// Show systemd status for the service.
    Status,
    /// Follow journald logs for the service.
    Logs,
    /// Self-update this binary.
    Update(UpdateArgs),
    /// Validate local deployment health.
    Healthcheck,
    /// Show or edit config.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Args, Debug)]
struct InstallArgs {
    #[arg(long)]
    api_key: String,
    #[arg(long)]
    dock_id: String,
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config_path: PathBuf,
}

#[derive(Args, Debug)]
struct UpdateArgs {
    #[arg(long)]
    version: Option<String>,
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Print current config with the API key redacted.
    Show,
    /// Set one supported config key and signal a running service to reload.
    Set { key: String, value: String },
}

pub fn run() -> Result<()> {
    run_from(Cli::parse())
}

pub fn init_logging(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn run_from(cli: Cli) -> Result<()> {
    match cli.command {
        CommandKind::Install(args) => service::install(InstallOptions {
            api_key: args.api_key,
            dock_id: args.dock_id,
            config_path: args.config_path,
        }),
        CommandKind::Uninstall => service::uninstall(),
        CommandKind::Watch => service::watch(),
        CommandKind::Status => service::exec_system("systemctl", &["status", "unspace"]),
        CommandKind::Logs => service::exec_system("journalctl", &["-u", "unspace", "-f"]),
        CommandKind::Update(args) => update(args),
        CommandKind::Healthcheck => health::healthcheck(),
        CommandKind::Config { command } => match command {
            ConfigCommand::Show => config_show(),
            ConfigCommand::Set { key, value } => config_set(&key, &value),
        },
    }
}

fn config_show() -> Result<()> {
    let config = load_config()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&config.redacted_value()?)?
    );
    Ok(())
}

fn config_set(key: &str, value: &str) -> Result<()> {
    let path = config_path();
    let mut config = load_config_from(&path)?;
    apply_config_value(&mut config, key, value)?;
    config.validate()?;
    write_config(&path, &config)?;
    if service::signal_running_service()? {
        println!("Config updated. Service reloaded.");
    } else {
        println!("Config updated. Service not running.");
    }
    Ok(())
}

fn update(args: UpdateArgs) -> Result<()> {
    let requested = args.version.as_deref().unwrap_or("latest");
    println!("checking for {requested} update...");
    bail!("self-update download/apply flow is not implemented yet")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_command_defines_required_subcommands() {
        let command = Cli::command();
        for name in [
            "install",
            "uninstall",
            "watch",
            "status",
            "logs",
            "update",
            "healthcheck",
            "config",
        ] {
            assert!(
                command
                    .get_subcommands()
                    .any(|subcommand| subcommand.get_name() == name),
                "missing subcommand {name}"
            );
        }
    }

    #[test]
    fn clap_config_defines_show_and_set_subcommands() {
        let command = Cli::command();
        let config = command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "config")
            .expect("config subcommand");
        assert!(
            config
                .get_subcommands()
                .any(|subcommand| subcommand.get_name() == "show")
        );
        assert!(
            config
                .get_subcommands()
                .any(|subcommand| subcommand.get_name() == "set")
        );
    }
}
