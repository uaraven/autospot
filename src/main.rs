//! Autospot -- Wi-Fi watchdog that turns Windows' Mobile Hotspot on when Wi-Fi stays down.
//!
//! See `docs/implementation.md` for the design and `docs/status.md` for build status.

mod adapters;
mod blocking;
mod config;
mod hotspot;
mod status_dump;
mod watchdog;
mod wifi;

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, Result};
use tracing::{debug, error, info, warn};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::adapters::Adapter;
use crate::config::Config;
use crate::hotspot::{Hotspot, State};
use crate::watchdog::{Intent, Watchdog};

const USAGE: &str = "\
Autospot -- Wi-Fi watchdog + automatic Mobile Hotspot

A portable, foreground console app: run it and leave the window open. It prints its
status -- Wi-Fi connection, IP address, hotspot state -- every few seconds, and an
explicit line for every action it takes. Stop it with Ctrl+C.

USAGE:
    autospot [COMMAND] [--config <PATH>]

COMMANDS:
    run       Watch Wi-Fi and manage the hotspot (default)
    status    Print Wi-Fi, adapter and hotspot state, then exit
    start     Turn the hotspot on once, using the config, then exit
    stop      Turn the hotspot off once, then exit

OPTIONS:
    --config <PATH>   Config file (default: autospot.toml next to the executable)
    -h, --help        Show this help
";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run,
    Status,
    Start,
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
    let (command, config_path) = match parse_args(std::env::args().skip(1))? {
        Some(parsed) => parsed,
        None => {
            print!("{USAGE}");
            return Ok(());
        }
    };

    let config_path = match config_path {
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
        Command::Run => run(&cfg, &config_path),
        Command::Status => print_status(&cfg),
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

/// Returns `None` when help was requested.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<(Command, Option<PathBuf>)>> {
    let mut command = Command::Run;
    let mut config_path = None;
    let mut args = args.peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" | "help" => return Ok(None),
            "run" => command = Command::Run,
            "status" => command = Command::Status,
            "start" => command = Command::Start,
            "stop" => command = Command::Stop,
            "--config" | "-c" => {
                let path = args
                    .next()
                    .context("--config needs a path argument")?;
                config_path = Some(PathBuf::from(path));
            }
            other if other.starts_with('-') => {
                anyhow::bail!("unknown option '{other}'\n\n{USAGE}");
            }
            // A bare path is accepted so `autospot C:\path\autospot.toml` works.
            other => config_path = Some(PathBuf::from(other)),
        }
    }

    Ok(Some((command, config_path)))
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
    let level = config::parse_level(&cfg.logging.level).unwrap_or(tracing::Level::INFO);
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .try_init();
}

/// Log to a daily-rotated file next to the executable, and always to the console too:
/// this is a foreground console app, not a background service, so the console is the
/// primary place its output is meant to be read.
fn init_logging(cfg: &Config) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let level = config::parse_level(&cfg.logging.level).unwrap_or(tracing::Level::INFO);

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
        .with_target(false);
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false);

    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::from_level(level))
        .with(file_layer)
        .with(stdout_layer)
        .init();

    Ok(guard)
}

