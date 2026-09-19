//! Autospot -- Wi-Fi watchdog that turns Windows' Mobile Hotspot on when Wi-Fi stays down.
//!
//! This file is the command line and nothing else: it resolves the config path, decides
//! what logging the chosen command needs, and hands off. `watchdog` runs the loop,
//! `service` deals with the Service Control Manager, `diagnostics` prints `status`.

mod adapters;
mod companion;
mod config;
mod diagnostics;
mod hotspot;
mod logging;
mod policy;
mod service;
mod status;
mod uplink;
mod watchdog;
mod wifi;

use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use tokio::sync::Notify;
use tracing::error;

use crate::config::Config;
use crate::hotspot::Hotspot;

/// Autospot -- Wi-Fi watchdog + automatic Mobile Hotspot
///
/// A portable, foreground console app: run it and leave the window open. It prints its
/// status -- Wi-Fi connection, IP address, hotspot state -- every few seconds, and an
/// explicit line for every action it takes. Stop it with Ctrl+C.
#[derive(Parser, Debug, PartialEq)]
#[command(name = "autospot", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Config file (default: autospot.toml next to the executable)
    #[arg(short = 'c', long = "config", value_name = "PATH", global = true)]
    config: Option<PathBuf>,

    // A bare path is also accepted so dropping a .toml file onto the exe works, e.g.
    // `autospot C:\path\autospot.toml`. Hidden from --help since --config is the
    // documented way to do this.
    #[arg(value_name = "PATH", hide = true)]
    bare_config: Option<PathBuf>,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
