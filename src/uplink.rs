//! Choosing which network connection Windows should share as the hotspot's uplink.
//!
//! Windows exposes one `ConnectionProfile` per network it knows about, and tethering is
//! configured against exactly one of them. This module picks that one from the config and
//! from what is actually connected right now -- see [`find_profile`] for the rules.

use anyhow::{Context as _, Result, bail};
use tracing::debug;
use windows::Networking::Connectivity::{
    ConnectionProfile, NetworkConnectivityLevel, NetworkInformation,
};

use crate::adapters::{self, Adapter};
use crate::config::{self, HotspotConfig};

/// Locate the connection profile to use as the hotspot's uplink, and a label for it.
///
/// `cfg.uplink_adapter` is either a specific adapter -- matched against its friendly
/// name or hardware description (via IP Helper), then the network profile name as a
/// fallback, so either spelling in the config works -- or the literal
/// [`crate::config::AUTO_ADAPTER`] (`"auto"`), which instead considers every currently
/// connected network. The hotspot's own virtual adapter, and whatever `cfg.hotspot_adapter`
/// resolves to (see [`resolve_hotspot_adapter`]), are always excluded: an adapter can't be
/// both its own uplink and its own hotspot -- one Wi-Fi radio can't reliably act as both a
/// client and an access point at once, which is why that combination tends to let clients
/// associate but never receive an IP address.
///
/// One adapter can carry several profiles -- remembered networks it is not currently
/// using still show up -- so among the matches the best [`uplink_score`] wins: a wired
/// Ethernet adapter outright, with connectivity only breaking ties.
pub fn find_profile(cfg: &HotspotConfig) -> Result<(ConnectionProfile, String)> {
    let adapters = adapters::list().unwrap_or_else(|e| {
        debug!("adapter enumeration failed, falling back to profile names: {e}");
        Vec::new()
    });
    let candidates = candidates(&adapters)?;

    let hotspot_adapter = resolve_hotspot_adapter(&cfg.hotspot_adapter, &adapters);
    let auto = cfg.is_auto_uplink();

    let mut best: Option<&Candidate> = None;
    let mut excluded_as_hotspot_adapter = false;

    for candidate in &candidates {
        let wanted = if auto {
            candidate.is_auto_candidate()
        } else {
            candidate.answers_to(&cfg.uplink_adapter)
        };
        if !wanted {
            continue;
        }

        if hotspot_adapter.excludes(candidate.adapter) {
            excluded_as_hotspot_adapter = true;
            continue;
        }

        if best.is_none_or(|best| candidate.score() > best.score()) {
            best = Some(candidate);
        }
    }

    if let Some(best) = best {
        if Connectivity::of(best.connectivity) < Connectivity::Internet {
            // This runs every tick for the console status line, so it stays at debug: a
            // real failure to start surfaces loudly from `Hotspot::start` itself
            // (`TetheringOperationStatus::NetworkLimitedConnectivity`), which is the
            // moment that actually deserves the user's attention.
            debug!(
                uplink = %best.label(),
                "The uplink network has no internet access; the hotspot may refuse to start"
            );
        }
        return Ok((best.profile.clone(), best.label()));
    }

    // Nothing usable: say what was on offer, since that is what the user has to change.
    let networks = if candidates.is_empty() {
        "none".to_string()
    } else {
        candidates
            .iter()
            .map(Candidate::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };

    if excluded_as_hotspot_adapter {
        bail!(
            "the only usable network(s) found are also needed to host the hotspot itself, \
             so none of them can be used as its uplink too (one Wi-Fi radio can't reliably \
             act as both a client and an access point at once). Connect a wired Ethernet \
             uplink, add another Wi-Fi adapter, or set hotspot.hotspot_adapter and \
             hotspot.uplink_adapter explicitly in {config}. Connected networks right \
             now: {networks}.",
            config = config::DEFAULT_FILE_NAME
        );
    }

    if auto {
        bail!(
            "auto uplink selection found no usable connected network to share. \
             Connected networks right now: {networks}."
        );
    }

    bail!(
        "no connected network found for uplink adapter '{}'. \
         Connected networks right now: {networks}. \
         Note that an adapter only has a connection profile while it is plugged in and up.",
        cfg.uplink_adapter
    )
}

/// Every connection profile Windows currently knows about, paired with its adapter.
fn candidates<'a>(adapters: &'a [Adapter]) -> Result<Vec<Candidate<'a>>> {
    let profiles = NetworkInformation::GetConnectionProfiles()
        .context("enumerating network connection profiles")?;

    Ok(profiles
        .into_iter()
        .map(|profile| Candidate::new(profile, adapters))
        .collect())
}

/// One connection profile, plus whatever IP Helper could tell us about the adapter
/// carrying it: everything the choice above needs to know about it.
struct Candidate<'a> {
    profile: ConnectionProfile,
    /// The network's name, e.g. "my-ssid" -- not the adapter's.
    network_name: String,
    /// The adapter carrying this profile, when it could be identified at all.
    adapter: Option<&'a Adapter>,
    connectivity: Option<NetworkConnectivityLevel>,
}

