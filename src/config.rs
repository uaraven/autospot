//! Configuration loading: the `autospot.toml` file documented in the readme.
//!
//! This module deliberately stays free of Windows API types so the parsing tests
//! can be run on any host.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// Name of the config file looked up next to the executable when no path is given.
pub const DEFAULT_FILE_NAME: &str = "autospot.toml";

/// The `uplink_adapter`/`hotspot_adapter` value that means "let Autospot resolve this
/// automatically instead of using a specific configured adapter".
pub const AUTO_ADAPTER: &str = "auto";

/// Is an adapter setting the [`AUTO_ADAPTER`] sentinel?
pub(crate) fn is_auto(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case(AUTO_ADAPTER)
}

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

impl Config {
    /// Parse and validate a config from a TOML string.
    pub fn from_toml(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).context("config is not valid TOML")?;
        cfg.monitor.validate()?;
        cfg.hotspot.validate()?;
        cfg.logging.validate()?;
        Ok(cfg)
    }

    /// Load and validate the config at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        Self::from_toml(&text).with_context(|| format!("in config file {}", path.display()))
    }
}

// --- [monitor] -------------------------------------------------------------------

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

fn default_poll_interval_secs() -> u64 {
    5
}
fn default_disconnect_threshold_secs() -> u64 {
    120
}
fn default_true() -> bool {
    true
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

impl MonitorConfig {
    /// How long to wait between polls.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs)
    }

    /// How long Wi-Fi must stay down before the hotspot goes up.
    pub fn disconnect_threshold(&self) -> Duration {
        Duration::from_secs(self.disconnect_threshold_secs)
    }

    /// A zero interval would spin the watchdog loop; a zero threshold would make it
    /// fire before the first poll even completes.
    fn validate(&self) -> Result<()> {
        if self.poll_interval_secs == 0 {
            bail!("monitor.poll_interval_secs must be at least 1");
        }
        if self.disconnect_threshold_secs == 0 {
            bail!("monitor.disconnect_threshold_secs must be at least 1");
        }
        Ok(())
    }
}

// --- [hotspot] -------------------------------------------------------------------

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

fn default_hotspot_adapter() -> String {
    AUTO_ADAPTER.to_string()
}

/// WPA2-PSK limits, enforced by Windows: an SSID is at most 32 bytes and a passphrase is
/// 8 to 63 characters. Checked here because `ConfigureAccessPointAsync` would otherwise
/// report a bare `E_INVALIDARG` much later.
const SSID_MAX_BYTES: usize = 32;
const PASSPHRASE_CHARS: std::ops::RangeInclusive<usize> = 8..=63;

impl HotspotConfig {
    /// Is `uplink_adapter` set to the auto-selection sentinel?
    pub fn is_auto_uplink(&self) -> bool {
        is_auto(&self.uplink_adapter)
    }

    /// Is `hotspot_adapter` set to the auto-detection sentinel?
    pub fn is_auto_hotspot_adapter(&self) -> bool {
        is_auto(&self.hotspot_adapter)
    }

    fn validate(&self) -> Result<()> {
        let ssid_len = self.ssid.len();
        if ssid_len == 0 {
            bail!("hotspot.ssid must not be empty");
        }
        if ssid_len > SSID_MAX_BYTES {
            bail!("hotspot.ssid must be at most {SSID_MAX_BYTES} bytes, got {ssid_len}");
        }

        let pass_len = self.passphrase.chars().count();
        if !PASSPHRASE_CHARS.contains(&pass_len) {
            bail!(
                "hotspot.passphrase must be {}-{} characters, got {pass_len}",
                PASSPHRASE_CHARS.start(),
                PASSPHRASE_CHARS.end()
            );
        }

        if self.uplink_adapter.trim().is_empty() {
            bail!("hotspot.uplink_adapter must not be empty");
        }
        if self.hotspot_adapter.trim().is_empty() {
            bail!("hotspot.hotspot_adapter must not be empty");
        }

        // Two "auto"s aren't a conflict -- neither is pinned down yet, and that is
        // resolved (or reported) at runtime rather than here.
        let both_named = !self.is_auto_uplink() && !self.is_auto_hotspot_adapter();
        let same_adapter = self
            .uplink_adapter
            .trim()
            .eq_ignore_ascii_case(self.hotspot_adapter.trim());
        if both_named && same_adapter {
            bail!(
                "hotspot.uplink_adapter and hotspot.hotspot_adapter must not both name the \
                 same adapter -- it can't be its own uplink and its own hotspot at once"
            );
        }

        Ok(())
    }
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

// --- [companion] -----------------------------------------------------------------

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

// --- [logging] -------------------------------------------------------------------

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
    /// Log file path. A relative path is resolved against the log directory of whichever
    /// mode is running -- see `logging::Mode`.
    #[serde(default = "default_log_path")]
    pub path: PathBuf,
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

impl LoggingConfig {
    fn validate(&self) -> Result<()> {
        for (key, value) in [
            ("logging.file_level", &self.file_level),
            ("logging.stdout_level", &self.stdout_level),
            ("logging.service_level", &self.service_level),
        ] {
            if parse_level(value).is_none() {
                bail!("{key} must be one of error|warn|info|debug|trace, got '{value}'");
            }
        }
        Ok(())
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

    /// The minimal valid config -- only `[hotspot]` is required -- with `extra` lines
    /// appended. Tests that need to change one of the required keys spell the whole
    /// config out instead.
    fn minimal(extra: &str) -> Result<Config> {
        Config::from_toml(&format!(
            r#"
[hotspot]
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "Ethernet"
{extra}
"#
        ))
    }

    /// The error message from a config that must not parse.
    fn error(extra: &str) -> String {
        format!("{:#}", minimal(extra).unwrap_err())
    }

    /// The error message from a `[hotspot]`-only config that must not parse.
    fn hotspot_error(section: &str) -> String {
        let err = Config::from_toml(&format!("[hotspot]\n{section}\n")).unwrap_err();
        format!("{err:#}")
    }

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
        let cfg = minimal("").unwrap();
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
            let cfg = minimal(&format!("band = \"{text}\"")).unwrap();
            assert_eq!(cfg.hotspot.band, expected, "band = {text}");
        }
    }

