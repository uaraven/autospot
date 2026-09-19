//! The decision logic: when should the hotspot go on, and when should it go off?
//!
//! This module is pure -- it takes the current time and whether Wi-Fi is up, and returns
//! an intent. It never touches Wi-Fi or the hotspot itself; that's `watchdog`'s job, which
//! is what makes the rules here testable on their own.

use std::time::{Duration, Instant};

/// How long to wait after a failed start before trying again, so a hotspot that cannot
/// come up doesn't get hammered once per poll.
const RETRY_BACKOFF: Duration = Duration::from_secs(60);

/// What the caller should do about the current tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Nothing to do.
    Idle,
    /// Wi-Fi just dropped; the countdown starts now.
    WifiLost,
    /// Wi-Fi is down but the threshold has not elapsed yet.
    Waiting { remaining: Duration },
    /// Wi-Fi has been down long enough: bring the hotspot up if it is not already.
    StartHotspot { down_for: Duration },
    /// A previous start attempt failed; holding off before trying again.
    HoldingForRetry { retry_in: Duration },
    /// Wi-Fi came back.
    WifiRestored {
        down_for: Duration,
        /// True when we started the hotspot and are configured to turn it back off.
        stop_hotspot: bool,
    },
}

/// What to do with a hotspot we started once Wi-Fi comes back, from
/// `monitor.auto_disable_on_reconnect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnReconnect {
    StopHotspot,
    LeaveHotspotOn,
}

pub struct Policy {
    threshold: Duration,
    on_reconnect: OnReconnect,
    retry_backoff: Duration,
    /// When the current run of disconnection began; `None` while Wi-Fi is up.
    disconnected_since: Option<Instant>,
    /// Only hotspots this process switched on are switched back off automatically.
    started_by_us: bool,
    /// Set after a failed start so we do not retry on every poll.
    retry_after: Option<Instant>,
}

impl Policy {
    pub fn new(threshold: Duration, on_reconnect: OnReconnect) -> Self {
        Self::with_backoff(threshold, on_reconnect, RETRY_BACKOFF)
    }

    pub fn with_backoff(
        threshold: Duration,
        on_reconnect: OnReconnect,
        retry_backoff: Duration,
    ) -> Self {
        Self {
            threshold,
            on_reconnect,
            retry_backoff,
            disconnected_since: None,
            started_by_us: false,
            retry_after: None,
        }
    }

    /// Feed one poll result in and get back what to do.
    pub fn evaluate(&mut self, now: Instant, wifi_connected: bool) -> Intent {
        if wifi_connected {
            let Some(since) = self.disconnected_since.take() else {
                return Intent::Idle;
            };
            self.retry_after = None;
            return Intent::WifiRestored {
                down_for: now.saturating_duration_since(since),
                stop_hotspot: self.started_by_us && self.on_reconnect == OnReconnect::StopHotspot,
            };
        }

        let Some(since) = self.disconnected_since else {
            self.disconnected_since = Some(now);
            return Intent::WifiLost;
        };

        let down_for = now.saturating_duration_since(since);
        if down_for < self.threshold {
            return Intent::Waiting {
                remaining: self.threshold.saturating_sub(down_for),
            };
        }

        if let Some(retry_after) = self.retry_after
            && now < retry_after
        {
            return Intent::HoldingForRetry {
                retry_in: retry_after.saturating_duration_since(now),
            };
        }

        Intent::StartHotspot { down_for }
    }

    /// Record that the hotspot is up because of us.
    pub fn record_started(&mut self) {
        self.started_by_us = true;
        self.retry_after = None;
    }

    /// Record that the hotspot is no longer our responsibility.
    pub fn record_stopped(&mut self) {
        self.started_by_us = false;
    }