impl<'a> Candidate<'a> {
    fn new(profile: ConnectionProfile, adapters: &'a [Adapter]) -> Self {
        let network_name = profile
            .ProfileName()
            .map(|name| name.to_string())
            .unwrap_or_default();
        let adapter = profile
            .NetworkAdapter()
            .and_then(|adapter| adapter.NetworkAdapterId())
            .ok()
            .and_then(|guid| adapters.iter().find(|a| a.guid == guid));

        Self {
            connectivity: profile.GetNetworkConnectivityLevel().ok(),
            profile,
            network_name,
            adapter,
        }
    }

    /// Does this profile answer to `uplink_adapter` as spelled in the config? Its adapter
    /// may, or -- when the adapter could not be identified -- the network's own name may.
    fn answers_to(&self, uplink_adapter: &str) -> bool {
        self.adapter.is_some_and(|a| a.matches(uplink_adapter))
            || self
                .network_name
                .eq_ignore_ascii_case(uplink_adapter.trim())
    }

    /// Is this profile eligible for automatic selection? Every adapter we can identify is,
    /// except the hotspot's own virtual one -- sharing that would make the hotspot its own
    /// uplink.
    fn is_auto_candidate(&self) -> bool {
        self.adapter
            .is_some_and(|a| !a.is_hotspot_virtual_adapter())
    }

    fn score(&self) -> (bool, Connectivity) {
        let is_ethernet = self.adapter.is_some_and(|a| a.is_ethernet);
        uplink_score(self.connectivity, is_ethernet)
    }

    /// What to call this uplink in logs and errors: the adapter's name as Windows shows
    /// it, falling back to the network's name when the adapter is unknown.
    fn label(&self) -> String {
        match self.adapter {
            Some(adapter) => adapter.friendly_name.clone(),
            None => self.network_name.clone(),
        }
    }
}

/// How a candidate is listed back to the user, e.g. `'my-ssid' (on Wi-Fi, internet access)`.
impl std::fmt::Display for Candidate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "'{}' (on {}, {})",
            self.network_name,
            self.adapter
                .map_or("unknown adapter", |a| a.friendly_name.as_str()),
            describe_connectivity(self.connectivity)
        )
    }
}

/// How useful a network's connectivity level makes it as an uplink. Derived `Ord` ranks
/// these worst to best in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Connectivity {
    /// Unknown, or known to carry nothing -- which is what a merely-remembered profile,
    /// not actually connected, looks like.
    None,
    LocalOnly,
    ConstrainedInternet,
    Internet,
}

impl Connectivity {
    fn of(level: Option<NetworkConnectivityLevel>) -> Self {
        match level {
            Some(NetworkConnectivityLevel::InternetAccess) => Connectivity::Internet,
            Some(NetworkConnectivityLevel::ConstrainedInternetAccess) => {
                Connectivity::ConstrainedInternet
            }
            Some(NetworkConnectivityLevel::LocalAccess) => Connectivity::LocalOnly,
            _ => Connectivity::None,
        }
    }
}

/// Prefer a wired Ethernet adapter over any other adapter type outright -- a wired link
/// is the predictable, always-safe choice to share, unlike Wi-Fi (which can't reliably
/// also host the hotspot) or anything else. This app does not care whether the uplink
/// itself has internet access, only that connecting to it works, so connectivity merely
/// breaks a tie between candidates that are equally Ethernet, or equally not. Tuple
/// comparison is lexicographic and `false < true`, so `is_ethernet` dominates.
fn uplink_score(
    level: Option<NetworkConnectivityLevel>,
    is_ethernet: bool,
) -> (bool, Connectivity) {
    (is_ethernet, Connectivity::of(level))
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

/// Which adapter (if any) Windows will use to broadcast the hotspot's own Wi-Fi access
/// point, resolved from `hotspot_adapter` config so it can be excluded from uplink
/// candidacy.
pub enum HotspotAdapter<'a> {
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
    pub fn excludes(&self, candidate: Option<&Adapter>) -> bool {
        match self {
            HotspotAdapter::Known(Some(hotspot)) => {
                candidate.is_some_and(|a| a.guid == hotspot.guid)
            }
            HotspotAdapter::Known(None) => false,
            HotspotAdapter::Ambiguous => candidate.is_some_and(|a| a.is_wifi),
        }
    }

    /// A short human-readable description, for diagnostics output.
    pub fn describe(&self) -> String {
        match self {
            HotspotAdapter::Known(Some(a)) => a.friendly_name.clone(),
            HotspotAdapter::Known(None) => "none detected".to_string(),
            HotspotAdapter::Ambiguous => "multiple Wi-Fi adapters present".to_string(),
        }
    }
}

