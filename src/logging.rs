//! Where log output goes, and at which level.
//!
//! Two modes, because the two ways of running autospot are read in different places:
//!
//! ```text
//!              log directory                             file level      console
//!   Console    %USERPROFILE%\Documents\autospot\logs     logging.file_level    yes
//!   Service    %ProgramData%\autospot\logs               logging.service_level no
//! ```
//!
//! In console mode the console is the primary place output is read live, so the file
//! defaults to `error` and doesn't fill up with routine status lines. As a service there
//! is no console, so the file is the only place output is read and defaults to `info`.
//! Neither uses the working directory: Task Scheduler and the Service Control Manager
//! don't guarantee a useful one.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::config::{self, Config};

/// How autospot is running, which is what decides where the log file lives and how much
/// goes into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Console,
    Service,
}

impl Mode {
    /// Directory a relative `logging.path` is resolved against. Both fall back to the
    /// current directory if the environment variable isn't set, rather than failing.
    fn log_dir(self) -> PathBuf {
        let (root_var, subdirs) = match self {
            Mode::Console => ("USERPROFILE", ["Documents", "autospot", "logs"].as_slice()),
            // Machine-wide, writable by LocalSystem, and easy to find regardless of which
            // account the service runs as.
            Mode::Service => ("ProgramData", ["autospot", "logs"].as_slice()),
        };
        match std::env::var_os(root_var) {
            Some(root) => PathBuf::from(root).join(subdirs.iter().collect::<PathBuf>()),
            None => PathBuf::from("."),
        }
    }

    /// Level for the log file, and the default when the configured name is unrecognised
    /// (which `Config` validation rejects anyway).
    fn file_level(self, cfg: &Config) -> Level {
        match self {
            Mode::Console => level(&cfg.logging.file_level, Level::ERROR),
            Mode::Service => level(&cfg.logging.service_level, Level::INFO),
        }
    }
}

/// Start logging to a daily-rotated file, and to the console in [`Mode::Console`].
/// The returned guard flushes the file writer when dropped, so callers must hold it for
/// as long as they want logs.
pub fn init(cfg: &Config, mode: Mode) -> Result<WorkerGuard> {
    let path = resolve_path(&cfg.logging.path, &mode.log_dir());
    // A bare "autospot.log" has no parent to speak of; write it where we stand.
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
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
        .with_filter(LevelFilter::from_level(mode.file_level(cfg)));

    let stdout_layer = (mode == Mode::Console).then(|| {
        tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_ansi(false)
            .with_filter(LevelFilter::from_level(stdout_level(cfg)))
    });

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .init();

    Ok(guard)
}

/// Log to the console only, for one-shot commands where a log file would just get in the
/// way. Failing to install the subscriber is not worth reporting: there is nothing left
/// for it to do.
pub fn init_console_only(cfg: &Config) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(stdout_level(cfg))
        .with_target(false)
        .try_init();
}

/// A relative `logging.path` lands under the mode's log directory; an absolute one
/// overrides it entirely.
fn resolve_path(configured: &Path, log_dir: &Path) -> PathBuf {
    if configured.is_absolute() {
        return configured.to_path_buf();
    }
    log_dir.join(configured)
}

fn stdout_level(cfg: &Config) -> Level {
    level(&cfg.logging.stdout_level, Level::INFO)
}

fn level(configured: &str, fallback: Level) -> Level {
    config::parse_level(configured).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_log_path_ignores_the_mode_directory() {
        let configured = Path::new("C:\\logs\\autospot.log");
        assert_eq!(
            resolve_path(configured, Path::new("C:\\ProgramData\\autospot\\logs")),
            configured
        );
    }

    #[test]
    fn a_relative_log_path_lands_under_the_mode_directory() {
        assert_eq!(
            resolve_path(Path::new("autospot.log"), Path::new("C:\\base")),
            Path::new("C:\\base\\autospot.log")
        );
    }

    #[test]
    fn console_and_service_modes_read_different_levels() {
        let cfg = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[logging]
file_level = "warn"
service_level = "debug"
"#,
        )
        .unwrap();
        assert_eq!(Mode::Console.file_level(&cfg), Level::WARN);
        assert_eq!(Mode::Service.file_level(&cfg), Level::DEBUG);
    }

    #[test]
    fn each_mode_logs_under_its_own_directory() {
        let console = Mode::Console.log_dir();
        let service = Mode::Service.log_dir();
        assert_ne!(console, service);
        for dir in [console, service] {
            assert!(dir.ends_with("logs") || dir == Path::new("."), "{dir:?}");
        }
    }
}
