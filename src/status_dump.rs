//! Writing a live `status.json` for something else to read -- a microcontroller polling
//! it from an attached drive is the motivating case (see `[status-dump]` in
//! `autospot.toml`).
//!
//! Two halves: a pure `compute()` that turns already-gathered state into a `StatusDump`
//! (unit tested, no Windows calls), and the disk/file I/O that resolves the configured
//! location and writes it (best-effort, not unit tested against real hardware for the
//! same reason `adapters::list()` isn't -- it needs a real disk).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};
use windows::core::HSTRING;

use crate::config::CompanionConfig;

/// SSID placeholder for a "connected" report that is not backed by a readable Wi-Fi
/// SSID. Autospot only ever monitors Wi-Fi (`wlanapi` has no concept of Ethernet), so in
/// practice this fires only when a Wi-Fi adapter reports itself connected but the SSID
/// query failed -- there is no other code path that produces "connected" without an
/// SSID at all.
pub const NON_WIFI_SSID: &str = "<ethernet>";

/// The document written to disk.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusDump {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected: Option<ConnectedInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotspot: Option<HotspotInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConnectedInfo {
    pub ssid: String,
    pub ip_address: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HotspotInfo {
    pub ssid: String,
    pub password: String,
    pub ip_address: String,
}

impl StatusDump {
    fn connected(ssid: Option<&str>, ip_address: Option<&str>) -> Self {
        Self {
            status: "connected",
            connected: Some(ConnectedInfo {
                ssid: ssid.unwrap_or(NON_WIFI_SSID).to_string(),
                ip_address: ip_address.unwrap_or_default().to_string(),
            }),
            hotspot: None,
        }
    }

    fn hotspot(ssid: &str, password: &str, ip_address: Option<&str>) -> Self {
        Self {
            status: "hotspot",
            connected: None,
            hotspot: Some(HotspotInfo {
                ssid: ssid.to_string(),
                password: password.to_string(),
                ip_address: ip_address.unwrap_or_default().to_string(),
            }),
        }
    }

    fn disconnected() -> Self {
        Self {
            status: "disconnected",
            connected: None,
            hotspot: None,
        }
    }

    fn unknown() -> Self {
        Self {
            status: "unknown",
            connected: None,
            hotspot: None,
        }
    }
}

/// The Wi-Fi half of what `compute()` needs, already resolved by the caller.
pub struct WifiSnapshot<'a> {
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

/// Decide what the dump should say this tick. Pure: no I/O, no Windows calls, so it is
/// exercised directly by unit tests below.
///
/// Priority when both could apply: a real Wi-Fi connection always wins over a hotspot
/// that has not been torn down yet (mirrors the watchdog's own reconnect handling).
pub fn compute(
    wifi_ok: bool,
    wifi: &WifiSnapshot,
    hotspot: Option<&HotspotSnapshot>,
) -> StatusDump {
    if !wifi_ok {
        return StatusDump::unknown();
    }
    if wifi.connected {
        return StatusDump::connected(wifi.ssid, wifi.ip_address);
    }
    if let Some(h) = hotspot {
        if h.on {
            return StatusDump::hotspot(h.ssid, h.password, h.ip_address);
        }
    }
    StatusDump::disconnected()
}

/// Write `dump` to the configured location if it differs from `last`, updating `last` on
/// success. Returns `Ok(true)` when a write happened, `Ok(false)` when nothing changed.
///
/// Resolves the target disk fresh on every call (see `resolve_root`) so a drive that was
/// unplugged and replugged -- possibly under a different letter -- is still found.
pub fn write_if_changed(
    cfg: &CompanionConfig,
    dump: &StatusDump,
    last: &mut Option<StatusDump>,
) -> Result<bool> {
    if last.as_ref() == Some(dump) {
        return Ok(false);
    }

    let root = resolve_root(cfg)?;
    let target = root.join(&cfg.file_name);
    write_atomically(&target, dump)?;

    *last = Some(dump.clone());
    Ok(true)
}