/// Resolve `hotspot_adapter` (see its doc comment in [`HotspotConfig`]) against the
/// currently present adapters.
pub fn resolve_hotspot_adapter<'a>(
    hotspot_adapter: &str,
    adapters: &'a [Adapter],
) -> HotspotAdapter<'a> {
    if !config::is_auto(hotspot_adapter) {
        return HotspotAdapter::Known(adapters.iter().find(|a| a.matches(hotspot_adapter)));
    }

    // "auto" only works out when exactly one Wi-Fi adapter could host the hotspot.
    let mut wifi_adapters = adapters.iter().filter(|a| a.is_wifi);
    let Some(only) = wifi_adapters.next() else {
        return HotspotAdapter::Known(None);
    };
    match wifi_adapters.next() {
        Some(_) => HotspotAdapter::Ambiguous,
        None => HotspotAdapter::Known(Some(only)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Level = Option<NetworkConnectivityLevel>;

    const INTERNET: Level = Some(NetworkConnectivityLevel::InternetAccess);
    const CONSTRAINED: Level = Some(NetworkConnectivityLevel::ConstrainedInternetAccess);
    const LOCAL: Level = Some(NetworkConnectivityLevel::LocalAccess);
    const NONE: Level = Some(NetworkConnectivityLevel::None);

    #[test]
    fn connectivity_ranks_internet_highest_and_unknown_lowest() {
        assert!(Connectivity::of(INTERNET) > Connectivity::of(CONSTRAINED));
        assert!(Connectivity::of(CONSTRAINED) > Connectivity::of(LOCAL));
        assert!(Connectivity::of(LOCAL) > Connectivity::of(NONE));
        assert_eq!(Connectivity::of(None), Connectivity::None);
    }

    #[test]
    fn ethernet_always_outranks_a_non_ethernet_adapter() {
        // Even a barely-connected Ethernet adapter beats a fully-connected non-Ethernet
        // one -- this app doesn't care about the uplink's own internet access, and a
        // wired link is always the safe, predictable choice over anything else.
        assert!(uplink_score(NONE, true) > uplink_score(INTERNET, false));
    }

    #[test]
    fn connectivity_rank_breaks_a_tie_within_the_same_ethernet_ness() {
        assert!(uplink_score(INTERNET, true) > uplink_score(LOCAL, true));
        assert!(uplink_score(INTERNET, false) > uplink_score(LOCAL, false));
    }

    #[test]
    fn resolves_hotspot_adapter_automatically_with_exactly_one_wifi_adapter() {
        let adapters = [Adapter::test(1).ethernet(), Adapter::test(2).wifi()];
        let resolved = resolve_hotspot_adapter("auto", &adapters);
        assert!(matches!(resolved, HotspotAdapter::Known(Some(a)) if a.guid == adapters[1].guid));
    }

    #[test]
    fn auto_resolution_is_none_without_any_wifi_adapter() {
        let adapters = [Adapter::test(1).ethernet()];
        let resolved = resolve_hotspot_adapter("auto", &adapters);
        assert!(matches!(resolved, HotspotAdapter::Known(None)));
    }

    #[test]
    fn auto_resolution_is_ambiguous_with_two_wifi_adapters() {
        let adapters = [Adapter::test(1).wifi(), Adapter::test(2).wifi()];
        let resolved = resolve_hotspot_adapter("auto", &adapters);
        assert!(matches!(resolved, HotspotAdapter::Ambiguous));
    }

    #[test]
    fn a_named_hotspot_adapter_is_matched_by_friendly_name() {
        let adapters = [Adapter::test(1).ethernet(), Adapter::test(2).wifi()];
        let resolved = resolve_hotspot_adapter(&adapters[1].friendly_name, &adapters);
        assert!(matches!(resolved, HotspotAdapter::Known(Some(a)) if a.guid == adapters[1].guid));
    }

    #[test]
    fn a_named_hotspot_adapter_not_currently_present_resolves_to_none() {
        let adapters = [Adapter::test(1).ethernet()];
        let resolved = resolve_hotspot_adapter("some other adapter", &adapters);
        assert!(matches!(resolved, HotspotAdapter::Known(None)));
    }

    #[test]
    fn known_hotspot_adapter_excludes_only_itself() {
        let hotspot = Adapter::test(1).wifi();
        let other = Adapter::test(2).ethernet();
        let resolution = HotspotAdapter::Known(Some(&hotspot));
        assert!(resolution.excludes(Some(&hotspot)));
        assert!(!resolution.excludes(Some(&other)));
        assert!(!resolution.excludes(None));
    }

    #[test]
    fn known_none_excludes_nothing() {
        let other = Adapter::test(2).ethernet();
        let resolution: HotspotAdapter = HotspotAdapter::Known(None);
        assert!(!resolution.excludes(Some(&other)));
        assert!(!resolution.excludes(None));
    }

    #[test]
    fn ambiguous_excludes_every_wifi_adapter_but_not_others() {
        let wifi = Adapter::test(1).wifi();
        let ethernet = Adapter::test(2).ethernet();
        let resolution = HotspotAdapter::Ambiguous;
        assert!(resolution.excludes(Some(&wifi)));
        assert!(!resolution.excludes(Some(&ethernet)));
        assert!(!resolution.excludes(None));
    }
}
