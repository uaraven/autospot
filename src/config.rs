//! Configuration loading: the `autospot.toml` file described in `docs/implementation.md`.
//!
//! This module deliberately stays free of Windows API types so the parsing tests
//! can be run on any host.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// Name of the config file looked up next to the executable when no path is given.
pub const DEFAULT_FILE_NAME: &str = "autospot.toml";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub monitor: MonitorConfig,
    pub hotspot: HotspotConfig,
    #[serde(default)]
    pub companion: CompanionConfig,
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
    /// Adapter whose connection is shared. Matched against the adapter's friendly name
    /// ("Ethernet"), its hardware description, or the network profile name,
    /// case-insensitively. The literal value `"auto"` instead picks, on every lookup,
    /// a wired Ethernet adapter over any other adapter type outright, regardless of
    /// connectivity; connectivity rank only breaks a tie between candidates of the same
    /// type. Either way, the hotspot's own virtual adapter and whatever `hotspot_adapter`
    /// resolves to are always skipped -- one adapter can't be both its own uplink and
    /// its own hotspot.
    pub uplink_adapter: String,
    /// Adapter Windows will use to broadcast the hotspot's own Wi-Fi access point.
    /// Resolved automatically ("auto", the default) when there is exactly one Wi-Fi
    /// adapter present; with two or more Wi-Fi adapters it's ambiguous which one Windows
    /// will actually use, so it must be set explicitly here (together with
    /// `uplink_adapter`, if that also needs pinning down). This is only used to keep
    /// `uplink_adapter` from ever resolving to the same adapter: one Wi-Fi radio can't
    /// reliably act as both a client and an access point at the same time, which is why
    /// a hotspot started this way tends to accept clients but never hand out an IP.
    #[serde(default = "default_hotspot_adapter")]
    pub hotspot_adapter: String,
}

/// The `uplink_adapter`/`hotspot_adapter` value that means "let Autospot resolve this
/// automatically instead of using a specific configured adapter".
pub const AUTO_UPLINK: &str = "auto";

fn default_hotspot_adapter() -> String {
    AUTO_UPLINK.to_string()
}

pub(crate) fn is_auto(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case(AUTO_UPLINK)
}

impl HotspotConfig {
    /// Is `uplink_adapter` set to the auto-selection sentinel?
    pub fn is_auto_uplink(&self) -> bool {
        is_auto(&self.uplink_adapter)
    }

    /// Is `hotspot_adapter` set to the auto-detection sentinel?
    pub fn is_auto_hotspot_adapter(&self) -> bool {
        is_auto(&self.hotspot_adapter)
    }
}

