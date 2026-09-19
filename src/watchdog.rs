//! The watchdog loop: polls Wi-Fi state every tick, drives the hotspot on and off in
//! response, and prints the console status line and optional companion status update.

use std::path::Path;
use std::time::Instant;

use anyhow::{Result, bail};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use crate::adapters::{self, Adapter};
use crate::companion::CompanionSession;
use crate::config::Config;
use crate::hotspot::{Hotspot, State};
use crate::policy::{Intent, OnReconnect, Policy};
use crate::status::{self, Status};
use crate::wifi;

/// The watchdog loop. Runs until `stop` is notified; pass a `Notify` that's never
/// notified to run forever, as the console `run` command does.
pub async fn run(cfg: &Config, config_path: &Path, stop: &Notify) -> Result<()> {
    Watchdog::new(cfg).run(config_path, stop).await
}

/// The watchdog loop's persistent state, carried across ticks.
struct Watchdog<'a> {
    cfg: &'a Config,
    /// Our own hotspot's SSID, which never counts as Wi-Fi being back -- see
    /// [`wifi::WifiStatus::active_interface`].
    ignore_ssid: Option<&'a str>,
    policy: Policy,
    companion: CompanionSession,
}

impl<'a> Watchdog<'a> {
    fn new(cfg: &'a Config) -> Self {
        let on_reconnect = if cfg.monitor.auto_disable_on_reconnect {
            OnReconnect::StopHotspot
        } else {
            OnReconnect::LeaveHotspotOn
        };

        Self {
            cfg,
            ignore_ssid: Some(cfg.hotspot.ssid.as_str()),
            policy: Policy::new(cfg.monitor.disconnect_threshold(), on_reconnect),
            companion: CompanionSession::new(cfg.companion),
        }
    }

    async fn run(&mut self, config_path: &Path, stop: &Notify) -> Result<()> {
        info!(
            version = env!("CARGO_PKG_VERSION"),
            config = %config_path.display(),
            ssid = %self.cfg.hotspot.ssid,
            uplink = %self.cfg.hotspot.uplink_adapter,
            poll_interval_secs = self.cfg.monitor.poll_interval_secs,
            disconnect_threshold_secs = self.cfg.monitor.disconnect_threshold_secs,
            auto_disable_on_reconnect = self.cfg.monitor.auto_disable_on_reconnect,
            "autospot starting"
        );

        loop {
            self.tick(Instant::now()).await;

            // Wait out the poll interval, unless we are asked to stop first.
            tokio::select! {
                _ = tokio::time::sleep(self.cfg.monitor.poll_interval()) => {}
                _ = stop.notified() => break,
            }
        }

        info!("autospot stopping");
        Ok(())
    }

    /// One poll: read where things stand, report it, and act on it.
    async fn tick(&mut self, now: Instant) {
        let (wifi_ok, status) = match wifi::query() {
            Ok(status) => (true, status),
            // Treat an unreadable Wi-Fi stack as "state unknown": still worth a companion
            // status update (status="unknown"), but not worth feeding a guess into the
            // policy.
            Err(e) => {
                warn!("could not read Wi-Fi state: {e:#}");
                (false, wifi::WifiStatus::default())
            }
        };

        // Best-effort snapshots shared by the console status line and the companion
        // status update; a failure here (e.g. a transient IP Helper hiccup) should never
        // stop the loop.
        let adapters = adapters::list().unwrap_or_default();
        let live_hotspot = query_hotspot(self.cfg);

        if self.cfg.companion.enabled {
            let report = self.companion_status(wifi_ok, &status, &adapters, live_hotspot.as_ref());
            self.companion.report(now, report);
        }

        if !wifi_ok {
            return;
        }

        let intent = self
            .policy
            .evaluate(now, status.is_connected(self.ignore_ssid));

        info!(
            "{} | Hotspot: {}",
            wifi_status_text(&status, &adapters, self.ignore_ssid, &intent),
            hotspot_summary_text(live_hotspot.as_ref(), &adapters)
        );

        self.handle_intent(now, &status, intent).await;
    }

