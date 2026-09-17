//! The watchdog loop: polls Wi-Fi state every tick, drives the hotspot on and off in
//! response, and prints the console status line and optional companion status update.

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::adapters::{self, Adapter};
use crate::companion::CompanionConn;
use crate::config::Config;
use crate::hotspot::{Hotspot, State};
use crate::status::{self, Status};
use crate::watchdog::{Intent, Watchdog};
use crate::wifi;

/// The watchdog loop.
pub fn run(cfg: &Config, config_path: &Path) -> Result<()> {
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
    // What was last sent to the companion, so only the changed fields need re-sending.
    let mut last_status = Status::default();
    // When the "s" field last actually changed, so outgoing updates can tell the
    // companion how long the current state has held.
    let mut last_state_change = Instant::now();
    let mut log_missing_companion = true;

    loop {
        let now = Instant::now();

        let (wifi_ok, status) = match wifi::query() {
            Ok(status) => (true, status),
            // Treat an unreadable Wi-Fi stack as "state unknown": still worth a companion
            // status update (status="unknown"), but not worth feeding a guess into the watchdog.
            Err(e) => {
                warn!("could not read Wi-Fi state: {e:#}");
                (false, wifi::WifiStatus::default())
            }
        };

        // Best-effort snapshots shared by the console status line and the companion
        // status update; a failure here (e.g. a transient IP Helper hiccup) should never
        // stop the loop.
        let adapters_snapshot = adapters::list().unwrap_or_default();
        let live_hotspot = query_hotspot(cfg);

        if cfg.companion.enabled {
            update_companion_status(
                cfg,
                now,
                wifi_ok,
                &status,
                ignore_ssid,
                &adapters_snapshot,
                &live_hotspot,
                &mut last_status,
                &mut last_state_change,
                &mut log_missing_companion,
            );
        }

        if !wifi_ok {
            std::thread::sleep(cfg.poll_interval());
            continue;
        }

        let connected = status.is_connected(ignore_ssid);
        let intent = watchdog.evaluate(now, connected);

        info!(
            "{}",
            status_line(
                &status,
                &adapters_snapshot,
                ignore_ssid,
                &intent,
                &live_hotspot
            )
        );

        match intent {
            Intent::Idle | Intent::Waiting { .. } | Intent::HoldingForRetry { .. } => {}
            Intent::WifiLost => {
                info!(
                    threshold_secs = cfg.monitor.disconnect_threshold_secs,
                    "Wi-Fi disconnected; starting countdown"
                );
            }
            Intent::StartHotspot { down_for } => {
                info!(
                    down_for_secs = down_for.as_secs(),
                    "Wi-Fi has been down past the threshold; bringing the hotspot up"
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
                    "Wi-Fi reconnected"
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

/// Compute this tick's status and send only what changed since `last_status` to the
/// companion microcontroller over serial, tagged with "t": seconds since the "s" field
/// itself last changed -- e.g. how long Wi-Fi has been disconnected. Resolves the serial
/// port fresh on every call (see `CompanionConn::new`) so a companion that is unplugged
/// and replugged -- possibly under a different COM port -- is still found. Updates
/// `last_status`/`last_state_change` on every call, even when there is no companion to
/// send to, so both stay accurate for whenever one shows up.
fn update_companion_status(
    cfg: &Config,
    now: Instant,
    wifi_ok: bool,
    status: &wifi::WifiStatus,
    ignore_ssid: Option<&str>,
    adapters: &[Adapter],
    live_hotspot: &Option<LiveHotspot>,
    last_status: &mut Status,
    last_state_change: &mut Instant,
    log_missing_companion: &mut bool,
) {
    let wifi_snapshot = match status.active_interface(ignore_ssid) {
        Some(iface) => status::WifiSnapshot {
            wifi_ok,
            connected: true,
            ssid: iface.ssid.as_deref(),
            ip_address: adapters::ipv4_of(adapters, &iface.description),
        },
        None => status::WifiSnapshot {
            wifi_ok,
            connected: false,
            ssid: None,
            ip_address: None,
        },
    };
    let hotspot_snapshot = live_hotspot.as_ref().map(|h| status::HotspotSnapshot {
        on: h.state == State::On,
        ssid: &cfg.hotspot.ssid,
        password: &cfg.hotspot.passphrase,
        ip_address: adapters::hotspot_ip(adapters),
    });

    let new_status = Status::new(&wifi_snapshot, hotspot_snapshot.as_ref());
    let changed = new_status.diff(last_status);
    *last_status = new_status;

    if changed.contains_key("s") {
        *last_state_change = now;
    }
    let since_state_change = now.duration_since(*last_state_change).as_secs();
    let to_send = last_status.with_field("t", since_state_change.to_string());

    let Some(companion) = CompanionConn::new(cfg.companion) else {
        if *log_missing_companion {
            warn!("no companion device found");
            *log_missing_companion = false;
        }
        return;
    };
    *log_missing_companion = true;
    if let Err(e) = companion.write_status(&to_send) {
        error!("could not send status to companion: {e:#}");
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
            adapters::ipv4_of(adapters, &iface.description).unwrap_or("no IP yet"),
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

/// Live hotspot state, queried once per tick and shared by the console status line and
/// the companion status update so neither reads Windows twice.
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
    Some(LiveHotspot {
        state,
        ssid,
        clients,
    })
}

/// Render a `LiveHotspot` snapshot for the console status line. Pure, so it is unit
/// tested directly.
fn hotspot_summary_text(live: &Option<LiveHotspot>, adapters: &[Adapter]) -> String {
    match live {
        None => "off".to_string(),
        Some(h) => match h.state {
            State::On => match adapters::hotspot_ip(adapters) {
                Some(ip) => format!("on (ssid='{}', ip={ip}, clients={})", h.ssid, h.clients),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn adapter(friendly: &str, description: &str, ipv4: &[&str]) -> Adapter {
        Adapter {
            guid: windows::core::GUID::zeroed(),
            friendly_name: friendly.into(),
            description: description.into(),
            is_ethernet: false,
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
}
