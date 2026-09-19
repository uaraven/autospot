//! Mobile Hotspot control via WinRT `NetworkOperatorTetheringManager`.
//!
//! Which network the hotspot shares is `uplink`'s decision; this module only drives the
//! tethering manager once that profile has been chosen.

use std::future::IntoFuture;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use tracing::{debug, info, warn};
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringAccessPointConfiguration, NetworkOperatorTetheringManager,
    NetworkOperatorTetheringOperationResult, TetheringCapability, TetheringOperationStatus,
    TetheringOperationalState, TetheringWiFiBand,
};
use windows::core::HSTRING;

use crate::config::{Band, HotspotConfig};
use crate::uplink;

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
        f.write_str(match self {
            State::On => "on",
            State::Off => "off",
            State::InTransition => "in-transition",
            State::Unknown => "unknown",
        })
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
    /// What the uplink the manager was created from is called, for logging.
    pub uplink_profile: String,
}

impl Hotspot {
    /// Build a manager sharing the connection of the adapter named in the config.
    ///
    /// The manager is deliberately not cached across polls: the uplink profile
    /// disappears when its adapter is unplugged, and a stale manager would keep failing.
    pub fn for_uplink(cfg: &HotspotConfig) -> Result<Self> {
        let (profile, uplink_profile) = uplink::find_profile(cfg)?;

        match NetworkOperatorTetheringManager::GetTetheringCapabilityFromConnectionProfile(&profile)
        {
            Ok(TetheringCapability::Enabled) => {}
            Ok(other) => bail!(
                "Windows reports tethering is not available on '{uplink_profile}': {}",
                describe_capability(other)
            ),
            // Not fatal on its own: the capability probe is advisory, so log and push on.
            Err(e) => debug!("tethering capability probe failed: {e}"),
        }

        let manager = NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile)
            .with_context(|| {
                format!("creating a tethering manager for uplink '{uplink_profile}'")
            })?;

        Ok(Self {
            manager,
            uplink_profile,
        })
    }

    /// Current operational state of the hotspot.
    pub fn state(&self) -> Result<State> {
        let state = self
            .manager
            .TetheringOperationalState()
            .context("reading tethering state")?;
        Ok(State::from_winrt(state))
    }

    /// Number of devices currently connected to the hotspot.
    pub fn client_count(&self) -> Result<u32> {
        self.manager.ClientCount().context("reading client count")
    }

    /// SSID the hotspot is currently configured with.
    pub fn current_ssid(&self) -> Result<String> {
        let ssid = self.access_point()?.Ssid().context("reading SSID")?;
        Ok(ssid.to_string())
    }

    /// Apply the configured SSID/passphrase/band and switch the hotspot on.
    pub async fn start(&self, cfg: &HotspotConfig) -> Result<()> {
        self.configure(cfg).await?;

        let result = await_op("StartTetheringAsync", || self.manager.StartTetheringAsync()).await?;

        match result.Status()? {
            TetheringOperationStatus::Success => {
                info!(ssid = %cfg.ssid, uplink = %self.uplink_profile, "hotspot started");
                Ok(())
            }
            TetheringOperationStatus::AlreadyOn => {
                info!("hotspot was already on");
                Ok(())
            }
            status => Err(failure("starting", status, &result)),
        }
    }

    /// Switch the hotspot off.
    pub async fn stop(&self) -> Result<()> {
        let result = await_op("StopTetheringAsync", || self.manager.StopTetheringAsync()).await?;

        match result.Status()? {
            TetheringOperationStatus::Success => {
                info!("hotspot stopped");
                Ok(())
            }
            status => Err(failure("stopping", status, &result)),
        }
    }

    /// Push SSID, passphrase and band into the access point configuration.
    async fn configure(&self, cfg: &HotspotConfig) -> Result<()> {
        let ap = self.access_point()?;

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

        await_op("ConfigureAccessPointAsync", || {
            self.manager.ConfigureAccessPointAsync(&ap)
        })
        .await?;

        debug!(ssid = %cfg.ssid, band = ?cfg.band, "access point configured");
        Ok(())
    }

    fn access_point(&self) -> Result<NetworkOperatorTetheringAccessPointConfiguration> {
        self.manager
            .GetCurrentAccessPointConfiguration()
            .context("reading access point configuration")
    }
}

/// Start a WinRT async tethering call and await its result. `what` names the call in any
/// error; [`OPERATION_TIMEOUT`] is the backstop against a driver that never answers.
async fn await_op<Op, T>(what: &str, start: impl FnOnce() -> windows::core::Result<Op>) -> Result<T>
where
    Op: IntoFuture<Output = windows::core::Result<T>>,
{
    let op = start().with_context(|| what.to_string())?;

    tokio::time::timeout(OPERATION_TIMEOUT, op)
        .await
        .map_err(|_| anyhow!("{what} did not complete within {OPERATION_TIMEOUT:?}"))?
        .with_context(|| what.to_string())
}

/// Turn an unsuccessful tethering operation into an error explaining what Windows said.
fn failure(
    action: &str,
    status: TetheringOperationStatus,
    result: &NetworkOperatorTetheringOperationResult,
) -> anyhow::Error {
    let detail = match result.AdditionalErrorMessage() {
        Ok(msg) if !msg.is_empty() => format!(" ({msg})"),
        _ => String::new(),
    };
    anyhow!(
        "{action} the hotspot failed: {}{detail}",
        describe_status(status)
    )
}

fn describe_status(status: TetheringOperationStatus) -> &'static str {
    match status {
        TetheringOperationStatus::Success => "success",
        TetheringOperationStatus::Unknown => "unknown error",
        TetheringOperationStatus::MobileBroadbandDeviceOff => "the mobile broadband device is off",
        TetheringOperationStatus::WiFiDeviceOff => "the Wi-Fi device is off",
        TetheringOperationStatus::EntitlementCheckTimeout => "the entitlement check timed out",
        TetheringOperationStatus::EntitlementCheckFailure => "the entitlement check failed",
        TetheringOperationStatus::OperationInProgress => {
            "another tethering operation is in progress"
        }
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
        assert_eq!(
            State::from_winrt(TetheringOperationalState::Off),
            State::Off
        );
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
        assert_eq!(
            Band::SixGigahertz.to_winrt(),
            TetheringWiFiBand::SixGigahertz
        );
    }

    #[test]
    fn states_render_for_logs() {
        assert_eq!(State::On.to_string(), "on");
        assert_eq!(State::InTransition.to_string(), "in-transition");
    }
}