    /// React to what the policy decided this tick: log it, and bring the hotspot up or
    /// down if that's what the intent calls for.
    async fn handle_intent(&mut self, now: Instant, status: &wifi::WifiStatus, intent: Intent) {
        match intent {
            Intent::Idle | Intent::Waiting { .. } | Intent::HoldingForRetry { .. } => {}
            Intent::WifiLost => {
                info!(
                    threshold_secs = self.cfg.monitor.disconnect_threshold_secs,
                    "Wi-Fi disconnected; starting countdown"
                );
            }
            Intent::StartHotspot { down_for } => {
                info!(
                    down_for_secs = down_for.as_secs(),
                    "Wi-Fi has been down past the threshold; bringing the hotspot up"
                );
                if let Err(e) = self.try_start(status).await {
                    error!("could not start the hotspot: {e:#}");
                    self.policy.record_start_failure(now);
                }
            }
            Intent::WifiRestored {
                down_for,
                stop_hotspot,
            } => {
                info!(
                    down_for_secs = down_for.as_secs(),
                    ssid = ?status.connected_ssid(self.ignore_ssid),
                    "Wi-Fi reconnected"
                );
                // Keep ownership on failure so the next reconnect tick tries again.
                if stop_hotspot && let Err(e) = self.try_stop().await {
                    error!("could not stop the hotspot: {e:#}");
                }
            }
        }
    }

    /// Bring the hotspot up, unless it is already up or mid-transition.
    async fn try_start(&mut self, wifi_status: &wifi::WifiStatus) -> Result<()> {
        if !wifi_status.radio_enabled() {
            bail!(
                "Wi-Fi is turned off; not starting the hotspot (Mobile Hotspot needs the \
                 Wi-Fi radio on to broadcast)"
            );
        }

        let hotspot = Hotspot::for_uplink(&self.cfg.hotspot)?;

        match hotspot.state()? {
            State::On => {
                // Only worth mentioning if it's broadcasting an SSID we didn't configure
                // -- if it matches ours, this is just our own hotspot, started on an
                // earlier tick, still running. Somebody else's stays under manual
                // control, including on reconnect.
                let is_ours = hotspot
                    .current_ssid()
                    .is_ok_and(|ssid| ssid == self.cfg.hotspot.ssid);
                if !is_ours {
                    info!(
                        uplink = %hotspot.uplink_profile,
                        "hotspot is already on with a different configuration; leaving it under manual control"
                    );
                }
                Ok(())
            }
            State::InTransition => {
                debug!("hotspot is mid-transition; waiting for it to settle");
                Ok(())
            }
            State::Off | State::Unknown => {
                hotspot.start(&self.cfg.hotspot).await?;
                self.policy.record_started();
                Ok(())
            }
        }
    }

    /// Take the hotspot back down after Wi-Fi returns.
    async fn try_stop(&mut self) -> Result<()> {
        let hotspot = Hotspot::for_uplink(&self.cfg.hotspot)?;

        match hotspot.state()? {
            State::Off => {
                debug!("hotspot is already off");
                self.policy.record_stopped();
                Ok(())
            }
            State::InTransition => {
                debug!("hotspot is mid-transition; will stop it on the next poll");
                Ok(())
            }
            State::On | State::Unknown => {
                hotspot.stop().await?;
                self.policy.record_stopped();
                Ok(())
            }
        }
    }

    /// Build this tick's status for the companion, from the wifi/adapter/hotspot
    /// snapshots already gathered for the console status line.
    fn companion_status(
        &self,
        wifi_ok: bool,
        status: &wifi::WifiStatus,
        adapters: &[Adapter],
        live_hotspot: Option<&LiveHotspot>,
    ) -> Status {
        let active = status.active_interface(self.ignore_ssid);
        let wifi = status::WifiSnapshot {
            wifi_ok,
            radio_enabled: status.radio_enabled(),
            connected: active.is_some(),
            ssid: active.and_then(|iface| iface.ssid.as_deref()),
            ip_address: active.and_then(|iface| adapters::ipv4_of(adapters, &iface.description)),
        };
        let hotspot = live_hotspot.map(|h| status::HotspotSnapshot {
            on: h.state == State::On,
            ssid: &self.cfg.hotspot.ssid,
            password: &self.cfg.hotspot.passphrase,
            ip_address: adapters::hotspot_ip(adapters),
        });

        Status::new(&wifi, hotspot.as_ref())
    }
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
    let hotspot = Hotspot::for_uplink(&cfg.hotspot)
        .inspect_err(|e| debug!("hotspot status unavailable: {e:#}"))
        .ok()?;
    let state = hotspot
        .state()
        .inspect_err(|e| debug!("could not read hotspot state: {e:#}"))
        .ok()?;