/// The watchdog loop.
fn run(cfg: &Config, config_path: &Path) -> Result<()> {
    info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %config_path.display(),
        ssid = %cfg.hotspot.ssid,
        uplink = %cfg.hotspot.uplink_adapter,
        poll_interval_secs = cfg.monitor.poll_interval_secs,
        disconnect_threshold_secs = cfg.monitor.disconnect_threshold_secs,
        auto_disable_on_reconnect = cfg.monitor.auto_disable_on_reconnect,
        "autospot starting"
    );

    let mut watchdog = Watchdog::new(
        cfg.disconnect_threshold(),
        cfg.monitor.auto_disable_on_reconnect,
    );
    let ignore_ssid = Some(cfg.hotspot.ssid.as_str());
    // Last dump written, so `[status-dump]` only writes on an actual change.
    let mut last_dump: Option<status_dump::StatusDump> = None;

    loop {
        let now = Instant::now();

        let (wifi_ok, status) = match wifi::query() {
            Ok(status) => (true, status),
            // Treat an unreadable Wi-Fi stack as "state unknown": still worth a status
            // dump (status="unknown"), but not worth feeding a guess into the watchdog.
            Err(e) => {
                warn!("could not read Wi-Fi state: {e:#}");
                (false, wifi::WifiStatus::default())
            }
        };

        // Best-effort snapshots shared by the console status line and the status dump; a
        // failure here (e.g. a transient IP Helper hiccup) should never stop the loop.
        let adapters_snapshot = adapters::list().unwrap_or_default();
        let live_hotspot = query_hotspot(cfg);

        if cfg.status_dump.enabled {
            dump_status(cfg, wifi_ok, &status, ignore_ssid, &adapters_snapshot, &live_hotspot, &mut last_dump);
        }

        if !wifi_ok {
            std::thread::sleep(cfg.poll_interval());
            continue;
        }

        let connected = status.is_connected(ignore_ssid);
        let intent = watchdog.evaluate(now, connected);

        info!(
            "{}",
            status_line(&status, &adapters_snapshot, ignore_ssid, &intent, &live_hotspot)
        );

        match intent {
            Intent::Idle | Intent::Waiting { .. } | Intent::HoldingForRetry { .. } => {}
            Intent::WifiLost => {
                info!(
                    threshold_secs = cfg.monitor.disconnect_threshold_secs,
                    "wi-fi disconnected; starting countdown"
                );
            }
            Intent::StartHotspot { down_for } => {
                info!(
                    down_for_secs = down_for.as_secs(),
                    "wi-fi has been down past the threshold; bringing the hotspot up"
                );
                if let Err(e) = try_start(cfg, &mut watchdog) {
                    error!("could not start the hotspot: {e:#}");
                    watchdog.record_start_failure(now);
                }
            }
            Intent::WifiRestored {
                down_for,
                stop_hotspot,
            } => {
                info!(
                    down_for_secs = down_for.as_secs(),
                    ssid = ?status.connected_ssid(ignore_ssid),
                    "wi-fi reconnected"
                );
                if stop_hotspot {
                    if let Err(e) = try_stop(cfg, &mut watchdog) {
                        // Keep ownership so the next reconnect tick tries again.
                        error!("could not stop the hotspot: {e:#}");
                    }
                }
            }
        }

        std::thread::sleep(cfg.poll_interval());
    }
}

/// Compute this tick's `[status-dump]` document and write it if it changed. Errors
/// (most commonly: `disk_label` not currently found) are logged and otherwise ignored --
/// per the config's contract, a dump that cannot be written is skipped, not fatal.
fn dump_status(
    cfg: &Config,
    wifi_ok: bool,
    status: &wifi::WifiStatus,
    ignore_ssid: Option<&str>,
    adapters: &[Adapter],
    live_hotspot: &Option<LiveHotspot>,
    last_dump: &mut Option<status_dump::StatusDump>,
) {
    let wifi_snapshot = match status.active_interface(ignore_ssid) {
        Some(iface) => status_dump::WifiSnapshot {
            connected: true,
            ssid: iface.ssid.as_deref(),
            ip_address: ipv4_of(adapters, &iface.description),
        },
        None => status_dump::WifiSnapshot {
            connected: false,
            ssid: None,
            ip_address: None,
        },
    };
    let hotspot_snapshot = live_hotspot.as_ref().map(|h| status_dump::HotspotSnapshot {
        on: h.state == State::On,
        ssid: &cfg.hotspot.ssid,
        password: &cfg.hotspot.passphrase,
        ip_address: hotspot_ip(adapters),
    });

    let dump = status_dump::compute(wifi_ok, &wifi_snapshot, hotspot_snapshot.as_ref());
    match status_dump::write_if_changed(&cfg.status_dump, &dump, last_dump) {
        Ok(true) => info!(status = dump.status, "status dump written"),
        Ok(false) => {}
        Err(e) => error!("could not write status dump: {e:#}"),
    }
}

/// One readable line summarising what is true right now: Wi-Fi state and IP, hotspot
/// state and IP, and (while waiting) a countdown -- printed to the console every tick.
fn status_line(
    status: &wifi::WifiStatus,
    adapters: &[Adapter],
    ignore_ssid: Option<&str>,
    intent: &Intent,
    live_hotspot: &Option<LiveHotspot>,
) -> String {
    format!(
        "{} | Hotspot: {}",
        wifi_status_text(status, adapters, ignore_ssid, intent),
        hotspot_summary_text(live_hotspot, adapters)
    )
}

