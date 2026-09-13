//! Configuration loading: the `autospot.toml` file described in `docs/implementation.md`.
//!
//! This module deliberately stays free of Windows API types so the parsing tests
//! can be run on any host.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// Name of the config file looked up next to the executable when no path is given.
pub const DEFAULT_FILE_NAME: &str = "autospot.toml";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub monitor: MonitorConfig,
    pub hotspot: HotspotConfig,
    #[serde(default, rename = "status-dump")]
    pub status_dump: StatusDumpConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MonitorConfig {
    /// How often to sample Wi-Fi state.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// How long Wi-Fi must stay down before the hotspot is switched on.
    #[serde(default = "default_disconnect_threshold_secs")]
    pub disconnect_threshold_secs: u64,
    /// Switch the hotspot back off once Wi-Fi returns.
    #[serde(default = "default_true")]
    pub auto_disable_on_reconnect: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HotspotConfig {
    pub ssid: String,
    pub passphrase: String,
    #[serde(default)]
    pub band: Band,
    /// Adapter whose internet connection is shared. Matched against the adapter's
    /// friendly name ("Ethernet"), its hardware description, or the network profile
    /// name, case-insensitively. The literal value `"auto"` instead picks, on every
    /// lookup, whichever currently-connected network ranks best (internet access beats
    /// local-only), skipping the hotspot's own virtual adapter.
    pub uplink_adapter: String,
}

/// The `uplink_adapter` value that means "let Windows/Autospot pick the best connected
/// network instead of a specific configured adapter".
pub const AUTO_UPLINK: &str = "auto";

impl HotspotConfig {
    /// Is `uplink_adapter` set to the auto-selection sentinel?
    pub fn is_auto_uplink(&self) -> bool {
        self.uplink_adapter.trim().eq_ignore_ascii_case(AUTO_UPLINK)
    }
}

/// Where to write a live `status.json` for something else to read -- a microcontroller
/// polling from an attached drive being the motivating case.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatusDumpConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Volume label of the disk to write to, e.g. "CIRCUITPY". Resolved fresh (by
    /// enumerating drives) every time a dump is written, so a drive that is unplugged
    /// and replugged under a different letter is still found. Mutually exclusive with
    /// `disk_path`.
    #[serde(default)]
    pub disk_label: Option<String>,
    /// A fixed root path to write to instead of searching by label, e.g. `"E:\\"`.
    /// Mutually exclusive with `disk_label`.
    #[serde(default)]
    pub disk_path: Option<String>,
    /// File name written at the root of the resolved disk. Must be a plain file name,
    /// not a path.
    #[serde(default = "default_status_dump_file_name")]
    pub file_name: String,
}

impl Default for StatusDumpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            disk_label: None,
            disk_path: None,
            file_name: default_status_dump_file_name(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Log file path. A relative path is resolved against the executable's directory
    /// so the task scheduler's working directory does not matter.
    #[serde(default = "default_log_path")]
    pub path: PathBuf,
}

/// Wi-Fi band for the access point. Names match the WinRT `TetheringWiFiBand` enum.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
pub enum Band {
    #[default]
    Auto,
    #[serde(alias = "2.4GHz", alias = "2.4")]
    TwoPointFourGigahertz,
    #[serde(alias = "5GHz", alias = "5")]
    FiveGigahertz,
    #[serde(alias = "6GHz", alias = "6")]
    SixGigahertz,
}

fn default_poll_interval_secs() -> u64 {
    5
}
fn default_disconnect_threshold_secs() -> u64 {
    120
}
fn default_true() -> bool {
    true
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_log_path() -> PathBuf {
    PathBuf::from("autospot.log")
}
fn default_status_dump_file_name() -> String {
    "status.json".to_string()
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval_secs(),
            disconnect_threshold_secs: default_disconnect_threshold_secs(),
            auto_disable_on_reconnect: default_true(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            path: default_log_path(),
        }
    }
}

