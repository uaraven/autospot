use serialport::*;
use tracing::debug;

use crate::{config::CompanionConfig, status::Status};

const VENDOR: &str = "autoport";
const PRODUCT: &str = "companion";

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
            debug!("checking port {:?}", port);
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
