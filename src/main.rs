//! Autospot -- Wi-Fi watchdog that turns Windows' Mobile Hotspot on when Wi-Fi stays down.
//!
//! See `docs/implementation.md` for the design and `docs/status.md` for build status.

mod adapters;
mod blocking;
mod companion;
mod config;
mod diagnostics;
mod hotspot;
mod monitor;
mod service;
mod status;
mod watchdog;
mod wifi;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use tracing::error;
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

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
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Run);

    match &command {
        // The Service Control Manager launches us with this hidden variant; it does its
        // own config loading and logging setup and never returns until the service stops.
        Command::Service {
            action: ServiceAction::Run,
        } => {
            let config_path = match cli.config.or(cli.bare_config) {
                Some(path) => path,
                None => default_config_path()?,
            };
            return service::run_dispatcher(config_path);
        }
        // Neither needs a config file to exist.
        Command::Service {
            action: ServiceAction::Remove,
        } => return service::remove(),
        Command::Service {
            action: ServiceAction::Status,
        } => return service::print_status(),
        _ => {}
    }

    let config_path = match cli.config.or(cli.bare_config) {
        Some(path) => path,
        None => default_config_path()?,
    };
    let cfg = Config::load(&config_path)?;

    // `status` is a one-shot diagnostic command; file logging would only get in the way.
    let _guard = if command == Command::Status {
        init_console_logging(&cfg);
        None
    } else {
        Some(init_logging(&cfg)?)
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
            monitor::run(&cfg, &config_path, &AtomicBool::new(false))
        }
        Command::Status => diagnostics::print_status(&cfg),
        Command::Start => {
            let hotspot = Hotspot::for_uplink(&cfg.hotspot)?;
            hotspot.start(&cfg.hotspot)
        }
        Command::Stop => {
            let hotspot = Hotspot::for_uplink(&cfg.hotspot)?;
            hotspot.stop()
        }
        Command::Service {
            action: ServiceAction::Install,
        } => service::install(&config_path),
        Command::Service { .. } => unreachable!("Remove/Status/Run handled above"),
    }
}

/// Directory holding the executable; the default config path resolves against it
/// because Task Scheduler does not guarantee a useful working directory. Log paths use
/// `user_documents_log_dir`/`service::program_data_log_dir` instead -- see there for why.
fn exe_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the autospot executable")?;
    Ok(exe
        .parent()
        .context("the autospot executable has no parent directory")?
        .to_path_buf())
}

fn default_config_path() -> Result<PathBuf> {
    Ok(exe_dir()?.join(config::DEFAULT_FILE_NAME))
}

/// Base directory for log files in console/application mode:
/// `%USERPROFILE%\Documents\autospot\logs`. Falls back to the current directory if
/// `USERPROFILE` isn't set, rather than failing outright. Service mode uses a different
/// base -- see `service::program_data_log_dir`.
fn user_documents_log_dir() -> PathBuf {
    match std::env::var_os("USERPROFILE") {
        Some(profile) => PathBuf::from(profile)
            .join("Documents")
            .join("autospot")
            .join("logs"),
        None => PathBuf::from("."),
    }
}

fn init_console_logging(cfg: &Config) {
    let level = config::parse_level(&cfg.logging.stdout_level).unwrap_or(tracing::Level::INFO);
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .try_init();
}

/// Log to a daily-rotated file under `base_dir` (a relative `cfg.logging.path` resolves
/// against it; an absolute one overrides it entirely), optionally also to the console.
/// The file's level depends on mode: `file_level` in console mode, where the console is
/// the primary place output is read live and the file defaults to `error` so it doesn't
/// fill up with routine status lines; `service_level` in service mode (`with_stdout`
/// `false`), where there is no console and the file is the only place output is read.
pub(crate) fn init_file_logging(
    cfg: &Config,
    base_dir: &Path,
    with_stdout: bool,
) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let file_level = if with_stdout {
        config::parse_level(&cfg.logging.file_level).unwrap_or(tracing::Level::ERROR)
    } else {
        config::parse_level(&cfg.logging.service_level).unwrap_or(tracing::Level::INFO)
    };

    let path = if cfg.logging.path.is_absolute() {
        cfg.logging.path.clone()
    } else {
        base_dir.join(&cfg.logging.path)
    };
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = path
        .file_name()
        .context("logging.path has no file name")?
        .to_owned();

    std::fs::create_dir_all(&directory)
        .with_context(|| format!("creating the log directory {}", directory.display()))?;

    let appender = tracing_appender::rolling::daily(&directory, &file_name);
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_target(false)
        .with_filter(tracing_subscriber::filter::LevelFilter::from_level(
            file_level,
        ));
    let stdout_layer = with_stdout.then(|| {
        let stdout_level =
            config::parse_level(&cfg.logging.stdout_level).unwrap_or(tracing::Level::INFO);
        tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_ansi(false)
            .with_filter(tracing_subscriber::filter::LevelFilter::from_level(
                stdout_level,
            ))
    });

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .init();

    Ok(guard)
}

fn init_logging(cfg: &Config) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    init_file_logging(cfg, &user_documents_log_dir(), true)
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
            Some(Command::Service {
                action: ServiceAction::Install
            })
        );
        assert_eq!(
            parse(&["service", "remove"]).command,
            Some(Command::Service {
                action: ServiceAction::Remove
            })
        );
        assert_eq!(
            parse(&["service", "status"]).command,
            Some(Command::Service {
                action: ServiceAction::Status
            })
        );
    }

    #[test]
    fn service_run_is_hidden_but_still_parses_with_a_config_path() {
        let cli = parse(&["service", "run", "--config", "C:\\autospot\\autospot.toml"]);
        assert_eq!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Run
            })
        );
        assert_eq!(cli.config, Some(PathBuf::from("C:\\autospot\\autospot.toml")));
    }

    #[test]
    fn top_level_status_is_not_confused_with_service_status() {
        assert_eq!(parse(&["status"]).command, Some(Command::Status));
        assert_ne!(
            parse(&["status"]).command,
            Some(Command::Service {
                action: ServiceAction::Status
            })
        );
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