impl Config {
    /// Parse and validate a config from a TOML string.
    pub fn from_toml(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).context("config is not valid TOML")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load and validate the config at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        Self::from_toml(&text).with_context(|| format!("in config file {}", path.display()))
    }

    /// Reject values Windows (or the state machine) would choke on later.
    fn validate(&self) -> Result<()> {
        if self.monitor.poll_interval_secs == 0 {
            bail!("monitor.poll_interval_secs must be at least 1");
        }
        if self.monitor.disconnect_threshold_secs == 0 {
            bail!("monitor.disconnect_threshold_secs must be at least 1");
        }

        let ssid_len = self.hotspot.ssid.as_bytes().len();
        if ssid_len == 0 {
            bail!("hotspot.ssid must not be empty");
        }
        if ssid_len > 32 {
            bail!("hotspot.ssid must be at most 32 bytes, got {ssid_len}");
        }

        // WPA2-PSK constraint enforced by Windows; catching it here gives a far better
        // message than the E_INVALIDARG that ConfigureAccessPointAsync would return.
        let pass_len = self.hotspot.passphrase.chars().count();
        if !(8..=63).contains(&pass_len) {
            bail!("hotspot.passphrase must be 8-63 characters, got {pass_len}");
        }

        if self.hotspot.uplink_adapter.trim().is_empty() {
            bail!("hotspot.uplink_adapter must not be empty");
        }

        if parse_level(&self.logging.level).is_none() {
            bail!(
                "logging.level must be one of error|warn|info|debug|trace, got '{}'",
                self.logging.level
            );
        }

        if self.status_dump.enabled {
            let label = non_empty(self.status_dump.disk_label.as_deref());
            let path = non_empty(self.status_dump.disk_path.as_deref());
            match (label, path) {
                (Some(_), Some(_)) => {
                    bail!("status-dump: specify only one of disk_label or disk_path, not both")
                }
                (None, None) => {
                    bail!("status-dump: enabled = true requires disk_label or disk_path")
                }
                _ => {}
            }

            let file_name = self.status_dump.file_name.trim();
            if file_name.is_empty() {
                bail!("status-dump.file_name must not be empty");
            }
            if file_name.contains(['/', '\\']) || file_name == "." || file_name == ".." {
                bail!(
                    "status-dump.file_name must be a plain file name, not a path: '{file_name}'"
                );
            }
        }

        Ok(())
    }

    /// Poll interval as a `Duration`.
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.monitor.poll_interval_secs)
    }

    /// Disconnect threshold as a `Duration`.
    pub fn disconnect_threshold(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.monitor.disconnect_threshold_secs)
    }
}