/// Resolve `disk_label` or `disk_path` (config validation guarantees exactly one is set)
/// to a root directory to write into.
fn resolve_root(cfg: &CompanionConfig) -> Result<PathBuf> {
    if let Some(path) = non_empty(cfg.disk_path.as_deref()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(label) = non_empty(cfg.disk_label.as_deref()) {
        return find_disk_by_label(label)?
            .ok_or_else(|| anyhow::anyhow!("no disk labeled '{label}' is currently present"));
    }
    // Unreachable in practice: Config::validate() rejects enabled=true with neither set.
    bail!("status-dump is enabled but has neither disk_label nor disk_path");
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Search every fixed or removable drive letter for one whose volume label matches.
fn find_disk_by_label(label: &str) -> Result<Option<PathBuf>> {
    let label = label.trim();
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        bail!("GetLogicalDrives reported no drives at all");
    }

    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let root_hs = HSTRING::from(&root);

        // Skip network/optical/RAM drives: querying an empty optical drive in particular
        // can be slow, and neither is a sensible place for a status file anyway.
        let drive_type = unsafe { GetDriveTypeW(&root_hs) };
        if drive_type != DRIVE_FIXED && drive_type != DRIVE_REMOVABLE {
            continue;
        }

        let mut name_buf = [0u16; 256];
        // A removable bay with no media in it, or a drive that vanished mid-scan, both
        // fail here; that's routine, not a reason to abort the whole search.
        if unsafe { GetVolumeInformationW(&root_hs, Some(&mut name_buf), None, None, None, None) }
            .is_err()
        {
            continue;
        }

        if wide_to_string(&name_buf).eq_ignore_ascii_case(label) {
            return Ok(Some(PathBuf::from(root)));
        }
    }

    Ok(None)
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Write via a temp file + rename so a reader polling the file never sees a partial
/// write. `std::fs::rename` on Windows replaces an existing destination.
fn write_atomically(target: &Path, dump: &StatusDump) -> Result<()> {
    let json = serde_json::to_vec_pretty(dump).context("serializing companion status")?;

    let tmp_name = format!(
        "{}.tmp",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("status.json")
    );
    let tmp_path = target.with_file_name(tmp_name);

    std::fs::write(&tmp_path, &json).with_context(|| format!("writing {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, target)
        .with_context(|| format!("renaming {} to {}", tmp_path.display(), target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi_connected<'a>(ssid: Option<&'a str>, ip: Option<&'a str>) -> WifiSnapshot<'a> {
        WifiSnapshot {
            connected: true,
            ssid,
            ip_address: ip,
        }
    }

    fn wifi_disconnected() -> WifiSnapshot<'static> {
        WifiSnapshot {
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

    fn hotspot_off<'a>() -> HotspotSnapshot<'a> {
        HotspotSnapshot {
            on: false,
            ssid: "",
            password: "",
            ip_address: None,
        }
    }

    #[test]
    fn wifi_query_failure_reports_unknown_regardless_of_everything_else() {
        let dump = compute(
            false,
            &wifi_connected(Some("home"), Some("192.168.1.2")),
            Some(&hotspot_on("AP", "pw", Some("192.168.137.1"))),
        );
        assert_eq!(dump, StatusDump::unknown());
        assert_eq!(dump.status, "unknown");
        assert!(dump.connected.is_none());
        assert!(dump.hotspot.is_none());
    }

    #[test]
    fn connected_wifi_reports_ssid_and_ip() {
        let dump = compute(
            true,
            &wifi_connected(Some("home"), Some("192.168.1.2")),
            None,
        );
        assert_eq!(dump.status, "connected");
        assert_eq!(
            dump.connected,
            Some(ConnectedInfo {
                ssid: "home".to_string(),
                ip_address: "192.168.1.2".to_string(),
            })
        );
        assert!(dump.hotspot.is_none());
    }

    #[test]
    fn connected_without_a_readable_ssid_uses_the_ethernet_placeholder() {
        let dump = compute(true, &wifi_connected(None, Some("192.168.1.2")), None);
        assert_eq!(dump.connected.unwrap().ssid, NON_WIFI_SSID);
    }

    #[test]
    fn connected_without_an_ip_yet_uses_an_empty_string() {
        let dump = compute(true, &wifi_connected(Some("home"), None), None);
        assert_eq!(dump.connected.unwrap().ip_address, "");
    }

    #[test]
    fn hotspot_on_reports_when_wifi_is_down() {
        let dump = compute(
            true,
            &wifi_disconnected(),
            Some(&hotspot_on(
                "MyFallbackHotspot",
                "changeme123",
                Some("192.168.137.1"),
            )),
        );
        assert_eq!(dump.status, "hotspot");
        assert_eq!(
            dump.hotspot,
            Some(HotspotInfo {
                ssid: "MyFallbackHotspot".to_string(),
                password: "changeme123".to_string(),
                ip_address: "192.168.137.1".to_string(),
            })
        );
        assert!(dump.connected.is_none());
    }

    #[test]
    fn connected_wifi_wins_over_a_hotspot_still_shutting_down() {
        let dump = compute(
            true,
            &wifi_connected(Some("home"), Some("192.168.1.2")),
            Some(&hotspot_on("AP", "pw", None)),
        );
        assert_eq!(dump.status, "connected");
    }

    #[test]
    fn wifi_down_and_hotspot_off_is_disconnected() {
        let dump = compute(true, &wifi_disconnected(), Some(&hotspot_off()));
        assert_eq!(dump, StatusDump::disconnected());
    }

    #[test]
    fn wifi_down_with_no_hotspot_info_at_all_is_disconnected() {
        let dump = compute(true, &wifi_disconnected(), None);
        assert_eq!(dump, StatusDump::disconnected());
    }

    #[test]
    fn disconnected_and_unknown_carry_no_other_fields() {
        assert_eq!(
            serde_json::to_string(&StatusDump::disconnected()).unwrap(),
            r#"{"status":"disconnected"}"#
        );
        assert_eq!(
            serde_json::to_string(&StatusDump::unknown()).unwrap(),
            r#"{"status":"unknown"}"#
        );
    }

    #[test]
    fn only_one_of_connected_or_hotspot_is_ever_serialized() {
        let connected =
            serde_json::to_value(&StatusDump::connected(Some("home"), Some("1.2.3.4"))).unwrap();
        assert!(connected.get("connected").is_some());
        assert!(connected.get("hotspot").is_none());

        let hotspot =
            serde_json::to_value(&StatusDump::hotspot("AP", "pw", Some("1.2.3.4"))).unwrap();
        assert!(hotspot.get("hotspot").is_some());
        assert!(hotspot.get("connected").is_none());
    }

    #[test]
    fn write_if_changed_skips_a_write_when_nothing_changed() {
        let dir = std::env::temp_dir().join(format!("autospot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = CompanionConfig {
            enabled: true,
            disk_label: None,
            disk_path: Some(dir.to_string_lossy().into_owned()),
            file_name: "status.json".to_string(),
        };

        let mut last = None;
        let dump = StatusDump::connected(Some("home"), Some("192.168.1.2"));

        assert!(write_if_changed(&cfg, &dump, &mut last).unwrap());
        assert_eq!(last.as_ref(), Some(&dump));
        // Second call, same dump: no write.
        assert!(!write_if_changed(&cfg, &dump, &mut last).unwrap());

        let written = std::fs::read_to_string(dir.join("status.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(v["status"], "connected");
        assert_eq!(v["connected"]["ssid"], "home");
        assert_eq!(v["connected"]["ip_address"], "192.168.1.2");
        assert!(v.get("hotspot").is_none());

        // A changed dump does write, and overwrites the previous file's content.
        let dump2 = StatusDump::disconnected();
        assert_eq!(write_if_changed(&cfg, &dump2, &mut last).unwrap(), true);
        let written2 = std::fs::read_to_string(dir.join("status.json")).unwrap();
        assert_eq!(written2, serde_json::to_string_pretty(&dump2).unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_root_uses_disk_path_directly_without_touching_windows_apis() {
        let cfg = CompanionConfig {
            enabled: true,
            disk_label: None,
            disk_path: Some("Z:\\some\\path".to_string()),
            file_name: "status.json".to_string(),
        };
        assert_eq!(resolve_root(&cfg).unwrap(), PathBuf::from("Z:\\some\\path"));
    }
}
