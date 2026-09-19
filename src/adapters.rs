//! Network adapter inventory via IP Helper (`GetAdaptersAddresses`).
//!
//! WinRT's `NetworkAdapter` only exposes an interface GUID, but the config names the
//! uplink the way the user sees it in Windows ("Ethernet"). This module bridges the two.

use anyhow::{Result, bail};
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
    GetIfEntry2, IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_UNICAST_ADDRESS_LH, MIB_IF_ROW2,
};
use windows::Win32::NetworkManagement::Ndis::{
    NDIS_PHYSICAL_MEDIUM, NET_LUID_LH, NdisPhysicalMedium802_3, NdisPhysicalMediumNative802_11,
    NdisPhysicalMediumWirelessLan,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC, SOCKADDR_IN, SOCKET_ADDRESS};
use windows::core::GUID;

/// The default ICS subnet Windows hands the Mobile Hotspot's own virtual adapter. Used as
/// a heuristic to identify that adapter -- there is no direct API for it -- so it is
/// never picked as its own uplink and so its IP can be reported as the hotspot's address.
/// Not a documented guarantee, but observed consistently in testing and cheap to fall
/// back away from if it ever changes.
pub const HOTSPOT_SUBNET_PREFIX: &str = "192.168.137.";

/// One network adapter as Windows describes it.
#[derive(Debug, Clone)]
pub struct Adapter {
    /// Interface GUID; matches WinRT's `NetworkAdapter::NetworkAdapterId`.
    pub guid: GUID,
    /// User-visible name, e.g. "Ethernet" or "Wi-Fi".
    pub friendly_name: String,
    /// Hardware description, e.g. "Realtek PCIe GbE Family Controller".
    pub description: String,
    /// Is this a wired Ethernet adapter (as opposed to Wi-Fi or any other interface
    /// type)? Used to prefer wired links when picking a hotspot uplink among otherwise
    /// equally-good candidates. Based on the adapter's NDIS physical medium, not its
    /// `IfType` -- several emulated-Ethernet transports (Bluetooth PAN in particular,
    /// via BNEP) report `IfType`s identical to a real NIC's, so `IfType` alone can't
    /// tell them apart.
    pub is_ethernet: bool,
    /// Is this a Wi-Fi adapter? Used to recognise the adapter the hotspot itself will
    /// broadcast on, so it's never picked as its own uplink.
    pub is_wifi: bool,
    /// IPv4 addresses currently assigned to this adapter, in dotted-decimal form.
    pub ipv4: Vec<String>,
}

impl Adapter {
    /// Does this adapter answer to `name`, as written in the config?
    pub fn matches(&self, name: &str) -> bool {
        let name = name.trim();
        self.friendly_name.eq_ignore_ascii_case(name) || self.description.eq_ignore_ascii_case(name)
    }

    /// Is this the virtual adapter Windows creates for the Mobile Hotspot's own network?
    pub fn is_hotspot_virtual_adapter(&self) -> bool {
        self.ipv4
            .iter()
            .any(|ip| ip.starts_with(HOTSPOT_SUBNET_PREFIX))
    }
}

/// IP address of the adapter with the given hardware description, if it has one.
///
/// `wlanapi`'s interface description and IP Helper's adapter description are both the
/// driver's own text, so they match directly without needing to cross-reference GUIDs.
pub fn ipv4_of<'a>(adapters: &'a [Adapter], description: &str) -> Option<&'a str> {
    adapters
        .iter()
        .find(|a| a.description.eq_ignore_ascii_case(description))
        .and_then(|a| a.ipv4.first())
        .map(String::as_str)
}

/// IP address of whichever adapter sits on the Mobile Hotspot's own ICS subnet.
pub fn hotspot_ip(adapters: &[Adapter]) -> Option<&str> {
    adapters
        .iter()
        .flat_map(|a| a.ipv4.iter())
        .find(|ip| ip.starts_with(HOTSPOT_SUBNET_PREFIX))
        .map(String::as_str)
}

/// Enough for a few dozen adapters; `GetAdaptersAddresses` says how much more it needs if
/// not, and the adapter list can change between the two calls -- hence the retries.
const INITIAL_BUFFER_BYTES: u32 = 16 * 1024;
const MAX_ATTEMPTS: usize = 4;

/// Enumerate every network adapter on the machine.
pub fn list() -> Result<Vec<Adapter>> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = INITIAL_BUFFER_BYTES;

    for _ in 0..MAX_ATTEMPTS {
        let mut buffer = vec![0u8; size as usize];
        let rc = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC.0 as u32,
                flags,
                None,
                Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut size,
            )
        };

        if rc == ERROR_SUCCESS.0 {
            // SAFETY: on success the buffer holds a linked list of adapter records.
            return Ok(unsafe { collect(buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH) });
        }
        // No adapters at all is an empty list, not an error.
        if rc == ERROR_NO_DATA.0 {
            return Ok(Vec::new());
        }
        // Anything but "your buffer was too small" is a real failure. On overflow `size`
        // now holds the required length, so the next attempt will fit.
        if rc != ERROR_BUFFER_OVERFLOW.0 {
            bail!("GetAdaptersAddresses failed with error {rc}");
        }
    }

    bail!("GetAdaptersAddresses kept reporting a too-small buffer")
}

