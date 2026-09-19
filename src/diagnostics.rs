//! One-shot diagnostics: the `autospot status` command, used to verify each piece by
//! hand -- Wi-Fi interfaces, adapters, and the hotspot's resolved uplink and state.
//!
//! This prints to stdout rather than logging: it is a report the user asked for, not a
//! record of what the watchdog is doing.

use anyhow::Result;

use crate::adapters::{self, Adapter};
use crate::config::Config;
use crate::hotspot::Hotspot;
use crate::uplink::{self, HotspotAdapter};
use crate::wifi;

pub fn print_status(cfg: &Config) -> Result<()> {
    let adapters = adapters::list().unwrap_or_default();
    // Both resolved once up front so the adapter listing and the hotspot section agree on
    // which adapter auto mode actually picked, and which one is excluded as the hotspot's
    // own adapter.
    let hotspot_adapter = uplink::resolve_hotspot_adapter(&cfg.hotspot.hotspot_adapter, &adapters);
    let hotspot = Hotspot::for_uplink(&cfg.hotspot);
    let uplink_label = hotspot.as_ref().ok().map(|h| h.uplink_profile.as_str());

    print_wifi(cfg, &adapters);
    print_adapters(cfg, &adapters, &hotspot_adapter, uplink_label);

    println!(
        "\nHotspot adapter ('{}' -> {}):",
        cfg.hotspot.hotspot_adapter,
        hotspot_adapter.describe()
    );
    if cfg.hotspot.is_auto_uplink() {
        println!(
            "Hotspot (uplink 'auto' -> {}):",
            uplink_label.unwrap_or("?")
        );
    } else {
        println!("Hotspot (uplink '{}'):", cfg.hotspot.uplink_adapter);
    }
    print_hotspot(cfg, hotspot);

    Ok(())
}

fn print_wifi(cfg: &Config, adapters: &[Adapter]) {
    println!("Wi-Fi interfaces:");

    let status = match wifi::query() {
        Ok(status) => status,
        Err(e) => return println!("  error: {e:#}"),
    };
    if status.interfaces.is_empty() {
        return println!("  (none found)");
    }

    for iface in &status.interfaces {
        let state = if iface.connected {
            "connected"
        } else {
            "not connected"
        };
        let ssid = iface
            .ssid
            .as_deref()
            .map(|s| format!("to '{s}'"))
            .unwrap_or_default();
        let ip = adapters::ipv4_of(adapters, &iface.description)
            .map(|ip| format!(", {ip}"))
            .unwrap_or_default();

        println!("  {} -- {state} {ssid}{ip}", iface.description);
    }

    let seen_as = if status.is_connected(Some(&cfg.hotspot.ssid)) {
        "CONNECTED"
    } else {
        "DISCONNECTED"
    };
    println!("  => watchdog sees Wi-Fi as: {seen_as}");
}

fn print_adapters(
    cfg: &Config,
    adapters: &[Adapter],
    hotspot_adapter: &HotspotAdapter,
    uplink_label: Option<&str>,
) {
    println!("\nNetwork adapters:");
    if adapters.is_empty() {
        println!("  (none found, or enumeration failed -- see log)");
    }

    for adapter in adapters {
        let ip = if adapter.ipv4.is_empty() {
            String::new()
        } else {
            format!(", {}", adapter.ipv4.join(", "))
        };
        println!(
            "   '{}' ({}{ip}) {}.",
            adapter.friendly_name,
            adapter.description,
            role_of(cfg, adapter, hotspot_adapter, uplink_label)
        );
    }
}

/// What part this adapter plays, if any, in the configuration as it resolves right now.
fn role_of(
    cfg: &Config,
    adapter: &Adapter,
    hotspot_adapter: &HotspotAdapter,
    uplink_label: Option<&str>,
) -> &'static str {
    if hotspot_adapter.excludes(Some(adapter)) {
        return " <== hotspot adapter (excluded from uplink)";
    }
    if cfg.hotspot.is_auto_uplink() {
        if uplink_label == Some(adapter.friendly_name.as_str()) {
            return " <== auto-selected uplink";
        }
        return "";
    }
    if adapter.matches(&cfg.hotspot.uplink_adapter) {
        return " <== configured uplink";
    }
    ""
}

fn print_hotspot(cfg: &Config, hotspot: Result<Hotspot>) {
    let hotspot = match hotspot {
        Ok(hotspot) => hotspot,
        Err(e) => return println!("  error: {e:#}"),
    };

    // Each field is read independently: one unreadable value shouldn't hide the others.
    println!("  uplink profile: {}", hotspot.uplink_profile);
    println!("  state:          {}", or_unknown(hotspot.state()));
    println!("  current SSID:   {}", or_unknown(hotspot.current_ssid()));
    println!("  clients:        {}", or_unknown(hotspot.client_count()));
    println!("  configured SSID: {}", cfg.hotspot.ssid);
}

/// Render a value we may not have been able to read.
fn or_unknown<T: std::fmt::Display>(value: Result<T>) -> String {
    value.map_or_else(|_| "?".to_string(), |value| value.to_string())
}