    Some(LiveHotspot {
        state,
        ssid: hotspot
            .current_ssid()
            .unwrap_or_else(|_| cfg.hotspot.ssid.clone()),
        clients: hotspot.client_count().unwrap_or(0),
    })
}

/// The Wi-Fi half of the status line: connection, IP, and (while waiting) a countdown.
/// Kept free of any Windows call of its own so it can be unit tested directly.
fn wifi_status_text(
    status: &wifi::WifiStatus,
    adapters: &[Adapter],
    ignore_ssid: Option<&str>,
    intent: &Intent,
) -> String {
    let wifi = match status.active_interface(ignore_ssid) {
        Some(iface) => format!(
            "Wi-Fi: connected to '{}' ({})",
            iface.ssid.as_deref().unwrap_or("?"),
            adapters::ipv4_of(adapters, &iface.description).unwrap_or("no IP yet"),
        ),
        None => "Wi-Fi: disconnected".to_string(),
    };

    let countdown = match intent {
        Intent::Waiting { remaining } => format!(" (hotspot in {}s)", remaining.as_secs()),
        Intent::HoldingForRetry { retry_in } => {
            format!(" (retrying hotspot start in {}s)", retry_in.as_secs())
        }
        _ => String::new(),
    };

    format!("{wifi}{countdown}")
}

/// The hotspot half of the status line. Pure, so it is unit tested directly.
fn hotspot_summary_text(live: Option<&LiveHotspot>, adapters: &[Adapter]) -> String {
    let Some(hotspot) = live else {
        return "off".to_string();
    };

    match hotspot.state {
        State::On => match adapters::hotspot_ip(adapters) {
            Some(ip) => format!(
                "on (ssid='{}', ip={ip}, clients={})",
                hotspot.ssid, hotspot.clients
            ),
            None => format!("on (ssid='{}', clients={})", hotspot.ssid, hotspot.clients),
        },
        State::InTransition => "starting/stopping".to_string(),
        State::Off => "off".to_string(),
        State::Unknown => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_config() -> Config {
        Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"
"#,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn run_returns_immediately_when_already_told_to_stop() {
        let cfg = test_config();
        let stop = Notify::new();
        stop.notify_one();
        let path = Path::new("autospot.toml");
        assert!(run(&cfg, path, &stop).await.is_ok());
    }

    fn connected_status(description: &str, ssid: &str) -> wifi::WifiStatus {
        wifi::WifiStatus {
            interfaces: vec![wifi::InterfaceStatus {
                description: description.into(),
                connected: true,
                ssid: Some(ssid.into()),
                radio_enabled: true,
            }],
        }
    }

    fn disconnected_status() -> wifi::WifiStatus {
        wifi::WifiStatus { interfaces: vec![] }
    }

    #[test]
    fn wifi_status_text_shows_ssid_and_ip_when_connected() {
        let status = connected_status("Intel(R) Wireless-AC 7260", "banderstadt");
        let adapters = [Adapter::test(1)
            .named("Wi-Fi", "Intel(R) Wireless-AC 7260")
            .with_ipv4(&["192.168.10.218"])];
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
    fn hotspot_summary_reports_off_when_there_is_nothing_to_report() {
        assert_eq!(hotspot_summary_text(None, &[]), "off");
    }

    #[test]
    fn hotspot_summary_reports_ssid_ip_and_clients_when_on() {
        let live = LiveHotspot {
            state: State::On,
            ssid: "Fallback".to_string(),
            clients: 2,
        };
        let adapters = [Adapter::test(1).with_ipv4(&["192.168.137.1"])];
        assert_eq!(
            hotspot_summary_text(Some(&live), &adapters),
            "on (ssid='Fallback', ip=192.168.137.1, clients=2)"
        );
        // Without the ICS address there is no IP to show yet.
        assert_eq!(
            hotspot_summary_text(Some(&live), &[]),
            "on (ssid='Fallback', clients=2)"
        );
    }
}