/// Walk the `IP_ADAPTER_ADDRESSES_LH` linked list into owned [`Adapter`]s.
unsafe fn collect(mut current: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<Adapter> {
    let mut adapters = Vec::new();
    while !current.is_null() {
        let entry = unsafe { &*current };
        let medium = unsafe { physical_medium(entry.Luid) };
        adapters.push(Adapter {
            guid: unsafe { guid_from_adapter_name(entry) },
            friendly_name: unsafe { pwstr_to_string(entry.FriendlyName.0) },
            description: unsafe { pwstr_to_string(entry.Description.0) },
            is_ethernet: medium == Some(NdisPhysicalMedium802_3),
            is_wifi: medium == Some(NdisPhysicalMediumNative802_11)
                || medium == Some(NdisPhysicalMediumWirelessLan),
            ipv4: unsafe { ipv4_addresses(entry) },
        });
        current = entry.Next;
    }
    adapters
}

/// Query the NDIS physical medium of the adapter identified by `luid` -- a much more
/// reliable "what kind of link is this, really" signal than `IfType`. `None` on any
/// failure (e.g. the interface disappeared between enumeration and this call); callers
/// treat that the same as "unknown", not as a match for anything.
unsafe fn physical_medium(luid: NET_LUID_LH) -> Option<NDIS_PHYSICAL_MEDIUM> {
    let mut row = MIB_IF_ROW2 {
        InterfaceLuid: luid,
        ..Default::default()
    };
    // SAFETY: `row` is a valid, fully zeroed `MIB_IF_ROW2` with only the lookup key set,
    // exactly what `GetIfEntry2` expects to fill in.
    let rc = unsafe { GetIfEntry2(&mut row) };
    (rc == ERROR_SUCCESS).then_some(row.PhysicalMediumType)
}

/// Walk `FirstUnicastAddress` and collect every IPv4 address assigned to this adapter.
unsafe fn ipv4_addresses(entry: &IP_ADAPTER_ADDRESSES_LH) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: *const IP_ADAPTER_UNICAST_ADDRESS_LH = entry.FirstUnicastAddress;
    while !current.is_null() {
        let unicast = unsafe { &*current };
        if let Some(ip) = unsafe { sockaddr_to_ipv4(&unicast.Address) } {
            out.push(ip);
        }
        current = unicast.Next;
    }
    out
}

/// Read a `SOCKET_ADDRESS` as an IPv4 dotted-decimal string, or `None` if it is IPv6 (or
/// otherwise not an `AF_INET` address).
unsafe fn sockaddr_to_ipv4(addr: &SOCKET_ADDRESS) -> Option<String> {
    if addr.lpSockaddr.is_null() {
        return None;
    }
    // SAFETY: `lpSockaddr` points at a valid sockaddr for the lifetime of the enclosing
    // GetAdaptersAddresses buffer; checking `sa_family` before reinterpreting as
    // SOCKADDR_IN is exactly how Windows expects this union to be read.
    let family = unsafe { (*addr.lpSockaddr).sa_family };
    if family != AF_INET {
        return None;
    }
    let sin = unsafe { &*(addr.lpSockaddr as *const SOCKADDR_IN) };
    // S_addr's bytes are already in dotted-decimal order regardless of host endianness,
    // since it aliases the same memory as the S_un_b per-octet view.
    let [a, b, c, d] = unsafe { sin.sin_addr.S_un.S_addr }.to_ne_bytes();
    Some(format!("{a}.{b}.{c}.{d}"))
}

/// `AdapterName` is the interface GUID in `{8-4-4-4-12}` text form.
unsafe fn guid_from_adapter_name(entry: &IP_ADAPTER_ADDRESSES_LH) -> GUID {
    if entry.AdapterName.is_null() {
        return GUID::zeroed();
    }
    let text = unsafe { entry.AdapterName.to_string() }.unwrap_or_default();
    parse_guid(&text).unwrap_or_else(GUID::zeroed)
}

