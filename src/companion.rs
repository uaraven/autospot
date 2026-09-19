use std::time::Instant;

use serialport::*;
use tracing::{debug, error, trace, warn};

use crate::{config::CompanionConfig, status::Status};

const VENDOR: &str = "autoport";
const PRODUCT: &str = "companion";
const DEFAULT_VID: u16 = 0x1209;
const DEFAULT_PID: u16 = 0x3a01;

pub struct CompanionConn {
    port: SerialPortInfo,
}

impl CompanionConn {
    /// Look for the companion microcontroller among the currently attached serial
    /// devices. `None` covers both "not plugged in" and a failure to enumerate ports at
    /// all -- neither is worth crashing the watchdog loop over.
    pub fn new(config: CompanionConfig) -> Option<Self> {
        let ports = match serialport::available_ports() {
            Ok(ports) => ports,
            Err(e) => {
                debug!("could not enumerate serial ports: {e}");
                return None;
            }
        };
        for port in ports {
            trace!("checking port {:?}", port);
            if let SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid,
                serial_number: _,
                manufacturer,
                product,
            }) = &port.port_type
            {
                let is_companion = if let Some(c_pid) = config.pid
                    && let Some(c_vid) = config.vid
                    && *vid == c_vid
                    && *pid == c_pid
                {
                    true
                } else if *vid == DEFAULT_VID && *pid == DEFAULT_PID {
                    true
                } else if manufacturer.as_deref() == Some(VENDOR)
                    && product.as_deref() == Some(PRODUCT)
                {
                    true
                } else {
                    false
                };
                if is_companion {
                    debug!("companion found on port {:?}", port);
                    return Some(CompanionConn { port });
                }
            }
        }
        None
    }

    pub fn write_status(&self, status: &Status) -> anyhow::Result<()> {
        let elements = status.as_elements();
        debug!(
            port = %self.port.port_name,
            "sending to companion: {}",
            redact_for_log(&elements).join(", ")
        );

        let mut port = serialport::new(self.port.port_name.as_str(), 9600).open()?;
        for line in &elements {
            port.write_all(line.as_bytes())?;
            port.write("\n".as_bytes())?;
            port.flush()?;
        }

        Ok(())
    }
}

/// A companion connection's outgoing state: what was last sent to it, and since when the
/// "s" field has held its current value. Owning this alongside the connection (rather
/// than in the caller) keeps "what does the companion currently know" in one place.
pub struct CompanionSession {
    cfg: CompanionConfig,
    /// What was last sent to the companion, so only the changed fields need re-sending.
    last_status: Status,
    /// When the "s" field last actually changed, so outgoing updates can tell the
    /// companion how long the current state has held.
    last_state_change: Instant,
    log_missing_companion: bool,
}

impl CompanionSession {
    pub fn new(cfg: CompanionConfig) -> Self {
        Self {
            cfg,
            last_status: Status::default(),
            last_state_change: Instant::now(),
            log_missing_companion: true,
        }
    }

    /// Diff `status` against what was last sent and forward only the changed fields to
    /// the companion microcontroller, tagged with "t": seconds since the "s" field
    /// itself last changed -- e.g. how long Wi-Fi has been disconnected. Resolves the
    /// serial port fresh on every call (see `CompanionConn::new`) so a companion that is
    /// unplugged and replugged -- possibly under a different COM port -- is still found.
    /// Updates `self.last_status`/`self.last_state_change` on every call, even when
    /// there is no companion to send to, so both stay accurate for whenever one shows up.
    pub fn report(&mut self, now: Instant, status: Status) {
        let changed = status.diff(&self.last_status);
        self.last_status = status;

        if changed.contains_key("s") {
            self.last_state_change = now;
        }
        let since_state_change = now.duration_since(self.last_state_change).as_secs();
        let to_send = self
            .last_status
            .with_field("t", since_state_change.to_string());

        let Some(companion) = CompanionConn::new(self.cfg) else {
            if self.log_missing_companion {
                warn!("no companion device found");
                self.log_missing_companion = false;
            }
            return;
        };
        self.log_missing_companion = true;
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
