//! The status report sent to the companion microcontroller: a handful of single-letter
//! fields, derived from what the watchdog saw this tick.
//!
//! Pure data assembly -- no Windows calls, no serial I/O -- so the rules for what gets
//! reported can be tested on their own. `companion` does the sending.

use std::collections::BTreeMap;

/// Field key for the overall state, the one field every report carries.
const STATE: &str = "s";

/// The Wi-Fi half of what [`Status::new`] needs, already resolved by the caller.
pub struct WifiSnapshot<'a> {
    pub wifi_ok: bool,
    /// False when the adapter's radio itself is off (the Wi-Fi toggle), as opposed to
    /// merely being disconnected from a network.
    pub radio_enabled: bool,
    pub connected: bool,
    pub ssid: Option<&'a str>,
    pub ip_address: Option<&'a str>,
}

/// The hotspot half of what [`Status::new`] needs, already resolved by the caller.
pub struct HotspotSnapshot<'a> {
    pub on: bool,
    pub ssid: &'a str,
    pub password: &'a str,
    pub ip_address: Option<&'a str>,
}

/// A companion status report, keyed by single-letter fields ("s" for state, "i" for the
/// SSID, "a" for the address, "p" for the password). Owns its values (rather than
/// borrowing, as `WifiSnapshot`/`HotspotSnapshot` do) so a status can be held onto across
/// polling ticks and compared against the next one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    elements: BTreeMap<&'static str, String>,
}

impl Status {
    /// The report for one tick. Wi-Fi comes first: the hotspot is only reported when
    /// there is no Wi-Fi connection to report instead, since that is the whole point of
    /// it being on.
    pub fn new(wifi: &WifiSnapshot, hotspot: Option<&HotspotSnapshot>) -> Self {
        let mut elements = BTreeMap::new();
        let mut set = |key: &'static str, value: &str| {
            elements.insert(key, value.to_string());
        };

        if !wifi.wifi_ok {
            set(STATE, "unknown");
        } else if !wifi.radio_enabled {
            set(STATE, "wifi-off");
        } else if wifi.connected {
            set(STATE, "connected");
            set("i", wifi.ssid.unwrap_or("N/A"));
            set("a", wifi.ip_address.unwrap_or("N/A"));
        } else if let Some(h) = hotspot.filter(|h| h.on) {
            set(STATE, "hotspot");
            set("i", h.ssid);
            set("p", h.password);
            set("a", h.ip_address.unwrap_or("N/A"));
        } else {
            set(STATE, "disconnected");
        }

        Status { elements }
    }

    /// The overall state field, e.g. `"connected"`. `None` only for a default (never
    /// reported) status, which is what the first tick is compared against.
    pub fn state(&self) -> Option<&str> {
        self.elements.get(STATE).map(String::as_str)
    }

    /// A copy of this status with `key` set to `value` -- for attaching a side-channel
    /// field (like time since the last state change) that isn't part of the state itself,
    /// just before sending.
    pub fn with_field(&self, key: &'static str, value: impl Into<String>) -> Status {
        let mut elements = self.elements.clone();
        elements.insert(key, value.into());
        Status { elements }
    }

    /// The report as `key:value` lines, in a stable order.
    pub fn as_elements(&self) -> Vec<String> {
        self.elements
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi_connected<'a>(ssid: Option<&'a str>, ip: Option<&'a str>) -> WifiSnapshot<'a> {
        WifiSnapshot {
            wifi_ok: true,
            radio_enabled: true,
            connected: true,
            ssid,
            ip_address: ip,
        }
    }

    fn wifi_disconnected() -> WifiSnapshot<'static> {
        WifiSnapshot {
            wifi_ok: true,
            radio_enabled: true,
            connected: false,
            ssid: None,
            ip_address: None,
        }
    }

    fn wifi_radio_off() -> WifiSnapshot<'static> {
        WifiSnapshot {
            radio_enabled: false,
            ..wifi_disconnected()
        }
    }

    fn hotspot_on<'a>(
        ssid: &'a str,
        password: &'a str,
        ip: Option<&'a str>,
    ) -> HotspotSnapshot<'a> {
        HotspotSnapshot {
            on: true,
            ssid,
            password,
            ip_address: ip,
        }
    }

    #[test]
    fn wifi_query_failure_reports_unknown_regardless_of_everything_else() {
        let unknown = WifiSnapshot {
            wifi_ok: false,
            ..wifi_connected(Some("home"), Some("192.168.1.2"))
        };
        let status = Status::new(
            &unknown,
            Some(&hotspot_on("AP", "pw", Some("192.168.137.1"))),
        );
        assert_eq!(status.as_elements(), ["s:unknown"]);
    }

    #[test]
    fn connected_wifi_reports_ssid_and_address() {
        let status = Status::new(&wifi_connected(Some("home"), Some("192.168.1.2")), None);
        assert_eq!(
            status.as_elements(),
            ["a:192.168.1.2", "i:home", "s:connected"]
        );
    }

    #[test]
    fn hotspot_reports_when_wifi_is_down() {
        let status = Status::new(
            &wifi_disconnected(),
            Some(&hotspot_on("AP", "pw", Some("192.168.137.1"))),
        );
        assert_eq!(
            status.as_elements(),
            ["a:192.168.137.1", "i:AP", "p:pw", "s:hotspot"]
        );
    }

    #[test]
    fn disconnected_when_wifi_and_hotspot_are_both_down() {
        let status = Status::new(&wifi_disconnected(), None);
        assert_eq!(status.as_elements(), ["s:disconnected"]);
    }

    #[test]
    fn disconnected_when_the_hotspot_is_present_but_off() {
        let off = HotspotSnapshot {
            on: false,
            ..hotspot_on("AP", "pw", None)
        };
        let status = Status::new(&wifi_disconnected(), Some(&off));
        assert_eq!(status.as_elements(), ["s:disconnected"]);
    }

    #[test]
    fn wifi_off_reports_even_with_a_hotspot_argument() {
        let status = Status::new(
            &wifi_radio_off(),
            Some(&hotspot_on("AP", "pw", Some("192.168.137.1"))),
        );
        assert_eq!(status.as_elements(), ["s:wifi-off"]);
    }

    #[test]
    fn state_names_the_overall_state_and_is_empty_until_one_is_reported() {
        assert_eq!(Status::default().state(), None);
        assert_eq!(
            Status::new(&wifi_disconnected(), None).state(),
            Some("disconnected")
        );
    }

    #[test]
    fn with_field_attaches_a_side_channel_value_without_touching_the_rest() {
        let status = Status::new(&wifi_connected(Some("home"), Some("192.168.1.2")), None);
        let tagged = status.with_field("t", "42");
        assert!(tagged.as_elements().contains(&"t:42".to_string()));
        // The original is untouched -- with_field returns a copy.
        assert!(!status.as_elements().contains(&"t:42".to_string()));
        assert_eq!(tagged.as_elements().len(), status.as_elements().len() + 1);
    }
}