unsafe fn pwstr_to_string(ptr: *mut u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: Windows guarantees these strings are NUL-terminated.
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Parse `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` into a `GUID`. The braces IP Helper
/// writes are optional here, but `GUID::try_from` itself rejects them.
fn parse_guid(text: &str) -> Option<GUID> {
    let bare = text.trim().trim_start_matches('{').trim_end_matches('}');
    GUID::try_from(bare).ok()
}

#[cfg(test)]
impl Adapter {
    /// A bare adapter for tests: `id` is both its GUID and its friendly name, so adapters
    /// can be told apart by identity. The builders below set whatever else a test needs.
    pub(crate) fn test(id: u32) -> Self {
        Self {
            guid: GUID::from_values(id, 0, 0, [0; 8]),
            friendly_name: format!("adapter-{id}"),
            description: String::new(),
            is_ethernet: false,
            is_wifi: false,
            ipv4: Vec::new(),
        }
    }

    pub(crate) fn named(mut self, friendly_name: &str, description: &str) -> Self {
        self.friendly_name = friendly_name.to_string();
        self.description = description.to_string();
        self
    }

    pub(crate) fn ethernet(mut self) -> Self {
        self.is_ethernet = true;
        self
    }

    pub(crate) fn wifi(mut self) -> Self {
        self.is_wifi = true;
        self
    }

    pub(crate) fn with_ipv4(mut self, addresses: &[&str]) -> Self {
        self.ipv4 = addresses.iter().map(ToString::to_string).collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The adapter used throughout: a wired NIC, named the way Windows shows it.
    fn ethernet() -> Adapter {
        Adapter::test(1)
            .named("Ethernet", "Realtek PCIe GbE Family Controller")
            .ethernet()
    }

    #[test]
    fn matches_the_friendly_name_case_insensitively() {
        let a = ethernet();
        assert!(a.matches("Ethernet"));
        assert!(a.matches("ethernet"));
        assert!(a.matches("  Ethernet  "));
    }

    #[test]
    fn matches_the_hardware_description_too() {
        assert!(ethernet().matches("Realtek PCIe GbE Family Controller"));
    }

    #[test]
    fn does_not_match_a_different_adapter() {
        let a = ethernet();
        assert!(!a.matches("Wi-Fi"));
        assert!(!a.matches("Ether"));
    }

    #[test]
    fn recognises_the_hotspot_virtual_adapter_by_its_ics_subnet() {
        let a = Adapter::test(2).named("Local Area Connection* 2", "Wi-Fi Direct Virtual Adapter");
        assert!(!a.is_hotspot_virtual_adapter());
        assert!(a.with_ipv4(&["192.168.137.1"]).is_hotspot_virtual_adapter());
    }

    #[test]
    fn does_not_mistake_a_normal_adapter_for_the_hotspot_one() {
        let a = Adapter::test(2)
            .named("Wi-Fi", "Intel(R) Wireless-AC 7260")
            .with_ipv4(&["192.168.10.218"]);
        assert!(!a.is_hotspot_virtual_adapter());
    }

    #[test]
    fn ipv4_of_matches_by_hardware_description_case_insensitively() {
        let adapters = [ethernet().with_ipv4(&["10.0.0.5"])];
        assert_eq!(
            ipv4_of(&adapters, "realtek pcie gbe family controller"),
            Some("10.0.0.5")
        );
        assert_eq!(ipv4_of(&adapters, "nope"), None);
    }

    #[test]
    fn ipv4_of_is_none_when_the_adapter_has_no_address() {
        let adapters = [ethernet()];
        assert_eq!(
            ipv4_of(&adapters, "Realtek PCIe GbE Family Controller"),
            None
        );
    }

    #[test]
    fn hotspot_ip_finds_the_ics_subnet_address() {
        let adapters = [
            Adapter::test(1).with_ipv4(&["192.168.10.218"]),
            Adapter::test(2).with_ipv4(&["192.168.137.1"]),
        ];
        assert_eq!(hotspot_ip(&adapters), Some("192.168.137.1"));
    }

    #[test]
    fn hotspot_ip_is_none_without_a_matching_subnet() {
        let adapters = [Adapter::test(1).with_ipv4(&["192.168.10.218"])];
        assert_eq!(hotspot_ip(&adapters), None);
    }

    #[test]
    fn parses_a_braced_guid() {
        let g = parse_guid("{9D534A4D-FEB4-43C9-906D-6F842FB3D32D}").unwrap();
        assert_eq!(g.data1, 0x9D53_4A4D);
        assert_eq!(g.data2, 0xFEB4);
        assert_eq!(g.data3, 0x43C9);
        assert_eq!(g.data4, [0x90, 0x6D, 0x6F, 0x84, 0x2F, 0xB3, 0xD3, 0x2D]);
    }

    #[test]
    fn parses_an_unbraced_lowercase_guid() {
        assert_eq!(
            parse_guid("9d534a4d-feb4-43c9-906d-6f842fb3d32d").unwrap(),
            parse_guid("{9D534A4D-FEB4-43C9-906D-6F842FB3D32D}").unwrap()
        );
    }

    #[test]
    fn rejects_malformed_guids() {
        assert!(parse_guid("").is_none());
        assert!(parse_guid("not-a-guid").is_none());
        assert!(parse_guid("{9d534a4d-feb4-43c9-906d}").is_none());
        assert!(parse_guid("{zzzzzzzz-feb4-43c9-906d-6f842fb3d32d}").is_none());
    }
}
