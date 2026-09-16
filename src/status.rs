use std::collections::HashMap;

/// The Wi-Fi half of what `compute()` needs, already resolved by the caller.
pub struct WifiSnapshot<'a> {
    pub wifi_ok: bool,
    pub connected: bool,
    pub ssid: Option<&'a str>,
    pub ip_address: Option<&'a str>,
}

/// The hotspot half of what `compute()` needs, already resolved by the caller.
pub struct HotspotSnapshot<'a> {
    pub on: bool,
    pub ssid: &'a str,
    pub password: &'a str,
    pub ip_address: Option<&'a str>,
}

/// A companion status report, keyed by single-letter fields ("s" for state, "i" for the
/// SSID, "a" for the address, "p" for the password). Owns its values (rather than
/// borrowing, as `WifiSnapshot`/`HotspotSnapshot` do) so a status can be held onto across
/// polling ticks and diffed against the next one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    elements: HashMap<&'static str, String>,
}

const KEYS: &[&str] = &["s", "i", "a", "p", "m"];

impl Status {
    fn new_from(elements: HashMap<&'static str, String>) -> Self {
        Status { elements }
    }

    pub fn new(wifi: &WifiSnapshot, hotspot: Option<&HotspotSnapshot>) -> Self {
        let mut elements: HashMap<&'static str, String> = HashMap::new();
        if !wifi.wifi_ok {
            elements.insert("s", "unknown".to_string());
        } else if wifi.connected {
            elements.insert("s", "connected".to_string());
            elements.insert("i", wifi.ssid.unwrap_or("N/A").to_string());
            elements.insert("a", wifi.ip_address.unwrap_or("N/A").to_string());
        } else if let Some(h) = hotspot {
            if h.on {
                elements.insert("s", "hotspot".to_string());
                elements.insert("i", h.ssid.to_string());
                elements.insert("p", h.password.to_string());
                elements.insert("a", h.ip_address.unwrap_or("N/A").to_string());
            }
        } else {
            elements.insert("s", "disconnected".to_string());
        }
        Status { elements }
    }

    /// The entries present in `self` that are missing from, or differ from, `other`.
    pub fn diff(&self, other: &Status) -> Self {
        let mut output: HashMap<&'static str, String> = HashMap::new();
        for k in KEYS {
            let this_value = self.elements.get(k).map(String::as_str);
            let other_value = other.elements.get(k).map(String::as_str);
            if this_value != other_value {
                if let Some(v) = this_value {
                    output.insert(k, v.to_string());
                }
            }
        }
        Status::new_from(output)
    }

    /// Does this status carry a new value for `key`? Used to tell whether a diff
    /// touched the state field ("s") specifically, e.g. to reset a since-last-change
    /// timer only on an actual state transition.
    pub fn contains_key(&self, key: &str) -> bool {
        self.elements.contains_key(key)
    }

    /// A copy of this status with `key` set to `value` -- for attaching a side-channel
    /// field (like time since the last state change) that isn't part of the diffed
    /// state itself, just before sending.
    pub fn with_field(&self, key: &'static str, value: impl Into<String>) -> Status {
        let mut elements = self.elements.clone();
        elements.insert(key, value.into());
        Status::new_from(elements)
    }

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
            connected: true,
            ssid,
            ip_address: ip,
        }
    }

    fn wifi_disconnected() -> WifiSnapshot<'static> {
        WifiSnapshot {
            wifi_ok: true,
            connected: false,
            ssid: None,
            ip_address: None,
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

    fn elements_of(status: &Status) -> Vec<String> {
        let mut elements = status.as_elements();
        elements.sort();
        elements
    }

    #[test]
    fn wifi_query_failure_reports_unknown_regardless_of_everything_else() {
        let mut unknown = wifi_connected(Some("home"), Some("192.168.1.2"));
        unknown.wifi_ok = false;
        let status = Status::new(
            &unknown,
            Some(&hotspot_on("AP", "pw", Some("192.168.137.1"))),
        );
        assert_eq!(elements_of(&status), vec!["s:unknown".to_string()]);
    }

    #[test]
    fn connected_wifi_reports_ssid_and_address() {
        let status = Status::new(&wifi_connected(Some("home"), Some("192.168.1.2")), None);
        assert_eq!(
            elements_of(&status),
            vec![
                "a:192.168.1.2".to_string(),
                "i:home".to_string(),
                "s:connected".to_string()
            ]
        );
    }

    #[test]
    fn hotspot_reports_when_wifi_is_down() {
        let status = Status::new(
            &wifi_disconnected(),
            Some(&hotspot_on("AP", "pw", Some("192.168.137.1"))),
        );
        assert_eq!(
            elements_of(&status),
            vec![
                "a:192.168.137.1".to_string(),
                "i:AP".to_string(),
                "p:pw".to_string(),
                "s:hotspot".to_string(),
            ]
        );
    }

    #[test]
    fn disconnected_when_wifi_and_hotspot_are_both_down() {
        let status = Status::new(&wifi_disconnected(), None);
        assert_eq!(elements_of(&status), vec!["s:disconnected".to_string()]);
    }

    #[test]
    fn diff_reports_only_the_fields_that_changed() {
        let old = Status::new(&wifi_connected(Some("home"), Some("192.168.1.2")), None);
        let new = Status::new(&wifi_connected(Some("home"), Some("192.168.1.3")), None);
        assert_eq!(
            elements_of(&new.diff(&old)),
            vec!["a:192.168.1.3".to_string()]
        );
    }

    #[test]
    fn diff_reports_a_field_that_disappeared_as_a_change_but_not_its_value() {
        // Going from hotspot (which has "p") to connected (which doesn't) drops "p"
        // entirely; diff only reports fields still present in the newer status.
        let old = Status::new(&wifi_disconnected(), Some(&hotspot_on("AP", "pw", None)));
        let new = Status::new(&wifi_connected(Some("home"), Some("192.168.1.2")), None);
        let changed = new.diff(&old);
        assert!(!changed.as_elements().contains(&"p:pw".to_string()));
        assert!(changed.as_elements().contains(&"s:connected".to_string()));
    }

    #[test]
    fn diff_contains_key_reports_whether_the_state_field_changed() {
        let old = Status::new(&wifi_connected(Some("home"), Some("192.168.1.2")), None);
        let new = Status::new(&wifi_connected(Some("home"), Some("192.168.1.3")), None);
        assert!(!new.diff(&old).contains_key("s"));

        let disconnected = Status::new(&wifi_disconnected(), None);
        assert!(disconnected.diff(&old).contains_key("s"));
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
