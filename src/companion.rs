//! The optional companion microcontroller: a little serial display that shows the same
//! status the console status line does. Everything here is best effort -- a companion
//! that isn't plugged in, or a write that fails, must never disturb the watchdog loop.

use std::time::Instant;

use anyhow::Result;
use serialport::{SerialPort, SerialPortInfo, SerialPortType, UsbPortInfo};
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

    fn open(&self) -> Result<Box<dyn SerialPort>> {
        // CircuitPython only sends console output (the companion's button presses) while
        // the host asserts DTR, and serialport leaves it low on Windows by default.
        Ok(serialport::new(self.port.port_name.as_str(), BAUD_RATE)
            .dtr_on_open(true)
            .open()?)
    }
}

/// Send every field of `status`, one `key:value` line at a time.
fn write_status(port: &mut dyn SerialPort, status: &Status) -> Result<()> {
    let elements = status.as_elements();
    debug!(
        port = port.name().unwrap_or_default(),
        "sending to companion: {}",
        redact_for_log(&elements).join(", ")
    );

    for element in &elements {
        writeln!(port, "{element}")?;
        port.flush()?;
    }

    Ok(())
}

/// Whatever is waiting in the port's receive buffer, without blocking.
fn read_pending(port: &mut dyn SerialPort) -> Result<Vec<u8>> {
    let pending = port.bytes_to_read()? as usize;
    if pending == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; pending];
    let got = port.read(&mut buf)?;
    buf.truncate(got);
    debug!(
        port = port.name().unwrap_or_default(),
        "received {got} bytes from companion: {:?}",
        String::from_utf8_lossy(&buf)
    );
    Ok(buf)
}

/// A request the companion sent us (a button press).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// "S": switch between Wi-Fi and hotspot.
    Toggle,
}

/// Longest partial line kept while waiting for its newline; anything beyond that is
/// garbage, not a command.
const MAX_PENDING_LINE: usize = 256;

fn parse_command(line: &[u8]) -> Option<Command> {
    match strip_escape_sequences(line).trim_ascii() {
        b"S" => Some(Command::Toggle),
        _ => None,
    }
}

/// Remove terminal escape sequences. CircuitPython's console wraps its output in them
/// -- notably a window-title sequence (`ESC ] 0 ; ... ESC \`) sent just before a
/// program's output -- so a bare `S` arrives as `<title sequence>S`.
fn strip_escape_sequences(line: &[u8]) -> Vec<u8> {
    const ESC: u8 = 0x1b;
    const BEL: u8 = 0x07;

    let mut out = Vec::with_capacity(line.len());
    let mut bytes = line.iter().copied();
    while let Some(b) = bytes.next() {
        if b != ESC {
            out.push(b);
            continue;
        }
        match bytes.next() {
            // OSC: runs to BEL or the two-byte string terminator ESC \.
            Some(b']') => {
                let mut prev = 0;
                for c in bytes.by_ref() {
                    if c == BEL || (prev == ESC && c == b'\\') {
                        break;
                    }
                    prev = c;
                }
            }
            // CSI: runs to a final byte in 0x40..=0x7e.
            Some(b'[') => {
                for c in bytes.by_ref() {
                    if (0x40..=0x7e).contains(&c) {
                        break;
                    }
                }
            }
            // Any other two-byte escape.
            _ => {}
        }
    }
    out
}

/// Append `chunk` to the partial-line buffer and consume every complete line in it.
/// However many toggle requests arrived, they collapse into one, so a double press
/// doesn't toggle twice.
fn extract_command(pending: &mut Vec<u8>, chunk: &[u8]) -> Option<Command> {
    pending.extend_from_slice(chunk);

    let mut command = None;
    while let Some(end) = pending.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = pending.drain(..=end).collect();
        match parse_command(&line) {
            Some(cmd) => command = Some(cmd),
            None => debug!("ignoring unknown companion input: {:?}", line.trim_ascii()),
        }
    }

    if pending.len() > MAX_PENDING_LINE {
        pending.clear();
    }
    command
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
    /// The open port, kept between calls so a button press sent while we are not looking
    /// still lands in the OS receive buffer. Dropped on any I/O error so the next call
    /// looks the companion up afresh.
    port: Option<Box<dyn SerialPort>>,
    /// Received bytes that do not yet form a complete line.
    pending_input: Vec<u8>,
}

impl CompanionSession {
    pub fn new(cfg: CompanionConfig) -> Self {
        Self {
            cfg,
            last_status: Status::default(),
            last_state_change: Instant::now(),
            warn_if_missing: true,
            port: None,
            pending_input: Vec::new(),
        }
    }

