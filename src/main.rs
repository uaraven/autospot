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
mod status;
mod watchdog;
mod wifi;

use std::path::{Path, PathBuf};

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
        Command::Run => monitor::run(&cfg, &config_path),
        Command::Status => diagnostics::print_status(&cfg),
        Command::Start => {
            let hotspot = Hotspot::for_uplink(&cfg.hotspot.uplink_adapter)?;
            hotspot.start(&cfg.hotspot)
        }
        Command::Stop => {
            let hotspot = Hotspot::for_uplink(&cfg.hotspot.uplink_adapter)?;
            hotspot.stop()
        }
    }
}

/// Directory holding the executable; config and relative log paths resolve against it
/// because Task Scheduler does not guarantee a useful working directory.
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

fn init_console_logging(cfg: &Config) {
    let level = config::parse_level(&cfg.logging.stdout_level).unwrap_or(tracing::Level::INFO);
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .try_init();
}

/// Log to a daily-rotated file next to the executable, and always to the console too:
/// this is a foreground console app, not a background service, so the console is the
/// primary place its output is meant to be read. The file and console keep independent
/// levels -- the file defaults to `error` so it doesn't fill up with routine status
/// lines that are only useful live, on the console.
fn init_logging(cfg: &Config) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let file_level = config::parse_level(&cfg.logging.file_level).unwrap_or(tracing::Level::ERROR);
    let stdout_level =
        config::parse_level(&cfg.logging.stdout_level).unwrap_or(tracing::Level::INFO);

    let path = if cfg.logging.path.is_absolute() {
        cfg.logging.path.clone()
    } else {
        exe_dir()?.join(&cfg.logging.path)
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
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_filter(tracing_subscriber::filter::LevelFilter::from_level(
            stdout_level,
        ));

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .init();

    Ok(guard)
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