/// Trim and treat an empty string the same as absent.
fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Map a config level string onto a `tracing` level. Returns `None` if unrecognised.
pub fn parse_level(level: &str) -> Option<tracing::Level> {
    match level.trim().to_ascii_lowercase().as_str() {
        "error" => Some(tracing::Level::ERROR),
        "warn" | "warning" => Some(tracing::Level::WARN),
        "info" => Some(tracing::Level::INFO),
        "debug" => Some(tracing::Level::DEBUG),
        "trace" => Some(tracing::Level::TRACE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[monitor]
poll_interval_secs = 5
disconnect_threshold_secs = 120
auto_disable_on_reconnect = true

[hotspot]
ssid = "MyFallbackHotspot"
passphrase = "changeme123"
band = "Auto"
uplink_adapter = "Ethernet"

[logging]
level = "info"
path = "autospot.log"
"#;

    #[test]
    fn parses_the_documented_example() {
        let cfg = Config::from_toml(FULL).unwrap();
        assert_eq!(cfg.monitor.poll_interval_secs, 5);
        assert_eq!(cfg.monitor.disconnect_threshold_secs, 120);
        assert!(cfg.monitor.auto_disable_on_reconnect);
        assert_eq!(cfg.hotspot.ssid, "MyFallbackHotspot");
        assert_eq!(cfg.hotspot.passphrase, "changeme123");
        assert_eq!(cfg.hotspot.band, Band::Auto);
        assert_eq!(cfg.hotspot.uplink_adapter, "Ethernet");
        assert_eq!(cfg.logging.level, "info");
        assert_eq!(cfg.logging.path, PathBuf::from("autospot.log"));
    }

    #[test]
    fn only_hotspot_section_is_required() {
        let cfg = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"
"#,
        )
        .unwrap();
        assert_eq!(cfg.monitor, MonitorConfig::default());
        assert_eq!(cfg.logging, LoggingConfig::default());
        assert_eq!(cfg.monitor.poll_interval_secs, 5);
        assert_eq!(cfg.monitor.disconnect_threshold_secs, 120);
        assert_eq!(cfg.hotspot.band, Band::Auto);
    }

    #[test]
    fn band_accepts_winrt_names_and_friendly_aliases() {
        for (text, expected) in [
            ("Auto", Band::Auto),
            ("TwoPointFourGigahertz", Band::TwoPointFourGigahertz),
            ("2.4GHz", Band::TwoPointFourGigahertz),
            ("FiveGigahertz", Band::FiveGigahertz),
            ("5GHz", Band::FiveGigahertz),
            ("SixGigahertz", Band::SixGigahertz),
        ] {
            let cfg = Config::from_toml(&format!(
                r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
band = "{text}"
uplink_adapter = "Ethernet"
"#
            ))
            .unwrap();
            assert_eq!(cfg.hotspot.band, expected, "band = {text}");
        }
    }

    #[test]
    fn rejects_unknown_keys_so_typos_are_not_silently_defaulted() {
        let err = Config::from_toml(
            r#"
[monitor]
pool_interval_secs = 5

[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"
"#,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("pool_interval_secs"),
            "error should name the offending key: {err:#}"
        );
    }

    #[test]
    fn rejects_missing_hotspot_section() {
        assert!(Config::from_toml("[monitor]\npoll_interval_secs = 5\n").is_err());
    }

    #[test]
    fn rejects_short_passphrase() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "short"
uplink_adapter = "Ethernet"
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("8-63"), "{err:#}");
    }

    #[test]
    fn rejects_oversized_ssid() {
        let err = Config::from_toml(&format!(
            r#"
[hotspot]
ssid = "{}"
passphrase = "password1"
uplink_adapter = "Ethernet"
"#,
            "x".repeat(33)
        ))
        .unwrap_err();
        assert!(format!("{err:#}").contains("32 bytes"), "{err:#}");
    }

    #[test]
    fn rejects_zero_poll_interval() {
        let err = Config::from_toml(
            r#"
[monitor]
poll_interval_secs = 0

[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("poll_interval_secs"), "{err:#}");
    }

    #[test]
    fn rejects_unknown_log_level() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[logging]
level = "verbose"
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("logging.level"), "{err:#}");
    }

    #[test]
    fn recognises_the_auto_uplink_sentinel_case_and_space_insensitively() {
        for value in ["auto", "Auto", "AUTO", "  auto  "] {
            let cfg = Config::from_toml(&format!(
                r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "{value}"
"#
            ))
            .unwrap();
            assert!(cfg.hotspot.is_auto_uplink(), "uplink_adapter = {value:?}");
        }

        let cfg = Config::from_toml(FULL).unwrap();
        assert!(!cfg.hotspot.is_auto_uplink());
    }

    #[test]
    fn rejects_empty_uplink_adapter() {
        assert!(Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "   "
"#,
        )
        .is_err());
    }

    #[test]
    fn level_strings_map_to_tracing_levels() {
        assert_eq!(parse_level("INFO"), Some(tracing::Level::INFO));
        assert_eq!(parse_level(" debug "), Some(tracing::Level::DEBUG));
        assert_eq!(parse_level("nope"), None);
    }

    #[test]
    fn status_dump_defaults_to_disabled() {
        let cfg = Config::from_toml(FULL).unwrap();
        assert_eq!(cfg.status_dump, StatusDumpConfig::default());
        assert!(!cfg.status_dump.enabled);
        assert_eq!(cfg.status_dump.file_name, "status.json");
        assert_eq!(cfg.status_dump.disk_label, None);
        assert_eq!(cfg.status_dump.disk_path, None);
    }

    #[test]
    fn status_dump_parses_with_a_disk_label() {
        let cfg = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[status-dump]
enabled = true
disk_label = "CIRCUITPY"
file_name = "status.json"
"#,
        )
        .unwrap();
        assert!(cfg.status_dump.enabled);
        assert_eq!(cfg.status_dump.disk_label.as_deref(), Some("CIRCUITPY"));
        assert_eq!(cfg.status_dump.disk_path, None);
        assert_eq!(cfg.status_dump.file_name, "status.json");
    }

    #[test]
    fn status_dump_parses_with_a_disk_path() {
        let cfg = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[status-dump]
enabled = true
disk_path = "E:\\"
"#,
        )
        .unwrap();
        assert_eq!(cfg.status_dump.disk_path.as_deref(), Some("E:\\"));
        // file_name still defaults even though the section is present.
        assert_eq!(cfg.status_dump.file_name, "status.json");
    }

    #[test]
    fn status_dump_disabled_does_not_require_a_location() {
        // enabled defaults to false, so an empty [status-dump] section is fine.
        assert!(Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[status-dump]
"#,
        )
        .is_ok());
    }

    #[test]
    fn status_dump_rejects_both_disk_label_and_disk_path() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[status-dump]
enabled = true
disk_label = "CIRCUITPY"
disk_path = "E:\\"
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("only one of"), "{err:#}");
    }

    #[test]
    fn status_dump_enabled_requires_a_location() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[status-dump]
enabled = true
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("disk_label or disk_path"), "{err:#}");
    }

    #[test]
    fn status_dump_rejects_an_empty_file_name() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[status-dump]
enabled = true
disk_label = "CIRCUITPY"
file_name = "   "
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("file_name"), "{err:#}");
    }

    #[test]
    fn status_dump_rejects_a_file_name_that_is_a_path() {
        for bad in ["sub/status.json", "sub\\status.json", ".."] {
            let err = Config::from_toml(&format!(
                r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[status-dump]
enabled = true
disk_label = "CIRCUITPY"
file_name = '{bad}'
"#
            ))
            .unwrap_err();
            assert!(
                format!("{err:#}").contains("plain file name"),
                "file_name = {bad}: {err:#}"
            );
        }
    }

    #[test]
    fn durations_come_from_the_monitor_section() {
        let cfg = Config::from_toml(FULL).unwrap();
        assert_eq!(cfg.poll_interval(), std::time::Duration::from_secs(5));
        assert_eq!(
            cfg.disconnect_threshold(),
            std::time::Duration::from_secs(120)
        );
    }
}