/// The Wi-Fi half of the status line: connection, IP, and (while waiting) a countdown.
/// Kept free of any Windows call of its own so it can be unit tested directly.
fn wifi_status_text(
    status: &wifi::WifiStatus,
    adapters: &[Adapter],
    ignore_ssid: Option<&str>,
    intent: &Intent,
) -> String {
    let wifi_part = match status.active_interface(ignore_ssid) {
        Some(iface) => format!(
            "Wi-Fi: connected to '{}' ({})",
            iface.ssid.as_deref().unwrap_or("?"),
            ipv4_of(adapters, &iface.description).unwrap_or("no IP yet"),
        ),
        None => "Wi-Fi: disconnected".to_string(),
    };

    let countdown = match intent {
        Intent::Waiting { remaining, .. } => format!(" (hotspot in {}s)", remaining.as_secs()),
        Intent::HoldingForRetry { retry_in } => {
            format!(" (retrying hotspot start in {}s)", retry_in.as_secs())
        }
        _ => String::new(),
    };

    format!("{wifi_part}{countdown}")
}

/// IP address of the adapter with the given hardware description, if it has one.
///
/// `wlanapi`'s interface description and IP Helper's adapter description are both the
/// driver's own text, so they match directly without needing to cross-reference GUIDs.
fn ipv4_of<'a>(adapters: &'a [Adapter], description: &str) -> Option<&'a str> {
    adapters
        .iter()
        .find(|a| a.description.eq_ignore_ascii_case(description))
        .and_then(|a| a.ipv4.first())
        .map(String::as_str)
}

fn hotspot_ip(adapters: &[Adapter]) -> Option<&str> {
    adapters
        .iter()
        .flat_map(|a| a.ipv4.iter())
        .find(|ip| ip.starts_with(crate::adapters::HOTSPOT_SUBNET_PREFIX))
        .map(String::as_str)
}

/// Live hotspot state, queried once per tick and shared by the console status line and
/// the `[status-dump]` computation so neither reads Windows twice.
struct LiveHotspot {
    state: State,
    ssid: String,
    clients: u32,
}

/// Best-effort live hotspot query. `None` collapses to "off" everywhere it is used,
/// since the most common cause -- the uplink has no connection profile right now -- is
/// routine, not worth alarming over every few seconds.
fn query_hotspot(cfg: &Config) -> Option<LiveHotspot> {
    let hotspot = match Hotspot::for_uplink(&cfg.hotspot.uplink_adapter) {
        Ok(h) => h,
        Err(e) => {
            debug!("hotspot status unavailable: {e:#}");
            return None;
        }
    };
    let state = match hotspot.state() {
        Ok(s) => s,
        Err(e) => {
            debug!("could not read hotspot state: {e:#}");
            return None;
        }
    };
    let ssid = hotspot
        .current_ssid()
        .unwrap_or_else(|_| cfg.hotspot.ssid.clone());
    let clients = hotspot.client_count().unwrap_or(0);
    Some(LiveHotspot { state, ssid, clients })
}

/// Render a `LiveHotspot` snapshot for the console status line. Pure, so it is unit
/// tested directly.
fn hotspot_summary_text(live: &Option<LiveHotspot>, adapters: &[Adapter]) -> String {
    match live {
        None => "off".to_string(),
        Some(h) => match h.state {
            State::On => match hotspot_ip(adapters) {
                Some(ip) => format!(
                    "on (ssid='{}', ip={ip}, clients={})",
                    h.ssid, h.clients
                ),
                None => format!("on (ssid='{}', clients={})", h.ssid, h.clients),
            },
            State::InTransition => "starting/stopping".to_string(),
            State::Off => "off".to_string(),
            State::Unknown => "unknown".to_string(),
        },
    }
}

/// Bring the hotspot up, unless it is already up or mid-transition.
fn try_start(cfg: &Config, watchdog: &mut Watchdog) -> Result<()> {
    let hotspot = Hotspot::for_uplink(&cfg.hotspot.uplink_adapter)?;

    match hotspot.state()? {
        State::On => {
            // Somebody else already did it. Leave it alone, including on reconnect.
            info!(
                uplink = %hotspot.uplink_profile,
                "hotspot is already on; leaving it under manual control"
            );
            Ok(())
        }
        State::InTransition => {
            debug!("hotspot is mid-transition; waiting for it to settle");
            Ok(())
        }
        _ => {
            hotspot.start(&cfg.hotspot)?;
            watchdog.record_started();
            Ok(())
        }
    }
}

/// Take the hotspot back down after Wi-Fi returns.
fn try_stop(cfg: &Config, watchdog: &mut Watchdog) -> Result<()> {
    let hotspot = Hotspot::for_uplink(&cfg.hotspot.uplink_adapter)?;

    match hotspot.state()? {
        State::Off => {
            debug!("hotspot is already off");
            watchdog.record_stopped();
            Ok(())
        }
        State::InTransition => {
            debug!("hotspot is mid-transition; will stop it on the next poll");
            Ok(())
        }
        _ => {
            hotspot.stop()?;
            watchdog.record_stopped();
            Ok(())
        }
    }
}

