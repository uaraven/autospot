//! Mobile Hotspot control via WinRT `NetworkOperatorTetheringManager`.

use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use tracing::{debug, info, warn};
use windows::core::HSTRING;
use windows::Networking::Connectivity::{
    ConnectionProfile, NetworkConnectivityLevel, NetworkInformation,
};
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringManager, TetheringCapability, TetheringOperationStatus,
    TetheringOperationalState, TetheringWiFiBand,
};

use crate::blocking::block_on;
use crate::config::{Band, HotspotConfig};

/// Tethering operations talk to the Wi-Fi driver and can take a few seconds; this is a
/// backstop so a wedged driver cannot hang the watchdog loop forever.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(45);

/// High-level hotspot state, decoupled from the WinRT enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    On,
    Off,
    InTransition,
    Unknown,
}

impl State {
    fn from_winrt(state: TetheringOperationalState) -> Self {
        match state {
            TetheringOperationalState::On => State::On,
            TetheringOperationalState::Off => State::Off,
            TetheringOperationalState::InTransition => State::InTransition,
            _ => State::Unknown,
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            State::On => "on",
            State::Off => "off",
            State::InTransition => "in-transition",
            State::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

impl Band {
    fn to_winrt(self) -> TetheringWiFiBand {
        match self {
            Band::Auto => TetheringWiFiBand::Auto,
            Band::TwoPointFourGigahertz => TetheringWiFiBand::TwoPointFourGigahertz,
            Band::FiveGigahertz => TetheringWiFiBand::FiveGigahertz,
            Band::SixGigahertz => TetheringWiFiBand::SixGigahertz,
        }
    }
}

/// A tethering manager bound to one uplink connection profile.
pub struct Hotspot {
    manager: NetworkOperatorTetheringManager,
    /// Profile name the manager was created from, for logging.
    pub uplink_profile: String,
}

impl Hotspot {
    /// Build a manager sharing the connection of the adapter named in the config.
    ///
    /// The manager is deliberately not cached across polls: the uplink profile
    /// disappears when its adapter is unplugged, and a stale manager would keep failing.
    pub fn for_uplink(adapter_name: &str) -> Result<Self> {
        let (profile, profile_name) = find_uplink_profile(adapter_name)?;

        match NetworkOperatorTetheringManager::GetTetheringCapabilityFromConnectionProfile(&profile)
        {
            Ok(TetheringCapability::Enabled) => {}
            Ok(other) => bail!(
                "Windows reports tethering is not available on '{profile_name}': {}",
                describe_capability(other)
            ),
            // Not fatal on its own: the capability probe is advisory, so log and push on.
            Err(e) => debug!("tethering capability probe failed: {e}"),
        }

        let manager = NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile)
            .with_context(|| {
                format!("creating a tethering manager for uplink '{profile_name}'")
            })?;

        Ok(Self {
            manager,
            uplink_profile: profile_name,
        })
    }

    /// Current operational state of the hotspot.
    pub fn state(&self) -> Result<State> {
        Ok(State::from_winrt(
            self.manager
                .TetheringOperationalState()
                .context("reading tethering state")?,
        ))
    }

    /// Number of devices currently connected to the hotspot.
    pub fn client_count(&self) -> Result<u32> {
        self.manager.ClientCount().context("reading client count")
    }

    /// SSID the hotspot is currently configured with.
    pub fn current_ssid(&self) -> Result<String> {
        let cfg = self
            .manager
            .GetCurrentAccessPointConfiguration()
            .context("reading access point configuration")?;
        Ok(cfg.Ssid().context("reading SSID")?.to_string())
    }

    /// Apply the configured SSID/passphrase/band and switch the hotspot on.
    pub fn start(&self, cfg: &HotspotConfig) -> Result<()> {
        self.configure(cfg)?;

        let op = self
            .manager
            .StartTetheringAsync()
            .context("StartTetheringAsync")?;
        let result = block_on(op, OPERATION_TIMEOUT, "StartTetheringAsync")??;

        match result.Status()? {
            TetheringOperationStatus::Success => {
                info!(ssid = %cfg.ssid, uplink = %self.uplink_profile, "hotspot started");
                Ok(())
            }
            TetheringOperationStatus::AlreadyOn => {
                info!("hotspot was already on");
                Ok(())
            }
            status => Err(anyhow!(
                "starting the hotspot failed: {}{}",
                describe_status(status),
                additional_message(&result)
            )),
        }
    }

    /// Switch the hotspot off.
    pub fn stop(&self) -> Result<()> {
        let op = self
            .manager
            .StopTetheringAsync()
            .context("StopTetheringAsync")?;
        let result = block_on(op, OPERATION_TIMEOUT, "StopTetheringAsync")??;

        match result.Status()? {
            TetheringOperationStatus::Success => {
                info!("hotspot stopped");
                Ok(())
            }
            status => Err(anyhow!(
                "stopping the hotspot failed: {}{}",
                describe_status(status),
                additional_message(&result)
            )),
        }
    }

    /// Push SSID, passphrase and band into the access point configuration.
    fn configure(&self, cfg: &HotspotConfig) -> Result<()> {
        let ap = self
            .manager
            .GetCurrentAccessPointConfiguration()
            .context("reading access point configuration")?;

        ap.SetSsid(&HSTRING::from(&cfg.ssid))
            .context("setting hotspot SSID")?;
        ap.SetPassphrase(&HSTRING::from(&cfg.passphrase))
            .context("setting hotspot passphrase")?;

        // Band control arrived later than the rest of this API and is not implemented by
        // every Wi-Fi driver -- this machine's returns E_FAIL from IsBandSupported. A
        // band we cannot set is worth a warning, not a failed hotspot.
        if let Err(e) = ap.SetBand(cfg.band.to_winrt()) {
            warn!(band = ?cfg.band, "could not set the hotspot band, leaving the driver default: {e}");
        }

        let op = self
            .manager
            .ConfigureAccessPointAsync(&ap)
            .context("ConfigureAccessPointAsync")?;
        block_on(op, OPERATION_TIMEOUT, "ConfigureAccessPointAsync")??;

        debug!(ssid = %cfg.ssid, band = ?cfg.band, "access point configured");
        Ok(())
    }
}

/// Locate the connection profile to use as the hotspot's uplink.
///
/// `adapter_name` is either a specific adapter -- matched against its friendly name or
/// hardware description (via IP Helper), then the network profile name as a fallback, so
/// either spelling in the config works -- or the literal [`crate::config::AUTO_UPLINK`]
/// (`"auto"`), which instead considers every currently connected network. The hotspot's
/// own virtual adapter is always excluded from auto-selection so it can never end up
/// sharing itself.
///
/// One adapter can carry several profiles -- remembered networks it is not currently
/// using still show up. Sharing one of those would fail with
/// `NetworkLimitedConnectivity`, so among the matches the one with the best connectivity
/// wins.
pub fn find_uplink_profile(adapter_name: &str) -> Result<(ConnectionProfile, String)> {
    let auto = adapter_name.trim().eq_ignore_ascii_case(crate::config::AUTO_UPLINK);

    let profiles = NetworkInformation::GetConnectionProfiles()
        .context("enumerating network connection profiles")?;

    let adapters = crate::adapters::list().unwrap_or_else(|e| {
        debug!("adapter enumeration failed, falling back to profile names: {e}");
        Vec::new()
    });
    let wanted: Vec<_> = if auto {
        Vec::new()
    } else {
        adapters.iter().filter(|a| a.matches(adapter_name)).collect()
    };

    let mut seen: Vec<String> = Vec::new();
    let mut best: Option<(u8, ConnectionProfile, String)> = None;

    for profile in &profiles {
        let profile_name = profile
            .ProfileName()
            .map(|s| s.to_string())
            .unwrap_or_default();

        let adapter_guid = profile
            .NetworkAdapter()
            .and_then(|a| a.NetworkAdapterId())
            .ok();

        let matched_adapter = adapter_guid.and_then(|guid| adapters.iter().find(|a| a.guid == guid));

        let adapter_label = matched_adapter.map(|a| a.friendly_name.clone());

        let level = profile.GetNetworkConnectivityLevel().ok();

        seen.push(format!(
            "'{profile_name}' (on {}, {})",
            adapter_label.as_deref().unwrap_or("unknown adapter"),
            describe_connectivity(level)
        ));

        let matched = if auto {
            // Every adapter we can identify is a candidate except the hotspot's own --
            // sharing that would try to make the hotspot its own uplink.
            matched_adapter.is_some_and(|a| !a.is_hotspot_virtual_adapter())
        } else {
            (match adapter_guid {
                Some(guid) => wanted.iter().any(|a| a.guid == guid),
                None => false,
            }) || profile_name.eq_ignore_ascii_case(adapter_name.trim())
        };

        if !matched {
            continue;
        }

        let rank = rank_connectivity(level);
        if best.as_ref().is_none_or(|(best_rank, _, _)| rank > *best_rank) {
            let label = adapter_label.unwrap_or_else(|| profile_name.clone());
            best = Some((rank, profile, label));
        }
    }

    if let Some((rank, profile, label)) = best {
        if rank < RANK_INTERNET {
            // This function is now polled every tick for the console status line, so this
            // stays at debug: a real failure to start surfaces loudly from start() itself
            // (TetheringOperationStatus::NetworkLimitedConnectivity), which is the moment
            // that actually deserves the user's attention.
            debug!(
                uplink = %label,
                "the uplink network has no internet access; the hotspot may refuse to start"
            );
        }
        return Ok((profile, label));
    }

    if auto {
        bail!(
            "auto uplink selection found no usable connected network to share. \
             Connected networks right now: {}.",
            if seen.is_empty() {
                "none".to_string()
            } else {
                seen.join(", ")
            }
        );
    }

    bail!(
        "no connected network found for uplink adapter '{adapter_name}'. \
         Connected networks right now: {}. \
         Note that an adapter only has a connection profile while it is plugged in and up.",
        if seen.is_empty() {
            "none".to_string()
        } else {
            seen.join(", ")
        }
    )
}

const RANK_INTERNET: u8 = 3;

/// Rank profiles so the one actually carrying internet is preferred as the uplink.
fn rank_connectivity(level: Option<NetworkConnectivityLevel>) -> u8 {
    match level {
        Some(NetworkConnectivityLevel::InternetAccess) => RANK_INTERNET,
        Some(NetworkConnectivityLevel::ConstrainedInternetAccess) => 2,
        Some(NetworkConnectivityLevel::LocalAccess) => 1,
        _ => 0,
    }
}

fn describe_connectivity(level: Option<NetworkConnectivityLevel>) -> &'static str {
    match level {
        Some(NetworkConnectivityLevel::InternetAccess) => "internet access",
        Some(NetworkConnectivityLevel::ConstrainedInternetAccess) => "constrained internet",
        Some(NetworkConnectivityLevel::LocalAccess) => "local access only",
        Some(NetworkConnectivityLevel::None) => "no connectivity",
        _ => "connectivity unknown",
    }
}

fn additional_message(result: &windows::Networking::NetworkOperators::NetworkOperatorTetheringOperationResult) -> String {
    match result.AdditionalErrorMessage() {
        Ok(msg) if !msg.is_empty() => format!(" ({msg})"),
        _ => String::new(),
    }
}

fn describe_status(status: TetheringOperationStatus) -> &'static str {
    match status {
        TetheringOperationStatus::Success => "success",
        TetheringOperationStatus::Unknown => "unknown error",
        TetheringOperationStatus::MobileBroadbandDeviceOff => "the mobile broadband device is off",
        TetheringOperationStatus::WiFiDeviceOff => "the Wi-Fi device is off",
        TetheringOperationStatus::EntitlementCheckTimeout => "the entitlement check timed out",
        TetheringOperationStatus::EntitlementCheckFailure => "the entitlement check failed",
        TetheringOperationStatus::OperationInProgress => "another tethering operation is in progress",
        TetheringOperationStatus::BluetoothDeviceOff => "the Bluetooth device is off",
        TetheringOperationStatus::NetworkLimitedConnectivity => {
            "the uplink network has limited connectivity"
        }
        TetheringOperationStatus::AlreadyOn => "the hotspot is already on",
        TetheringOperationStatus::RadioRestriction => "a radio restriction is in effect",
        TetheringOperationStatus::BandInterference => "band interference was detected",
        _ => "unrecognised status",
    }
}

