// Created: 2026-06-03 by Constructor Tech
//! Leader-election domain types: the leadership status, the capability
//! requirement and native-features descriptors, and the validated election
//! timing configuration.

use std::time::Duration;

use crate::error::ClusterError;

/// The leadership state observed by a [`LeaderWatch`](crate::leader::LeaderWatch).
///
/// This is the complete leadership state space, so the enum is deliberately
/// **closed** (not `#[non_exhaustive]`): consumers match it exhaustively inside
/// their observation loops. `Lost` is a *transient* observable transition — the
/// watch auto-reenrolls and the next [`Status`](crate::leader::LeaderWatchEvent::Status)
/// event resolves to `Leader` or `Follower`. It is never terminal; terminal
/// failures arrive as [`LeaderWatchEvent::Closed`](crate::leader::LeaderWatchEvent::Closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderStatus {
    /// This participant currently holds the claim (advisory — see the staleness
    /// bound documented on [`LeaderWatch::is_leader`](crate::leader::LeaderWatch::is_leader)).
    Leader,
    /// Another participant holds the claim; this participant is enrolled and
    /// observing.
    Follower,
    /// The claim was lost (renewals exhausted the budget, or a graceful
    /// step-down revoked it). Transient: the watch auto-reenrolls.
    Lost,
}

/// A capability a consumer can require of a leader-election backend at
/// resolution time. Each variant maps to a concrete backend characteristic
/// check (DESIGN §3.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeaderElectionCapability {
    /// Require the backend to declare linearizable election semantics
    /// ([`LeaderElectionFeatures::linearizable`] is `true`) — the condition
    /// under which at most one participant observes itself as leader.
    Linearizable,
}

/// Native capability flags a leader-election backend declares via
/// [`LeaderElectionBackend::features`](crate::leader::LeaderElectionBackend::features).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LeaderElectionFeatures {
    /// Whether the backend provides linearizable election semantics. Required
    /// for the at-most-one-leader guarantee; eventually-consistent backends may
    /// transiently elect two leaders under partition.
    pub linearizable: bool,
}

impl LeaderElectionFeatures {
    /// Creates a features descriptor.
    #[must_use]
    pub fn new(linearizable: bool) -> Self {
        Self { linearizable }
    }
}

/// Election timing configuration (DESIGN §3.1).
///
/// Construct with [`ElectionConfig::new`], which rejects misconfigured values,
/// or use [`ElectionConfig::default`] for the standard `ttl = 30s`,
/// `max_missed_renewals = 2` (derived `renewal_interval = 10s`). The fields are
/// private so a constructed value is always valid by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElectionConfig {
    ttl: Duration,
    max_missed_renewals: u8,
}

impl ElectionConfig {
    /// The default lease time-to-live: 30 seconds.
    pub const DEFAULT_TTL: Duration = Duration::from_secs(30);
    /// The default missed-renewal budget: 2.
    pub const DEFAULT_MAX_MISSED_RENEWALS: u8 = 2;

    /// Creates a validated election timing configuration.
    ///
    /// Implements `cpt-cf-clst-algo-leader-election-config-validate`: both `ttl`
    /// and `max_missed_renewals` must be greater than zero; the renewal interval
    /// is derived as `ttl / (max_missed_renewals + 1)`.
    ///
    /// # Errors
    /// Returns [`ClusterError::InvalidConfig`] if `ttl` is zero,
    /// `max_missed_renewals` is zero, or `ttl` is so small relative to the
    /// budget that the derived renewal interval would be zero (which would make
    /// a backend renewal loop busy-spin).
    pub fn new(ttl: Duration, max_missed_renewals: u8) -> Result<Self, ClusterError> {
        if ttl.is_zero() || max_missed_renewals == 0 {
            return Err(ClusterError::InvalidConfig {
                reason: format!(
                    "ttl and max_missed_renewals must both be > 0 (got ttl={ttl:?}, \
                     max_missed_renewals={max_missed_renewals})"
                ),
            });
        }
        let config = Self {
            ttl,
            max_missed_renewals,
        };
        if config.renewal_interval().is_zero() {
            return Err(ClusterError::InvalidConfig {
                reason: format!(
                    "ttl={ttl:?} is too small for max_missed_renewals={max_missed_renewals}: \
                     the derived renewal_interval would be zero"
                ),
            });
        }
        Ok(config)
    }

    /// The lease time-to-live: leadership is lost if the claim is not renewed
    /// within this window.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The number of consecutive renewal failures tolerated before a
    /// leadership-lost transition is emitted.
    #[must_use]
    pub fn max_missed_renewals(&self) -> u8 {
        self.max_missed_renewals
    }

    /// The derived renewal cadence: `ttl / (max_missed_renewals + 1)`. With the
    /// default config this is `30s / 3 = 10s`, leaving budget for two missed
    /// renewals before the TTL elapses.
    #[must_use]
    pub fn renewal_interval(&self) -> Duration {
        // `max_missed_renewals` is validated `>= 1` (or is the default `2`), so
        // the divisor is always `>= 2`; widening to u32 avoids any overflow.
        self.ttl / (u32::from(self.max_missed_renewals) + 1)
    }
}

impl Default for ElectionConfig {
    /// The standard configuration: `ttl = 30s`, `max_missed_renewals = 2`
    /// (derived `renewal_interval = 10s`). Constructed directly from the known
    /// valid defaults — `Default` cannot surface the validation error path.
    fn default() -> Self {
        Self {
            ttl: Self::DEFAULT_TTL,
            max_missed_renewals: Self::DEFAULT_MAX_MISSED_RENEWALS,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::ElectionConfig;
    use crate::error::ClusterError;

    #[test]
    fn default_config_derives_ten_second_interval() {
        let cfg = ElectionConfig::default();
        assert_eq!(cfg.ttl(), Duration::from_secs(30));
        assert_eq!(cfg.max_missed_renewals(), 2);
        assert_eq!(cfg.renewal_interval(), Duration::from_secs(10));
    }

    #[test]
    fn new_derives_interval_from_ttl_and_budget() {
        let Ok(cfg) = ElectionConfig::new(Duration::from_secs(12), 3) else {
            panic!("valid timing must construct");
        };
        // 12s / (3 + 1) = 3s.
        assert_eq!(cfg.renewal_interval(), Duration::from_secs(3));
    }

    #[test]
    fn new_rejects_zero_ttl() {
        assert!(matches!(
            ElectionConfig::new(Duration::ZERO, 2),
            Err(ClusterError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn new_rejects_zero_missed_renewals() {
        assert!(matches!(
            ElectionConfig::new(Duration::from_secs(30), 0),
            Err(ClusterError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn new_rejects_config_deriving_zero_interval() {
        // 1ns / (255 + 1) == 0ns — a non-zero ttl/budget that still derives a
        // zero renewal interval must be rejected.
        assert!(matches!(
            ElectionConfig::new(Duration::from_nanos(1), 255),
            Err(ClusterError::InvalidConfig { .. })
        ));
    }
}
