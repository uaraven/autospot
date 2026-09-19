//! The optional companion microcontroller: a little serial display that shows the same
//! status the console status line does. Everything here is best effort -- a companion
//! that isn't plugged in, or a write that fails, must never disturb the watchdog loop.

use std::io::Write as _;
use std::time::Instant;

use anyhow::Result;
use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};
use tracing::{debug, error, trace, warn};

use crate::config::CompanionConfig;
use crate::status::Status;

/// USB identity the companion firmware ships with, recognised without any config.
const DEFAULT_VID: u16 = 0x1209;
const DEFAULT_PID: u16 = 0x3a01;
const VENDOR: &str = "autoport";
const PRODUCT: &str = "companion";

/// The companion listens at this rate; see `companion/code.py`.
const BAUD_RATE: u32 = 9600;

/// A companion found on a serial port, ready to be written to.
struct CompanionConn {
    port: SerialPortInfo,
}

impl CompanionConn {
    /// Look for the companion microcontroller among the currently attached serial
    /// devices. `None` covers both "not plugged in" and a failure to enumerate ports at
    /// all -- neither is worth crashing the watchdog loop over.
    fn find(cfg: CompanionConfig) -> Option<Self> {
        let ports = serialport::available_ports()
            .inspect_err(|e| debug!("could not enumerate serial ports: {e}"))
            .ok()?;

        let port = ports
            .into_iter()
            .inspect(|port| trace!("checking port {port:?}"))
            .find(|port| match &port.port_type {
                SerialPortType::UsbPort(usb) => is_companion(usb, cfg),
                _ => false,
            })?;

        debug!("companion found on port {port:?}");
        Some(Self { port })
    }

    /// Send every field of `status`, one `key:value` line at a time.
    fn write_status(&self, status: &Status) -> Result<()> {
        let elements = status.as_elements();
        debug!(
            port = %self.port.port_name,
            "sending to companion: {}",
            redact_for_log(&elements).join(", ")
        );

        let mut port = serialport::new(self.port.port_name.as_str(), BAUD_RATE).open()?;
        for element in &elements {
            writeln!(port, "{element}")?;
            port.flush()?;
        }

        Ok(())
    }
}

/// Is this USB serial device our companion board? Either it carries the VID/PID pair
/// configured in `autospot.toml`, or the pair the firmware ships with, or it introduces
/// itself by name.
fn is_companion(usb: &UsbPortInfo, cfg: CompanionConfig) -> bool {
    let configured_ids = (cfg.vid, cfg.pid) == (Some(usb.vid), Some(usb.pid));
    let default_ids = (usb.vid, usb.pid) == (DEFAULT_VID, DEFAULT_PID);
    let usb_strings =
        usb.manufacturer.as_deref() == Some(VENDOR) && usb.product.as_deref() == Some(PRODUCT);

    configured_ids || default_ids || usb_strings
}

/// What the companion has been told, and when its state last changed. Owning this
/// alongside the config (rather than in the caller) keeps "what does the companion
/// currently know" in one place.
pub struct CompanionSession {
    cfg: CompanionConfig,
    last_status: Status,
    /// When the state field last actually changed, so outgoing updates can tell the
    /// companion how long the current state has held.
    last_state_change: Instant,
    /// Cleared after warning once, so a companion that is simply not plugged in doesn't
    /// fill the log; re-armed once one is found again.
    warn_if_missing: bool,
}

impl CompanionSession {
    pub fn new(cfg: CompanionConfig) -> Self {
        Self {
            cfg,
            last_status: Status::default(),
            last_state_change: Instant::now(),
            warn_if_missing: true,
        }
    }

    /// Send `status` to the companion, tagged with "t": seconds since the state itself
    /// last changed -- e.g. how long Wi-Fi has been disconnected. Resolves the serial port
    /// fresh on every call (see [`CompanionConn::find`]) so a companion that is unplugged
    /// and replugged -- possibly under a different COM port -- is still found. The state
    /// is recorded even when there is no companion to send it to, so "t" stays accurate
    /// for whenever one shows up.
    pub fn report(&mut self, now: Instant, status: Status) {
        if status.state() != self.last_status.state() {
            self.last_state_change = now;
        }
        self.last_status = status;

        let held_for = now.duration_since(self.last_state_change).as_secs();
        let to_send = self.last_status.with_field("t", held_for.to_string());

        let Some(companion) = CompanionConn::find(self.cfg) else {
            if self.warn_if_missing {
                warn!("no companion device found");
                self.warn_if_missing = false;
            }
            return;
        };

        self.warn_if_missing = true;
        if let Err(e) = companion.write_status(&to_send) {
            error!("could not send status to companion: {e:#}");
        }
    }
}

/// Mask the password field before logging -- everything else here is either public
/// (SSID) or transient (IP address), but the Wi-Fi/hotspot passphrase should never end
/// up in a log file.
fn redact_for_log(elements: &[String]) -> Vec<&str> {
    elements
        .iter()
        .map(|e| {
            if e.starts_with("p:") {
                "p:<redacted>"
            } else {
                e.as_str()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(vid: u16, pid: u16) -> UsbPortInfo {
        UsbPortInfo {
            vid,
            pid,
            serial_number: None,
            manufacturer: None,
            product: None,
        }
    }

    #[test]
    fn recognises_the_firmwares_own_usb_ids_without_any_config() {
        let cfg = CompanionConfig::default();
        assert!(is_companion(&usb(DEFAULT_VID, DEFAULT_PID), cfg));
        assert!(!is_companion(&usb(0x1234, 0x5678), cfg));
    }

    #[test]
    fn recognises_configured_usb_ids() {
        let cfg = CompanionConfig {
            enabled: true,
            vid: Some(0x1234),
            pid: Some(0x5678),
        };
        assert!(is_companion(&usb(0x1234, 0x5678), cfg));
        // One half of the pair matching is not enough.
        assert!(!is_companion(&usb(0x1234, 0x9999), cfg));
    }

    #[test]
    fn recognises_the_companion_by_its_usb_strings() {
        let named = UsbPortInfo {
            manufacturer: Some(VENDOR.to_string()),
            product: Some(PRODUCT.to_string()),
            ..usb(0x1234, 0x5678)
        };
        assert!(is_companion(&named, CompanionConfig::default()));
    }

    #[test]
    fn redacts_only_the_password_field() {
        let elements = ["s:hotspot".to_string(), "p:secret".to_string()];
        assert_eq!(redact_for_log(&elements), ["s:hotspot", "p:<redacted>"]);
    }
}