    /// Record a failed start so the next attempt waits out the backoff.
    pub fn record_start_failure(&mut self, now: Instant) {
        self.retry_after = Some(now + self.retry_backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: Duration = Duration::from_secs(120);

    fn policy() -> (Policy, Instant) {
        (
            Policy::new(THRESHOLD, OnReconnect::StopHotspot),
            Instant::now(),
        )
    }

    /// `t0 + secs`, to keep the timeline in the tests below readable.
    fn at(t0: Instant, secs: u64) -> Instant {
        t0 + Duration::from_secs(secs)
    }

    #[test]
    fn stays_idle_while_wifi_is_up() {
        let (mut w, t0) = policy();
        assert_eq!(w.evaluate(t0, true), Intent::Idle);
        assert_eq!(w.evaluate(at(t0, 300), true), Intent::Idle);
    }

    #[test]
    fn reports_the_first_tick_of_a_disconnection() {
        let (mut w, t0) = policy();
        assert_eq!(w.evaluate(t0, false), Intent::WifiLost);
    }

    #[test]
    fn waits_out_the_full_threshold_before_starting() {
        let (mut w, t0) = policy();
        w.evaluate(t0, false);

        assert_eq!(
            w.evaluate(at(t0, 5), false),
            Intent::Waiting {
                remaining: Duration::from_secs(115),
            }
        );
        assert!(matches!(
            w.evaluate(at(t0, 119), false),
            Intent::Waiting { .. }
        ));
        assert_eq!(
            w.evaluate(t0 + THRESHOLD, false),
            Intent::StartHotspot {
                down_for: THRESHOLD
            }
        );
    }

    #[test]
    fn a_brief_blip_resets_the_countdown() {
        let (mut w, t0) = policy();
        w.evaluate(t0, false);
        w.evaluate(at(t0, 60), false);

        // Wi-Fi returns at t+70 before the threshold, so no hotspot was ever started.
        assert_eq!(
            w.evaluate(at(t0, 70), true),
            Intent::WifiRestored {
                down_for: Duration::from_secs(70),
                stop_hotspot: false,
            }
        );

        // Dropping again starts a fresh 120s countdown rather than resuming the old one.
        assert_eq!(w.evaluate(at(t0, 75), false), Intent::WifiLost);
        assert!(matches!(
            w.evaluate(at(t0, 180), false),
            Intent::Waiting { .. }
        ));
        assert!(matches!(
            w.evaluate(at(t0, 195), false),
            Intent::StartHotspot { .. }
        ));
    }

    #[test]
    fn asks_to_stop_only_a_hotspot_it_started() {
        let (mut w, t0) = policy();
        w.evaluate(t0, false);
        w.evaluate(t0 + THRESHOLD, false);
        w.record_started();

        assert_eq!(
            w.evaluate(at(t0, 200), true),
            Intent::WifiRestored {
                down_for: Duration::from_secs(200),
                stop_hotspot: true,
            }
        );
    }

    #[test]
    fn leaves_a_manually_started_hotspot_alone() {
        let (mut w, t0) = policy();
        w.evaluate(t0, false);
        // No record_started(): the hotspot was already on, not switched on by us.
        assert_eq!(
            w.evaluate(at(t0, 200), true),
            Intent::WifiRestored {
                down_for: Duration::from_secs(200),
                stop_hotspot: false,
            }
        );
    }

    #[test]
    fn honours_auto_disable_on_reconnect_being_off() {
        let mut w = Policy::new(THRESHOLD, OnReconnect::LeaveHotspotOn);
        let t0 = Instant::now();
        w.evaluate(t0, false);
        w.evaluate(t0 + THRESHOLD, false);
        w.record_started();

        assert_eq!(
            w.evaluate(at(t0, 200), true),
            Intent::WifiRestored {
                down_for: Duration::from_secs(200),
                stop_hotspot: false,
            }
        );
    }

    #[test]
    fn backs_off_after_a_failed_start_instead_of_retrying_every_poll() {
        let mut w =
            Policy::with_backoff(THRESHOLD, OnReconnect::StopHotspot, Duration::from_secs(60));
        let t0 = Instant::now();
        w.evaluate(t0, false);

        let at_threshold = t0 + THRESHOLD;
        assert!(matches!(
            w.evaluate(at_threshold, false),
            Intent::StartHotspot { .. }
        ));
        w.record_start_failure(at_threshold);

        assert_eq!(
            w.evaluate(at(at_threshold, 5), false),
            Intent::HoldingForRetry {
                retry_in: Duration::from_secs(55)
            }
        );
        assert!(matches!(
            w.evaluate(at(at_threshold, 60), false),
            Intent::StartHotspot { .. }
        ));
    }

    #[test]
    fn a_reconnect_clears_a_pending_retry_backoff() {
        let mut w = Policy::with_backoff(
            THRESHOLD,
            OnReconnect::StopHotspot,
            Duration::from_secs(600),
        );
        let t0 = Instant::now();
        w.evaluate(t0, false);
        let at_threshold = t0 + THRESHOLD;
        w.evaluate(at_threshold, false);
        w.record_start_failure(at_threshold);

        w.evaluate(at(at_threshold, 10), true);

        // Fresh outage: the old backoff must not delay the new attempt.
        let t1 = at(at_threshold, 20);
        w.evaluate(t1, false);
        assert!(matches!(
            w.evaluate(t1 + THRESHOLD, false),
            Intent::StartHotspot { .. }
        ));
    }

    #[test]
    fn keeps_asking_to_start_until_told_it_succeeded() {
        let (mut w, t0) = policy();
        w.evaluate(t0, false);
        assert!(matches!(
            w.evaluate(t0 + THRESHOLD, false),
            Intent::StartHotspot { .. }
        ));
        // No record_started(), e.g. the caller found the profile missing.
        assert!(matches!(
            w.evaluate(t0 + THRESHOLD + Duration::from_secs(5), false),
            Intent::StartHotspot { .. }
        ));
    }

    #[test]
    fn record_stopped_releases_ownership() {
        let (mut w, t0) = policy();
        w.record_started();
        w.record_stopped();

        w.evaluate(t0, false);
        assert_eq!(
            w.evaluate(at(t0, 10), true),
            Intent::WifiRestored {
                down_for: Duration::from_secs(10),
                stop_hotspot: false,
            }
        );
    }
}
