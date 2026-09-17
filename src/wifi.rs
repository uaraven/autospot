//! Wi-Fi connection status via the Win32 native Wi-Fi API (`wlanapi.dll`).

use tracing::debug;
use anyhow::{bail, Result};
use windows::core::GUID;
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::NetworkManagement::WiFi::{
    wlan_intf_opcode_current_connection, wlan_interface_state_connected, WlanCloseHandle,
    WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
    WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST,
};

/// What a single Wi-Fi adapter is currently doing.
#[derive(Debug, Clone)]
pub struct InterfaceStatus {
    pub description: String,
    pub connected: bool,
    /// SSID of the current connection, when connected and readable.
    pub ssid: Option<String>,
}

/// Snapshot of every Wi-Fi adapter on the machine.
#[derive(Debug, Clone, Default)]
pub struct WifiStatus {
    pub interfaces: Vec<InterfaceStatus>,
}

impl WifiStatus {
    /// The adapter, if any, that counts as our real Wi-Fi connection.
    ///
    /// `ignore_ssid` exists because switching on Mobile Hotspot can make the Wi-Fi
    /// adapter (or a virtual AP interface) report itself as "connected" to our own
    /// hotspot SSID. Counting that as Wi-Fi being back would make the watchdog turn the
    /// hotspot straight off again, and flap.
    pub fn active_interface(&self, ignore_ssid: Option<&str>) -> Option<&InterfaceStatus> {
        self.interfaces.iter().find(|i| {
            i.connected
                && match (&i.ssid, ignore_ssid) {
                    (Some(ssid), Some(ignore)) => !ssid.eq_ignore_ascii_case(ignore),
                    _ => true,
                }
        })
    }

    /// True when at least one adapter is associated with a real network.
    pub fn is_connected(&self, ignore_ssid: Option<&str>) -> bool {
        self.active_interface(ignore_ssid).is_some()
    }

    /// SSID of the active adapter, for logging.
    pub fn connected_ssid(&self, ignore_ssid: Option<&str>) -> Option<&str> {
        self.active_interface(ignore_ssid)
            .and_then(|i| i.ssid.as_deref())
    }
}

/// RAII wrapper around the WLAN client handle.
struct WlanHandle(HANDLE);

impl WlanHandle {
    fn open() -> Result<Self> {
        let mut negotiated = 0u32;
        let mut handle = HANDLE::default();
        // Client version 2 = Vista and later.
        let rc = unsafe { WlanOpenHandle(2, None, &mut negotiated, &mut handle) };
        if rc != ERROR_SUCCESS.0 {
            bail!("WlanOpenHandle failed with error {rc}");
        }
        Ok(Self(handle))
    }
}

impl Drop for WlanHandle {
    fn drop(&mut self) {
        unsafe {
            WlanCloseHandle(self.0, None);
        }
    }
}

/// Read the state of every Wi-Fi adapter.
pub fn query() -> Result<WifiStatus> {
    let handle = WlanHandle::open()?;

    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    let rc = unsafe { WlanEnumInterfaces(handle.0, None, &mut list) };
    if rc != ERROR_SUCCESS.0 {
        bail!("WlanEnumInterfaces failed with error {rc}");
    }
    if list.is_null() {
        return Ok(WifiStatus::default());
    }

    // SAFETY: WlanEnumInterfaces returned success and a non-null list, whose
    // `InterfaceInfo` is a variable-length array of `dwNumberOfItems` entries.
    let interfaces = unsafe {
        let count = (*list).dwNumberOfItems as usize;
        let infos = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), count);
        let collected: Vec<InterfaceStatus> = infos
            .iter()
            .map(|info| read_interface(handle.0, info))
            .collect();
        WlanFreeMemory(list as *const _);
        collected
    };

    Ok(WifiStatus { interfaces })
}

fn read_interface(handle: HANDLE, info: &WLAN_INTERFACE_INFO) -> InterfaceStatus {
    let connected = info.isState == wlan_interface_state_connected;
    debug!("Interface status {:?}", info.isState);
    InterfaceStatus {
        description: wide_to_string(&info.strInterfaceDescription),
        connected,
        // The SSID only exists while associated, so skip the query otherwise.
        ssid: if connected {
            current_ssid(handle, &info.InterfaceGuid)
        } else {
            None
        },
    }
}

/// Query `wlan_intf_opcode_current_connection` for the associated SSID.
///
/// Best effort: a failure here only costs us a nicer log line, so it is not fatal.
fn current_ssid(handle: HANDLE, guid: &GUID) -> Option<String> {
    let mut size = 0u32;
    let mut data: *mut std::ffi::c_void = std::ptr::null_mut();

    let rc = unsafe {
        WlanQueryInterface(
            handle,
            guid,
            wlan_intf_opcode_current_connection,
            None,
            &mut size,
            &mut data,
            None,
        )
    };
    if rc != ERROR_SUCCESS.0 || data.is_null() {
        return None;
    }

    // SAFETY: on success the call yields a WLAN_CONNECTION_ATTRIBUTES buffer that we
    // own and must release with WlanFreeMemory.
    unsafe {
        let attrs = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
        let ssid = &attrs.wlanAssociationAttributes.dot11Ssid;
        let len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
        let name = String::from_utf8_lossy(&ssid.ucSSID[..len]).into_owned();
        WlanFreeMemory(data as *const _);
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }
}

/// Convert a fixed-size, NUL-padded UTF-16 buffer into a `String`.
fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(connected: bool, ssid: Option<&str>) -> InterfaceStatus {
        InterfaceStatus {
            description: "test".into(),
            connected,
            ssid: ssid.map(str::to_string),
        }
    }

    #[test]
    fn no_adapters_means_not_connected() {
        assert!(!WifiStatus::default().is_connected(None));
    }

    #[test]
    fn a_connected_adapter_counts() {
        let status = WifiStatus {
            interfaces: vec![iface(false, None), iface(true, Some("home"))],
        };
        assert!(status.is_connected(None));
        assert_eq!(status.connected_ssid(None), Some("home"));
    }

    #[test]
    fn our_own_hotspot_ssid_does_not_count_as_wifi() {
        let status = WifiStatus {
            interfaces: vec![iface(true, Some("MyFallbackHotspot"))],
        };
        assert!(!status.is_connected(Some("MyFallbackHotspot")));
        assert!(!status.is_connected(Some("myfallbackhotspot")));
        assert!(status.is_connected(Some("SomethingElse")));
    }

    #[test]
    fn a_real_network_still_counts_while_the_hotspot_runs() {
        let status = WifiStatus {
            interfaces: vec![iface(true, Some("MyFallbackHotspot")), iface(true, Some("home"))],
        };
        assert!(status.is_connected(Some("MyFallbackHotspot")));
        assert_eq!(status.connected_ssid(Some("MyFallbackHotspot")), Some("home"));
    }

    #[test]
    fn connected_without_a_readable_ssid_still_counts() {
        let status = WifiStatus {
            interfaces: vec![iface(true, None)],
        };
        assert!(status.is_connected(Some("MyFallbackHotspot")));
    }

    #[test]
    fn trims_at_the_first_nul() {
        let mut buf = [0u16; 8];
        for (i, c) in "Wi-Fi".encode_utf16().enumerate() {
            buf[i] = c;
        }
        assert_eq!(wide_to_string(&buf), "Wi-Fi");
    }
}