    #[test]
    fn rejects_unknown_keys_so_typos_are_not_silently_defaulted() {
        let err = error("\n[monitor]\npool_interval_secs = 5");
        assert!(
            err.contains("pool_interval_secs"),
            "error should name the offending key: {err}"
        );
    }

    #[test]
    fn rejects_missing_hotspot_section() {
        assert!(Config::from_toml("[monitor]\npoll_interval_secs = 5\n").is_err());
    }

    #[test]
    fn rejects_short_passphrase() {
        let err = hotspot_error(
            r#"
ssid = "Fallback"
passphrase = "short"
uplink_adapter = "Ethernet"
"#,
        );
        assert!(err.contains("8-63"), "{err}");
    }

    #[test]
    fn rejects_oversized_ssid() {
        let err = hotspot_error(&format!(
            r#"
ssid = "{}"
passphrase = "password1"
uplink_adapter = "Ethernet"
"#,
            "x".repeat(SSID_MAX_BYTES + 1)
        ));
        assert!(err.contains("32 bytes"), "{err}");
    }

    #[test]
    fn rejects_zero_poll_interval() {
        let err = error("\n[monitor]\npoll_interval_secs = 0");
        assert!(err.contains("poll_interval_secs"), "{err}");
    }

    #[test]
    fn rejects_an_unknown_level_naming_the_offending_key() {
        for key in ["file_level", "stdout_level", "service_level"] {
            let err = error(&format!("\n[logging]\n{key} = \"verbose\""));
            assert!(err.contains(&format!("logging.{key}")), "{err}");
        }
    }

    #[test]
    fn logging_levels_default_to_error_for_file_and_info_for_stdout_and_service() {
        let cfg = minimal("").unwrap();
        assert_eq!(cfg.logging.file_level, "error");
        assert_eq!(cfg.logging.stdout_level, "info");
        assert_eq!(cfg.logging.service_level, "info");
    }

    #[test]
    fn recognises_the_auto_sentinel_case_and_space_insensitively() {
        for value in ["auto", "Auto", "AUTO", "  auto  "] {
            assert!(is_auto(value), "{value:?}");
        }
        assert!(!is_auto("Ethernet"));
        assert!(!Config::from_toml(FULL).unwrap().hotspot.is_auto_uplink());
    }

    #[test]
    fn rejects_an_empty_adapter_name() {
        let err = hotspot_error(
            r#"
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "   "
"#,
        );
        assert!(err.contains("uplink_adapter"), "{err}");
        assert!(minimal(r#"hotspot_adapter = "   ""#).is_err());
    }

    #[test]
    fn hotspot_adapter_defaults_to_auto() {
        let cfg = Config::from_toml(FULL).unwrap();
        assert!(cfg.hotspot.is_auto_hotspot_adapter());
    }

    #[test]
    fn rejects_uplink_adapter_and_hotspot_adapter_naming_the_same_adapter() {
        let err = hotspot_error(
            r#"
ssid = "Fallback"
passphrase = "password1"
uplink_adapter = "WiFi"
hotspot_adapter = "WiFi"
"#,
        );
        assert!(err.contains("must not both name"), "{err}");
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
        let cfg = minimal("\n[companion]\nenabled = true").unwrap();
        assert!(cfg.companion.enabled);
    }

    #[test]
    fn durations_come_from_the_monitor_section() {
        let monitor = Config::from_toml(FULL).unwrap().monitor;
        assert_eq!(monitor.poll_interval(), Duration::from_secs(5));
        assert_eq!(monitor.disconnect_threshold(), Duration::from_secs(120));
    }
}