/// One-shot diagnostics, used to verify each piece by hand.
fn print_status(cfg: &Config) -> Result<()> {
    let adapters_snapshot = adapters::list().unwrap_or_default();
    let auto = cfg.hotspot.is_auto_uplink();
    // Resolved once up front so the adapter listing below and the hotspot section agree
    // on which adapter auto mode actually picked.
    let hotspot_result = Hotspot::for_uplink(&cfg.hotspot.uplink_adapter);
    let resolved_label = hotspot_result.as_ref().ok().map(|h| h.uplink_profile.as_str());

    println!("Wi-Fi interfaces:");
    match wifi::query() {
        Ok(status) if status.interfaces.is_empty() => println!("  (none found)"),
        Ok(status) => {
            for i in &status.interfaces {
                let ip = ipv4_of(&adapters_snapshot, &i.description)
                    .map(|ip| format!(", {ip}"))
                    .unwrap_or_default();
                println!(
                    "  {} -- {} {}{ip}",
                    i.description,
                    if i.connected {
                        "connected"
                    } else {
                        "not connected"
                    },
                    i.ssid
                        .as_deref()
                        .map(|s| format!("to '{s}'"))
                        .unwrap_or_default()
                );
            }
            println!(
                "  => watchdog sees Wi-Fi as: {}",
                if status.is_connected(Some(&cfg.hotspot.ssid)) {
                    "CONNECTED"
                } else {
                    "DISCONNECTED"
                }
            );
        }
        Err(e) => println!("  error: {e:#}"),
    }

    println!("\nNetwork adapters:");
    if adapters_snapshot.is_empty() {
        println!("  (none found, or enumeration failed -- see log)");
    }
    for a in &adapters_snapshot {
        let marker = if auto {
            if resolved_label == Some(a.friendly_name.as_str()) {
                " <== auto-selected uplink"
            } else {
                ""
            }
        } else if a.matches(&cfg.hotspot.uplink_adapter) {
            " <== configured uplink"
        } else {
            ""
        };
        let ip = if a.ipv4.is_empty() {
            String::new()
        } else {
            format!(", {}", a.ipv4.join(", "))
        };
        println!("  {} ({}{ip}){marker}", a.friendly_name, a.description);
    }

    if auto {
        println!(
            "\nHotspot (uplink 'auto' -> {}):",
            resolved_label.unwrap_or("?")
        );
    } else {
        println!("\nHotspot (uplink '{}'):", cfg.hotspot.uplink_adapter);
    }
    match hotspot_result {
        Ok(h) => {
            println!("  uplink profile: {}", h.uplink_profile);
            println!(
                "  state:          {}",
                h.state().map(|s| s.to_string()).unwrap_or("?".into())
            );
            println!(
                "  current SSID:   {}",
                h.current_ssid().unwrap_or_else(|_| "?".into())
            );
            println!(
                "  clients:        {}",
                h.client_count()
                    .map(|c| c.to_string())
                    .unwrap_or("?".into())
            );
            println!("  configured SSID: {}", cfg.hotspot.ssid);
        }
        Err(e) => println!("  error: {e:#}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn parse(args: &[&str]) -> Option<(Command, Option<PathBuf>)> {
        parse_args(args.iter().map(|s| s.to_string())).unwrap()
    }

    fn adapter(friendly: &str, description: &str, ipv4: &[&str]) -> Adapter {
        Adapter {
            guid: windows::core::GUID::zeroed(),
            friendly_name: friendly.into(),
            description: description.into(),
            ipv4: ipv4.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn connected_status(description: &str, ssid: &str) -> wifi::WifiStatus {
        wifi::WifiStatus {
            interfaces: vec![wifi::InterfaceStatus {
                description: description.into(),
                connected: true,
                ssid: Some(ssid.into()),
            }],
        }
    }

    fn disconnected_status() -> wifi::WifiStatus {
        wifi::WifiStatus { interfaces: vec![] }
    }

    #[test]
    fn wifi_status_text_shows_ssid_and_ip_when_connected() {
        let status = connected_status("Intel(R) Wireless-AC 7260", "banderstadt");
        let adapters = [adapter(
            "Wi-Fi",
            "Intel(R) Wireless-AC 7260",
            &["192.168.10.218"],
        )];
        assert_eq!(
            wifi_status_text(&status, &adapters, None, &Intent::Idle),
            "Wi-Fi: connected to 'banderstadt' (192.168.10.218)"
        );
    }

    #[test]
    fn wifi_status_text_flags_a_missing_ip_rather_than_hiding_it() {
        let status = connected_status("Intel(R) Wireless-AC 7260", "banderstadt");
        assert_eq!(
            wifi_status_text(&status, &[], None, &Intent::Idle),
            "Wi-Fi: connected to 'banderstadt' (no IP yet)"
        );
    }

    #[test]
    fn wifi_status_text_reports_disconnected() {
        assert_eq!(
            wifi_status_text(&disconnected_status(), &[], None, &Intent::WifiLost),
            "Wi-Fi: disconnected"
        );
    }

    #[test]
    fn wifi_status_text_appends_a_countdown_while_waiting() {
        let intent = Intent::Waiting {
            down_for: Duration::from_secs(45),
            remaining: Duration::from_secs(75),
        };
        assert_eq!(
            wifi_status_text(&disconnected_status(), &[], None, &intent),
            "Wi-Fi: disconnected (hotspot in 75s)"
        );
    }

    #[test]
    fn wifi_status_text_appends_a_retry_countdown() {
        let intent = Intent::HoldingForRetry {
            retry_in: Duration::from_secs(30),
        };
        assert_eq!(
            wifi_status_text(&disconnected_status(), &[], None, &intent),
            "Wi-Fi: disconnected (retrying hotspot start in 30s)"
        );
    }

    #[test]
    fn wifi_status_text_has_no_suffix_for_other_intents() {
        for intent in [
            Intent::Idle,
            Intent::WifiLost,
            Intent::StartHotspot {
                down_for: Duration::from_secs(120),
            },
            Intent::WifiRestored {
                down_for: Duration::from_secs(5),
                stop_hotspot: true,
            },
        ] {
            assert_eq!(
                wifi_status_text(&disconnected_status(), &[], None, &intent),
                "Wi-Fi: disconnected"
            );
        }
    }

    #[test]
    fn ipv4_of_matches_by_hardware_description_case_insensitively() {
        let adapters = [adapter("Ethernet", "Realtek PCIe GbE", &["10.0.0.5"])];
        assert_eq!(ipv4_of(&adapters, "realtek pcie gbe"), Some("10.0.0.5"));
        assert_eq!(ipv4_of(&adapters, "nope"), None);
    }

    #[test]
    fn ipv4_of_is_none_when_the_adapter_has_no_address() {
        let adapters = [adapter("Ethernet", "Realtek PCIe GbE", &[])];
        assert_eq!(ipv4_of(&adapters, "Realtek PCIe GbE"), None);
    }

    #[test]
    fn hotspot_ip_finds_the_ics_subnet_address() {
        let adapters = [
            adapter("Wi-Fi", "desc1", &["192.168.10.218"]),
            adapter("Local Area Connection* 2", "desc2", &["192.168.137.1"]),
        ];
        assert_eq!(hotspot_ip(&adapters), Some("192.168.137.1"));
    }

    #[test]
    fn hotspot_ip_is_none_without_a_matching_subnet() {
        let adapters = [adapter("Wi-Fi", "desc1", &["192.168.10.218"])];
        assert_eq!(hotspot_ip(&adapters), None);
    }

    #[test]
    fn defaults_to_running_the_watchdog() {
        assert_eq!(parse(&[]), Some((Command::Run, None)));
    }

    #[test]
    fn recognises_each_command() {
        assert_eq!(parse(&["run"]).unwrap().0, Command::Run);
        assert_eq!(parse(&["status"]).unwrap().0, Command::Status);
        assert_eq!(parse(&["start"]).unwrap().0, Command::Start);
        assert_eq!(parse(&["stop"]).unwrap().0, Command::Stop);
    }

    #[test]
    fn takes_a_config_path_as_a_flag_or_a_bare_argument() {
        assert_eq!(
            parse(&["--config", "C:\\autospot\\autospot.toml"]).unwrap().1,
            Some(PathBuf::from("C:\\autospot\\autospot.toml"))
        );
        assert_eq!(
            parse(&["status", "other.toml"]).unwrap(),
            (Command::Status, Some(PathBuf::from("other.toml")))
        );
    }

    #[test]
    fn help_short_circuits() {
        assert_eq!(parse(&["--help"]), None);
        assert_eq!(parse(&["-h"]), None);
    }

    #[test]
    fn rejects_unknown_options_and_a_dangling_config_flag() {
        assert!(parse_args(["--nope".to_string()].into_iter()).is_err());
        assert!(parse_args(["--config".to_string()].into_iter()).is_err());
    }
}
