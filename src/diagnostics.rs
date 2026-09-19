//! One-shot diagnostics: the `autospot status` command, used to verify each piece by
//! hand -- Wi-Fi interfaces, adapters, and the hotspot's resolved uplink and state.

use anyhow::Result;

use crate::adapters;
use crate::config::Config;
use crate::hotspot::{self, Hotspot};
use crate::wifi;

/// One-shot diagnostics, used to verify each piece by hand.
pub fn print_status(cfg: &Config) -> Result<()> {
    let adapters_snapshot = adapters::list().unwrap_or_default();
    let auto = cfg.hotspot.is_auto_uplink();
    // Resolved once up front so the adapter listing below and the hotspot section agree
    // on which adapter auto mode actually picked, and which one is excluded as the
    // hotspot's own adapter.
    let hotspot_adapter =
        hotspot::resolve_hotspot_adapter(&cfg.hotspot.hotspot_adapter, &adapters_snapshot);
    let hotspot_result = Hotspot::for_uplink(&cfg.hotspot);
    let resolved_label = hotspot_result
        .as_ref()
        .ok()
        .map(|h| h.uplink_profile.as_str());

    println!("Wi-Fi interfaces:");
    match wifi::query() {
        Ok(status) if status.interfaces.is_empty() => println!("  (none found)"),
        Ok(status) => {
            for i in &status.interfaces {
                let ip = adapters::ipv4_of(&adapters_snapshot, &i.description)
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
        let marker = if hotspot_adapter.excludes(Some(a)) {
            " <== hotspot adapter (excluded from uplink)"
        } else if auto {
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
        println!("   '{}' ({}{ip}) {marker}.", a.friendly_name, a.description);
    }

    println!(
        "\nHotspot adapter ('{}' -> {}):",
        cfg.hotspot.hotspot_adapter,
        hotspot_adapter.describe()
    );
    if auto {
        println!(
            "Hotspot (uplink 'auto' -> {}):",
            resolved_label.unwrap_or("?")
        );
    } else {
        println!("Hotspot (uplink '{}'):", cfg.hotspot.uplink_adapter);
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