fn describe_capability(capability: TetheringCapability) -> &'static str {
    match capability {
        TetheringCapability::Enabled => "enabled",
        TetheringCapability::DisabledByGroupPolicy => "disabled by group policy",
        TetheringCapability::DisabledByHardwareLimitation => "disabled by a hardware limitation",
        TetheringCapability::DisabledByOperator => "disabled by the mobile operator",
        TetheringCapability::DisabledBySku => "disabled by this Windows edition",
        TetheringCapability::DisabledByRequiredAppNotInstalled => {
            "disabled because a required app is not installed"
        }
        TetheringCapability::DisabledDueToUnknownCause => "disabled for an unknown reason",
        TetheringCapability::DisabledBySystemCapability => "disabled by a system capability",
        _ => "unrecognised capability",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winrt_states_map_onto_our_state_enum() {
        assert_eq!(State::from_winrt(TetheringOperationalState::On), State::On);
        assert_eq!(State::from_winrt(TetheringOperationalState::Off), State::Off);
        assert_eq!(
            State::from_winrt(TetheringOperationalState::InTransition),
            State::InTransition
        );
        assert_eq!(
            State::from_winrt(TetheringOperationalState::Unknown),
            State::Unknown
        );
    }

    #[test]
    fn bands_map_onto_the_winrt_enum() {
        assert_eq!(Band::Auto.to_winrt(), TetheringWiFiBand::Auto);
        assert_eq!(
            Band::TwoPointFourGigahertz.to_winrt(),
            TetheringWiFiBand::TwoPointFourGigahertz
        );
        assert_eq!(
            Band::FiveGigahertz.to_winrt(),
            TetheringWiFiBand::FiveGigahertz
        );
        assert_eq!(Band::SixGigahertz.to_winrt(), TetheringWiFiBand::SixGigahertz);
    }

    #[test]
    fn a_profile_with_internet_outranks_one_without() {
        assert!(
            rank_connectivity(Some(NetworkConnectivityLevel::InternetAccess))
                > rank_connectivity(Some(NetworkConnectivityLevel::ConstrainedInternetAccess))
        );
        assert!(
            rank_connectivity(Some(NetworkConnectivityLevel::ConstrainedInternetAccess))
                > rank_connectivity(Some(NetworkConnectivityLevel::LocalAccess))
        );
        assert!(
            rank_connectivity(Some(NetworkConnectivityLevel::LocalAccess))
                > rank_connectivity(Some(NetworkConnectivityLevel::None))
        );
        assert_eq!(rank_connectivity(None), 0);
        assert_eq!(
            rank_connectivity(Some(NetworkConnectivityLevel::InternetAccess)),
            RANK_INTERNET
        );
    }

    #[test]
    fn states_render_for_logs() {
        assert_eq!(State::On.to_string(), "on");
        assert_eq!(State::InTransition.to_string(), "in-transition");
    }
}