enum Command {
    /// Watch Wi-Fi and manage the hotspot (default)
    Run,
    /// Print Wi-Fi, adapter and hotspot state, then exit
    Status,
    /// Turn the hotspot on once, using the config, then exit
    Start,
    /// Turn the hotspot off once, then exit
    Stop,
    /// Manage autospot as a Windows service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
enum ServiceAction {
    /// Register autospot as an auto-start Windows service and start it
    Install,
    /// Stop the service, then start it again -- e.g. after editing autospot.toml, since
    /// the service only reads the config file once, at startup
    Restart,
    /// Stop (if running) and unregister the service
    Remove,
    /// Print whether the service is installed and its current state
    Status,
    /// Internal: entry point used by the Service Control Manager
    #[command(hide = true)]
    Run,
}

fn main() -> std::process::ExitCode {
    match real_main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // The error may predate logging setup, so always write it to stderr too.
            eprintln!("error: {e:#}");
            error!("{e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<()> {
    use ServiceAction::{Remove, Restart, Run, Status};

    let cli = Cli::parse();
    let config_path = match cli.config.or(cli.bare_config) {
        Some(path) => path,
        None => default_config_path()?,
    };

    match cli.command.unwrap_or(Command::Run) {
        // Service management talks to the Service Control Manager and nothing else -- no
        // config file to read, no logging to set up. `run` is the hidden variant the SCM
        // itself launches; it sets up both on its own and does not return until the
        // service stops. `install` is the one action that wants the config, so it goes
        // through `run_with_config` below with everything else.
        Command::Service { action: Run } => service::run_dispatcher(config_path),
        Command::Service { action: Remove } => service::remove(),
        Command::Service { action: Restart } => service::restart(),
        Command::Service { action: Status } => service::print_status(),
        command => run_with_config(&command, &config_path),
    }
}

/// Every other command reads `autospot.toml` first -- including `service install`, which
/// validates it rather than registering a service that would fail on every start.
fn run_with_config(command: &Command, config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;

    // `status` is a one-shot diagnostic command; file logging would only get in the way.
    let _guard = match command {
        Command::Status => {
            logging::init_console_only(&cfg);
            None
        }
        _ => Some(logging::init(&cfg, logging::Mode::Console)?),
    };

    match command {
        Command::Run => {
            if service::is_running()? {
                anyhow::bail!(
                    "autospot is already running as a Windows service; stop it with \
                     `autospot service remove` (or `sc stop autospot`) before running it \
                     in the foreground. Check `autospot service status` for details."
                );
            }
            // Never notified, so the loop runs until the process is killed.
            block_on(watchdog::run(&cfg, config_path, &Notify::new()))
        }
        Command::Status => diagnostics::print_status(&cfg),
        Command::Start => block_on(Hotspot::for_uplink(&cfg.hotspot)?.start(&cfg.hotspot)),
        Command::Stop => block_on(Hotspot::for_uplink(&cfg.hotspot)?.stop()),
        Command::Service { .. } => service::install(config_path),
    }
}

/// Run one async operation to completion on a fresh single-threaded tokio runtime. The
/// WinRT tethering calls are the only async work in this app; a current-thread runtime
/// never moves their futures across OS threads, which keeps COM apartment affinity out of
/// the picture entirely.
pub(crate) fn block_on<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .context("building the tokio runtime")?
        .block_on(future)
}

/// The config file next to the executable. Resolved against the executable's own
/// directory because Task Scheduler does not guarantee a useful working directory.
fn default_config_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the autospot executable")?;
    let dir = exe
        .parent()
        .context("the autospot executable has no parent directory")?;
    Ok(dir.join(config::DEFAULT_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let argv: Vec<&str> = std::iter::once("autospot")
            .chain(args.iter().copied())
            .collect();
        Cli::try_parse_from(argv).unwrap()
    }

    fn service(action: ServiceAction) -> Option<Command> {
        Some(Command::Service { action })
    }

    #[test]
    fn defaults_to_running_the_watchdog() {
        let cli = parse(&[]);
        assert_eq!(cli.command, None);
        assert_eq!(cli.config, None);
        assert_eq!(cli.bare_config, None);
    }

    #[test]
    fn recognises_each_command() {
        assert_eq!(parse(&["run"]).command, Some(Command::Run));
        assert_eq!(parse(&["status"]).command, Some(Command::Status));
        assert_eq!(parse(&["start"]).command, Some(Command::Start));
        assert_eq!(parse(&["stop"]).command, Some(Command::Stop));
    }

    #[test]
    fn recognises_each_service_subcommand() {
        assert_eq!(
            parse(&["service", "install"]).command,
            service(ServiceAction::Install)
        );
        assert_eq!(
            parse(&["service", "remove"]).command,
            service(ServiceAction::Remove)
        );
        assert_eq!(
            parse(&["service", "status"]).command,
            service(ServiceAction::Status)
        );
        assert_eq!(
            parse(&["service", "restart"]).command,
            service(ServiceAction::Restart)
        );
    }

    #[test]
    fn service_run_is_hidden_but_still_parses_with_a_config_path() {
        let cli = parse(&["service", "run", "--config", "C:\\autospot\\autospot.toml"]);
        assert_eq!(cli.command, service(ServiceAction::Run));
        assert_eq!(
            cli.config,
            Some(PathBuf::from("C:\\autospot\\autospot.toml"))
        );
    }

    #[test]
    fn top_level_status_is_not_confused_with_service_status() {
        assert_eq!(parse(&["status"]).command, Some(Command::Status));
        assert_ne!(parse(&["status"]).command, service(ServiceAction::Status));
    }

    #[test]
    fn takes_a_config_path_as_a_flag_or_a_bare_argument() {
        assert_eq!(
            parse(&["--config", "C:\\autospot\\autospot.toml"]).config,
            Some(PathBuf::from("C:\\autospot\\autospot.toml"))
        );
        assert_eq!(
            parse(&["C:\\autospot\\autospot.toml"]).bare_config,
            Some(PathBuf::from("C:\\autospot\\autospot.toml"))
        );
    }

    #[test]
    fn config_flag_works_alongside_a_subcommand() {
        let cli = parse(&["status", "--config", "other.toml"]);
        assert_eq!(cli.command, Some(Command::Status));
        assert_eq!(cli.config, Some(PathBuf::from("other.toml")));
    }

    #[test]
    fn help_short_circuits() {
        use clap::error::ErrorKind;
        assert_eq!(
            Cli::try_parse_from(["autospot", "--help"])
                .unwrap_err()
                .kind(),
            ErrorKind::DisplayHelp
        );
        assert_eq!(
            Cli::try_parse_from(["autospot", "-h"]).unwrap_err().kind(),
            ErrorKind::DisplayHelp
        );
    }

    #[test]
    fn rejects_unknown_options_and_a_dangling_config_flag() {
        assert!(Cli::try_parse_from(["autospot", "--nope"]).is_err());
        assert!(Cli::try_parse_from(["autospot", "--config"]).is_err());
    }
}