/// Whether to report Wi-Fi/hotspot status to a companion microcontroller over serial.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompanionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub vid: Option<u16>,
    #[serde(default)]
    pub pid: Option<u16>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// Level for the rotated log file. Defaults to `error` so routine status lines
    /// don't pile up on disk -- the console is the primary place output is read.
    #[serde(default = "default_file_log_level")]
    pub file_level: String,
    /// Level for the console. Defaults to `info` so the status line is visible while
    /// the app runs in the foreground.
    #[serde(default = "default_stdout_log_level")]
    pub stdout_level: String,
    /// Level for the rotated log file when running as the Windows service. There is no
    /// console in that mode, so the file is the only place output is read -- defaults
    /// to `info`, matching what `stdout_level` would otherwise have shown live.
    #[serde(default = "default_service_log_level")]
    pub service_level: String,
    /// Log file path. A relative path is resolved against
    /// `%USERPROFILE%\Documents\autospot\logs` in console/application mode, or
    /// `%ProgramData%\autospot\logs` when running as the service -- see
    /// `user_documents_log_dir` and `service::program_data_log_dir` in the source.
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
fn default_file_log_level() -> String {
    "error".to_string()
}
fn default_stdout_log_level() -> String {
    "info".to_string()
}
fn default_service_log_level() -> String {
    "info".to_string()
}
fn default_log_path() -> PathBuf {
    PathBuf::from("autospot.log")
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
            file_level: default_file_log_level(),
            stdout_level: default_stdout_log_level(),
            service_level: default_service_log_level(),
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
        if self.hotspot.hotspot_adapter.trim().is_empty() {
            bail!("hotspot.hotspot_adapter must not be empty");
        }
        if !self.hotspot.is_auto_uplink()
            && !self.hotspot.is_auto_hotspot_adapter()
            && self
                .hotspot
                .uplink_adapter
                .trim()
                .eq_ignore_ascii_case(self.hotspot.hotspot_adapter.trim())
        {
            bail!(
                "hotspot.uplink_adapter and hotspot.hotspot_adapter must not both name the \
                 same adapter -- it can't be its own uplink and its own hotspot at once"
            );
        }

        if parse_level(&self.logging.file_level).is_none() {
            bail!(
                "logging.file_level must be one of error|warn|info|debug|trace, got '{}'",
                self.logging.file_level
            );
        }
        if parse_level(&self.logging.stdout_level).is_none() {
            bail!(
                "logging.stdout_level must be one of error|warn|info|debug|trace, got '{}'",
                self.logging.stdout_level
            );
        }
        if parse_level(&self.logging.service_level).is_none() {
            bail!(
                "logging.service_level must be one of error|warn|info|debug|trace, got '{}'",
                self.logging.service_level
            );
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
file_level = "warn"
stdout_level = "debug"
service_level = "trace"
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
        assert_eq!(cfg.logging.file_level, "warn");
        assert_eq!(cfg.logging.stdout_level, "debug");
        assert_eq!(cfg.logging.service_level, "trace");
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
    fn rejects_unknown_file_log_level() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[logging]
file_level = "verbose"
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("logging.file_level"), "{err:#}");
    }

    #[test]
    fn rejects_unknown_stdout_log_level() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[logging]
stdout_level = "verbose"
"#,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("logging.stdout_level"),
            "{err:#}"
        );
    }

    #[test]
    fn rejects_unknown_service_log_level() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[logging]
service_level = "verbose"
"#,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("logging.service_level"),
            "{err:#}"
        );
    }

    #[test]
    fn logging_levels_default_to_error_for_file_and_info_for_stdout_and_service() {
        let cfg = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"
"#,
        )
        .unwrap();
        assert_eq!(cfg.logging.file_level, "error");
        assert_eq!(cfg.logging.stdout_level, "info");
        assert_eq!(cfg.logging.service_level, "info");
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
        assert!(
            Config::from_toml(
                r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "   "
"#,
            )
            .is_err()
        );
    }

    #[test]
    fn hotspot_adapter_defaults_to_auto() {
        let cfg = Config::from_toml(FULL).unwrap();
        assert!(cfg.hotspot.is_auto_hotspot_adapter());
    }

    #[test]
    fn rejects_empty_hotspot_adapter() {
        assert!(
            Config::from_toml(
                r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"
hotspot_adapter = "   "
"#,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_uplink_adapter_and_hotspot_adapter_naming_the_same_adapter() {
        let err = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "WiFi"
hotspot_adapter = "WiFi"
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("must not both name"), "{err:#}");
    }

    #[test]
    fn allows_uplink_adapter_and_hotspot_adapter_to_both_be_auto() {
        // "auto" naming "auto" isn't the same-adapter conflict -- it just means neither
        // is pinned down yet, which is resolved (or fails) at runtime, not config load.
        assert!(
            Config::from_toml(
                r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "auto"
hotspot_adapter = "auto"
"#,
            )
            .is_ok()
        );
    }

    #[test]
    fn level_strings_map_to_tracing_levels() {
        assert_eq!(parse_level("INFO"), Some(tracing::Level::INFO));
        assert_eq!(parse_level(" debug "), Some(tracing::Level::DEBUG));
        assert_eq!(parse_level("nope"), None);
    }

    #[test]
    fn companion_defaults_to_disabled() {
        let cfg = Config::from_toml(FULL).unwrap();
        assert_eq!(cfg.companion, CompanionConfig::default());
        assert!(!cfg.companion.enabled);
    }

    #[test]
    fn companion_can_be_enabled() {
        let cfg = Config::from_toml(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"

[companion]
enabled = true
"#,
        )
        .unwrap();
        assert!(cfg.companion.enabled);
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
