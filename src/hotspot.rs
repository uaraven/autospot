//! Mobile Hotspot control via WinRT `NetworkOperatorTetheringManager`.

use std::future::IntoFuture;
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

use crate::adapters::Adapter;
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

/// Await a WinRT async operation, giving up after `OPERATION_TIMEOUT` -- a backstop so a
/// wedged driver cannot hang the watchdog loop forever.
async fn await_op<F>(op: F, what: &str) -> Result<F::Output>
where
    F: IntoFuture,
{
    tokio::time::timeout(OPERATION_TIMEOUT, async { op.await })
        .await
        .map_err(|_| anyhow!("{what} did not complete within {OPERATION_TIMEOUT:?}"))
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
    pub fn for_uplink(cfg: &HotspotConfig) -> Result<Self> {
        let (profile, profile_name) = find_uplink_profile(cfg)?;

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
    pub async fn start(&self, cfg: &HotspotConfig) -> Result<()> {
        self.configure(cfg).await?;

        let op = self
            .manager
            .StartTetheringAsync()
            .context("StartTetheringAsync")?;
        let result = await_op(op, "StartTetheringAsync").await??;

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
    pub async fn stop(&self) -> Result<()> {
        let op = self
            .manager
            .StopTetheringAsync()
            .context("StopTetheringAsync")?;
        let result = await_op(op, "StopTetheringAsync").await??;

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
    async fn configure(&self, cfg: &HotspotConfig) -> Result<()> {
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
        await_op(op, "ConfigureAccessPointAsync").await??;

        debug!(ssid = %cfg.ssid, band = ?cfg.band, "access point configured");
        Ok(())
    }
}

/// Locate the connection profile to use as the hotspot's uplink.
///
/// `cfg.uplink_adapter` is either a specific adapter -- matched against its friendly
/// name or hardware description (via IP Helper), then the network profile name as a
/// fallback, so either spelling in the config works -- or the literal
/// [`crate::config::AUTO_UPLINK`] (`"auto"`), which instead considers every currently
/// connected network. The hotspot's own virtual adapter, and whatever `cfg.hotspot_adapter`
/// resolves to (see [`resolve_hotspot_adapter`]), are always excluded: an adapter can't be
/// both its own uplink and its own hotspot -- one Wi-Fi radio can't reliably act as both a
/// client and an access point at once, which is why that combination tends to let clients
/// associate but never receive an IP address.
///
/// One adapter can carry several profiles -- remembered networks it is not currently
/// using still show up. Among the matches, a wired Ethernet adapter always wins over any
/// other adapter type outright, regardless of connectivity -- Ethernet is the
/// predictable, always-safe choice to share, and this app does not care whether the
/// uplink itself has internet access (only that connecting to it works). Connectivity
/// rank (`NetworkLimitedConnectivity` is what sharing a merely-remembered, not actually
/// connected, profile would fail with) only breaks a tie between candidates that are
/// equally Ethernet, or equally not. This applies whenever more than one profile
/// matches, whether that's because of auto-selection or because a named adapter has
/// several stored profiles.
pub fn find_uplink_profile(cfg: &HotspotConfig) -> Result<(ConnectionProfile, String)> {
    let adapter_name = cfg.uplink_adapter.as_str();
    let auto = cfg.is_auto_uplink();

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
    let hotspot_adapter = resolve_hotspot_adapter(&cfg.hotspot_adapter, &adapters);

    let mut seen: Vec<String> = Vec::new();
    let mut best: Option<((bool, u8), ConnectionProfile, String)> = None;
    let mut excluded_as_hotspot_adapter = false;

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
        let is_ethernet = matched_adapter.is_some_and(|a| a.is_ethernet);

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

        if hotspot_adapter.excludes(matched_adapter) {
            excluded_as_hotspot_adapter = true;
            continue;
        }

        let score = uplink_score(level, is_ethernet);
        if best.as_ref().is_none_or(|(best_score, _, _)| score > *best_score) {
            let label = adapter_label.unwrap_or_else(|| profile_name.clone());
            best = Some((score, profile, label));
        }
    }

    if let Some((score, profile, label)) = best {
        if score.1 < RANK_INTERNET {
            // This function is now polled every tick for the console status line, so this
            // stays at debug: a real failure to start surfaces loudly from start() itself
            // (TetheringOperationStatus::NetworkLimitedConnectivity), which is the moment
            // that actually deserves the user's attention.
            debug!(
                uplink = %label,
                "The uplink network has no internet access; the hotspot may refuse to start"
            );
        }
        return Ok((profile, label));
    }

    if excluded_as_hotspot_adapter {
        bail!(
            "the only usable network(s) found are also needed to host the hotspot itself, \
             so none of them can be used as its uplink too (one Wi-Fi radio can't reliably \
             act as both a client and an access point at once). Connect a wired Ethernet \
             uplink, add another Wi-Fi adapter, or set hotspot.hotspot_adapter and \
             hotspot.uplink_adapter explicitly in autospot.toml. Connected networks right \
             now: {}.",
            if seen.is_empty() {
                "none".to_string()
            } else {
                seen.join(", ")
            }
        );
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

/// Which adapter (if any) Windows will use to broadcast the hotspot's own Wi-Fi access
/// point, resolved from `hotspot_adapter` config so it can be excluded from uplink
/// candidacy.
pub(crate) enum HotspotAdapter<'a> {
    /// Confidently resolved, possibly to "no such adapter is currently present".
    Known(Option<&'a Adapter>),
    /// `hotspot_adapter = "auto"` and more than one Wi-Fi adapter is present, so which
    /// one Windows would actually use can't be determined. Every Wi-Fi adapter is
    /// treated as a potential hotspot adapter (and excluded) until this is configured
    /// explicitly.
    Ambiguous,
}

impl HotspotAdapter<'_> {
    /// Should `candidate` be excluded from uplink candidacy because it's the (or a
    /// possible) hotspot adapter?
    pub(crate) fn excludes(&self, candidate: Option<&Adapter>) -> bool {
        match self {
            HotspotAdapter::Known(Some(hotspot)) => {
                candidate.is_some_and(|a| a.guid == hotspot.guid)
            }
            HotspotAdapter::Known(None) => false,
            HotspotAdapter::Ambiguous => candidate.is_some_and(|a| a.is_wifi),
        }
    }

    /// A short human-readable description, for diagnostics output.
    pub(crate) fn describe(&self) -> String {
        match self {
            HotspotAdapter::Known(Some(a)) => a.friendly_name.clone(),
            HotspotAdapter::Known(None) => "none detected".to_string(),
            HotspotAdapter::Ambiguous => "multiple Wi-Fi adapters present".to_string(),
        }
    }
}

/// Resolve `hotspot_adapter` (see its doc comment in [`HotspotConfig`]) against the
/// currently present adapters.
pub(crate) fn resolve_hotspot_adapter<'a>(
    hotspot_adapter: &str,
    adapters: &'a [Adapter],
) -> HotspotAdapter<'a> {
    if !crate::config::is_auto(hotspot_adapter) {
        return HotspotAdapter::Known(adapters.iter().find(|a| a.matches(hotspot_adapter)));
    }

    let mut wifi_adapters = adapters.iter().filter(|a| a.is_wifi);
    let Some(first) = wifi_adapters.next() else {
        return HotspotAdapter::Known(None);
    };
    if wifi_adapters.next().is_some() {
        HotspotAdapter::Ambiguous
    } else {
        HotspotAdapter::Known(Some(first))
    }
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

/// Prefer a wired Ethernet adapter over any other adapter type outright -- a wired link
/// is the predictable, always-safe choice to share, unlike Wi-Fi (which can't reliably
/// also host the hotspot) or anything else. Connectivity rank only breaks a tie between
/// candidates that are equally Ethernet, or equally not. Tuple comparison is
/// lexicographic and `false < true`, so `is_ethernet` dominates and connectivity rank
/// only decides ties within the same Ethernet-ness.
fn uplink_score(level: Option<NetworkConnectivityLevel>, is_ethernet: bool) -> (bool, u8) {
    (is_ethernet, rank_connectivity(level))
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
    use windows::core::GUID;

    fn adapter(guid: u32, is_ethernet: bool, is_wifi: bool) -> Adapter {
        Adapter {
            guid: GUID::from_values(guid, 0, 0, [0; 8]),
            friendly_name: format!("adapter-{guid}"),
            description: String::new(),
            is_ethernet,
            is_wifi,
            ipv4: Vec::new(),
        }
    }

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
    fn ethernet_always_outranks_a_non_ethernet_adapter() {
        // Even a barely-connected Ethernet adapter beats a fully-connected non-Ethernet
        // one -- this app doesn't care about the uplink's own internet access, and a
        // wired link is always the safe, predictable choice over anything else.
        assert!(
            uplink_score(Some(NetworkConnectivityLevel::None), true)
                > uplink_score(Some(NetworkConnectivityLevel::InternetAccess), false)
        );
    }

    #[test]
    fn connectivity_rank_breaks_a_tie_within_the_same_ethernet_ness() {
        assert!(
            uplink_score(Some(NetworkConnectivityLevel::InternetAccess), true)
                > uplink_score(Some(NetworkConnectivityLevel::LocalAccess), true)
        );
        assert!(
            uplink_score(Some(NetworkConnectivityLevel::InternetAccess), false)
                > uplink_score(Some(NetworkConnectivityLevel::LocalAccess), false)
        );
    }

    #[test]
    fn resolves_hotspot_adapter_automatically_with_exactly_one_wifi_adapter() {
        let ethernet = adapter(1, true, false);
        let wifi = adapter(2, false, true);
        let adapters = [ethernet, wifi.clone()];
        let resolved = resolve_hotspot_adapter("auto", &adapters);
        assert!(matches!(resolved, HotspotAdapter::Known(Some(a)) if a.guid == wifi.guid));
    }

    #[test]
    fn auto_resolution_is_none_without_any_wifi_adapter() {
        let adapters = [adapter(1, true, false)];
        let resolved = resolve_hotspot_adapter("auto", &adapters);
        assert!(matches!(resolved, HotspotAdapter::Known(None)));
    }

    #[test]
    fn auto_resolution_is_ambiguous_with_two_wifi_adapters() {
        let adapters = [adapter(1, false, true), adapter(2, false, true)];
        let resolved = resolve_hotspot_adapter("auto", &adapters);
        assert!(matches!(resolved, HotspotAdapter::Ambiguous));
    }

    #[test]
    fn a_named_hotspot_adapter_is_matched_by_friendly_name() {
        let wifi = adapter(2, false, true);
        let adapters = [adapter(1, true, false), wifi.clone()];
        let resolved = resolve_hotspot_adapter(&wifi.friendly_name, &adapters);
        assert!(matches!(resolved, HotspotAdapter::Known(Some(a)) if a.guid == wifi.guid));
    }

    #[test]
    fn a_named_hotspot_adapter_not_currently_present_resolves_to_none() {
        let adapters = [adapter(1, true, false)];
        let resolved = resolve_hotspot_adapter("some other adapter", &adapters);
        assert!(matches!(resolved, HotspotAdapter::Known(None)));
    }

    #[test]
    fn known_hotspot_adapter_excludes_only_itself() {
        let hotspot = adapter(1, false, true);
        let other = adapter(2, true, false);
        let resolution = HotspotAdapter::Known(Some(&hotspot));
        assert!(resolution.excludes(Some(&hotspot)));
        assert!(!resolution.excludes(Some(&other)));
        assert!(!resolution.excludes(None));
    }

    #[test]
    fn known_none_excludes_nothing() {
        let other = adapter(2, true, false);
        let resolution: HotspotAdapter = HotspotAdapter::Known(None);
        assert!(!resolution.excludes(Some(&other)));
        assert!(!resolution.excludes(None));
    }

    #[test]
    fn ambiguous_excludes_every_wifi_adapter_but_not_others() {
        let wifi = adapter(1, false, true);
        let ethernet = adapter(2, true, false);
        let resolution = HotspotAdapter::Ambiguous;
        assert!(resolution.excludes(Some(&wifi)));
        assert!(!resolution.excludes(Some(&ethernet)));
        assert!(!resolution.excludes(None));
    }

    #[test]
    fn states_render_for_logs() {
        assert_eq!(State::On.to_string(), "on");
        assert_eq!(State::InTransition.to_string(), "in-transition");
    }
}