    /// The open companion port, finding and opening it first if needed. `None` when
    /// there is no companion (or it cannot be opened); warns once per outage.
    fn ensure_open(&mut self) -> Option<&mut Box<dyn SerialPort>> {
        if self.port.is_none() {
            let Some(companion) = CompanionConn::find(self.cfg) else {
                if self.warn_if_missing {
                    warn!("no companion device found");
                    self.warn_if_missing = false;
                }
                return None;
            };

            match companion.open() {
                Ok(port) => {
                    self.warn_if_missing = true;
                    self.pending_input.clear();
                    self.port = Some(port);
                }
                Err(e) => {
                    debug!("could not open the companion port: {e:#}");
                    if self.warn_if_missing {
                        error!("could not open the companion port: {e:#}");
                        self.warn_if_missing = false;
                    }
                    return None;
                }
            }
        }
        self.port.as_mut()
    }

    /// Check the companion for a request. Never blocks: it only looks at what has
    /// already arrived.
    pub fn poll_command(&mut self) -> Option<Command> {
        let port = self.ensure_open()?;
        match read_pending(port.as_mut()) {
            Ok(chunk) => extract_command(&mut self.pending_input, &chunk),
            Err(e) => {
                warn!("could not read from companion: {e:#}");
                self.port = None;
                None
            }
        }
    }

    /// Send `status` to the companion, tagged with "t": seconds since the state itself
    /// last changed -- e.g. how long Wi-Fi has been disconnected. The port is looked up
    /// again (see [`CompanionConn::find`]) after any I/O error so a companion that is
    /// unplugged and replugged -- possibly under a different COM port -- is still found. The state
    /// is recorded even when there is no companion to send it to, so "t" stays accurate
    /// for whenever one shows up.
    pub fn report(&mut self, now: Instant, status: Status) {
        if status.state() != self.last_status.state() {
            self.last_state_change = now;
        }
        self.last_status = status;

        let held_for = now.duration_since(self.last_state_change).as_secs();
        let to_send = self.last_status.with_field("t", held_for.to_string());

        let Some(port) = self.ensure_open() else {
            return;
        };

        if let Err(e) = write_status(port.as_mut(), &to_send) {
            error!("could not send status to companion: {e:#}");
            self.port = None;
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

    fn feed(pending: &mut Vec<u8>, chunk: &str) -> Option<Command> {
        extract_command(pending, chunk.as_bytes())
    }

    #[test]
    fn s_newline_is_a_toggle() {
        let mut p = Vec::new();
        assert_eq!(feed(&mut p, "S\n"), Some(Command::Toggle));
        assert!(p.is_empty());
    }

    #[test]
    fn tolerates_carriage_returns() {
        assert_eq!(feed(&mut Vec::new(), "S\r\n"), Some(Command::Toggle));
    }

    #[test]
    fn ignores_circuitpython_title_escape_sequences() {
        // What the board really sent: a window-title sequence, then the button's "S".
        let chunk = b"\x1b]0;\xf0\x9f\x90\x8dWi-Fi: off | BLE:Off | code.py | 10.3.1\x1b\\S\r\n";
        assert_eq!(
            extract_command(&mut Vec::new(), chunk),
            Some(Command::Toggle)
        );
        // Colour codes and BEL-terminated titles too.
        assert_eq!(
            extract_command(&mut Vec::new(), b"\x1b[0m\x1b]0;t\x07S\n"),
            Some(Command::Toggle)
        );
        // A title on its own is not a command.
        assert_eq!(extract_command(&mut Vec::new(), b"\x1b]0;x\x1b\\\n"), None);
    }

    #[test]
    fn waits_for_the_newline_across_reads() {
        let mut p = Vec::new();
        assert_eq!(feed(&mut p, "S"), None);
        assert_eq!(feed(&mut p, "\n"), Some(Command::Toggle));
    }

    #[test]
    fn repeated_presses_collapse_into_one_toggle() {
        let mut p = Vec::new();
        assert_eq!(feed(&mut p, "S\nS\n"), Some(Command::Toggle));
        assert_eq!(feed(&mut p, "S\n"), Some(Command::Toggle));
    }

    #[test]
    fn ignores_other_input() {
        let mut p = Vec::new();
        assert_eq!(feed(&mut p, "hello\nSS\ns\n"), None);
        assert_eq!(feed(&mut p, "junk\nS\n"), Some(Command::Toggle));
    }

    #[test]
    fn drops_an_endless_line() {
        let mut p = Vec::new();
        assert_eq!(feed(&mut p, &"x".repeat(MAX_PENDING_LINE + 1)), None);
        assert!(p.is_empty());
    }

    #[test]
    fn redacts_only_the_password_field() {
        let elements = ["s:hotspot".to_string(), "p:secret".to_string()];
        assert_eq!(redact_for_log(&elements), ["s:hotspot", "p:<redacted>"]);
    }
}
