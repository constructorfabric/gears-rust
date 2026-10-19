//! Typed configuration of the Rating gear (`gears.bss-rating.config`).
//!
//! Every event source is disabled by default and pull reads stay authoritative. A source can be
//! enabled only once its owner and Rating have adopted the same event contract (DESIGN §3.3 event
//! table, T-D-71); none has been, so enabling any source is refused at start-up.

use std::collections::BTreeMap;
use std::time::Duration;

use cluster_sdk::ElectionConfig;
use serde::Deserialize;
use toolkit_db::outbox::Partitions;

use crate::domain::inbox::{InboxSource, ProducerPolicy};
use crate::infra::queues::QueueLayout;

/// The consumed event sources whose contract is adopted on both sides, so configuration may
/// enable them. Empty: every consumed event is REQUESTED of its owner (DESIGN §3.3).
pub const ADOPTED_EVENT_CONTRACTS: &[InboxSource] = &[];

/// The gear's configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RatingConfig {
    /// The outbox work queues.
    pub queues: QueueConfig,
    /// Event intake per inbox source.
    pub event_intake: EventIntakeConfig,
    /// Advisory leadership over the Cluster SDK.
    pub leadership: LeadershipConfig,
    /// The exception sweeper.
    pub exception_sweep: ExceptionSweepConfig,
}

/// Partition counts of the work queues (powers of two, 1–64).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueConfig {
    /// `rating.child_work`.
    pub child_work_partitions: u16,
    /// `rating.rollup`.
    pub rollup_partitions: u16,
    /// Each `rating.delivery.e{n}`.
    pub delivery_partitions: u16,
    /// The feed epochs whose delivery queues are served (DESIGN §4.9).
    pub feed_epochs: Vec<u32>,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            child_work_partitions: 16,
            rollup_partitions: 8,
            delivery_partitions: 8,
            feed_epochs: vec![1],
        }
    }
}

/// Event intake, keyed by inbox source name (`fact`, `segment`, …).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventIntakeConfig {
    /// The configured sources; a source left out is disabled.
    pub sources: BTreeMap<String, EventSourceConfig>,
}

/// One source's event path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventSourceConfig {
    /// Whether the event consumer of the source starts. Defaults to `false`.
    pub enabled: bool,
    /// The producer identity events of the source must carry.
    pub producer: Option<String>,
}

/// Advisory leadership (ADR-0008).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LeadershipConfig {
    /// Whether the leader-only jobs run. `false` is for a local profile with no Cluster backend:
    /// those jobs then never run, which costs only their work, never correctness.
    pub enabled: bool,
    /// Election lease TTL, in seconds.
    pub ttl_secs: u64,
    /// Renewals a leader may miss before it loses the lease; at least 1.
    pub max_missed_renewals: u8,
    /// Seconds a leader's work has to stop after a loss before it is aborted; at most
    /// [`MAX_LEADER_STOP_SECS`].
    pub stop_timeout_secs: u64,
    /// The longest pause, in seconds, between attempts to resolve the Cluster backend.
    pub resolve_retry_max_secs: u64,
}

/// The longest stop budget a leader's work may have: with the resign budget it must fit inside
/// the gear's 30 s stop budget, which itself stays inside the host's 35 s shutdown deadline.
pub const MAX_LEADER_STOP_SECS: u64 = 20;

/// The longest producer identity an event source may name.
pub const MAX_PRODUCER_LEN: usize = 255;

impl Default for LeadershipConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_secs: 30,
            max_missed_renewals: 2,
            stop_timeout_secs: 15,
            resolve_retry_max_secs: 60,
        }
    }
}

/// The exception sweeper.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExceptionSweepConfig {
    /// Seconds between sweeps (15 minutes by default).
    pub interval_secs: u64,
}

impl Default for ExceptionSweepConfig {
    fn default() -> Self {
        Self { interval_secs: 900 }
    }
}

/// Why a configuration is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A partition count is not a power of two in 1–64.
    #[error("queues.{field} must be a power of two between 1 and 64, got {value}")]
    Partitions { field: &'static str, value: u16 },
    /// The feed epochs are empty, zero or repeated.
    #[error("queues.feed_epochs must list distinct epochs of at least 1")]
    FeedEpochs,
    /// An event source name is not an inbox source.
    #[error("event_intake.sources: {0:?} is not an inbox source")]
    UnknownSource(String),
    /// A source was enabled before its event contract was adopted (T-D-71).
    #[error(
        "event_intake.sources.{0}: no event contract is adopted for this source; pull reads stay authoritative"
    )]
    ContractNotAdopted(String),
    /// An enabled source names no producer identity to verify.
    #[error("event_intake.sources.{0}: an enabled source needs its producer identity")]
    ProducerMissing(String),
    /// A producer identity is empty, padded, too long or holds a control character.
    #[error(
        "event_intake.sources.{0}.producer must be 1-255 bytes without surrounding spaces or control characters"
    )]
    ProducerInvalid(String),
    /// The leader stop budget does not fit inside the gear's stop budget.
    #[error("leadership.stop_timeout_secs must be at most {MAX_LEADER_STOP_SECS}, got {0}")]
    LeaderStopTooLong(u64),
    /// A leadership or sweep duration is zero.
    #[error("{0} must be at least 1")]
    ZeroDuration(&'static str),
    /// The election timing is one the Cluster SDK refuses, so no leader job could run.
    #[error("leadership: {0}")]
    Leadership(String),
}

impl RatingConfig {
    /// Checks the configuration.
    ///
    /// # Errors
    ///
    /// The first [`ConfigError`] found.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.queue_layout()?;
        self.producer_policy()?;
        for (field, value) in [
            ("leadership.ttl_secs", self.leadership.ttl_secs),
            (
                "leadership.stop_timeout_secs",
                self.leadership.stop_timeout_secs,
            ),
            (
                "leadership.resolve_retry_max_secs",
                self.leadership.resolve_retry_max_secs,
            ),
            (
                "exception_sweep.interval_secs",
                self.exception_sweep.interval_secs,
            ),
        ] {
            if value == 0 {
                return Err(ConfigError::ZeroDuration(field));
            }
        }
        if self.leadership.stop_timeout_secs > MAX_LEADER_STOP_SECS {
            return Err(ConfigError::LeaderStopTooLong(
                self.leadership.stop_timeout_secs,
            ));
        }
        if self.leadership.enabled {
            ElectionConfig::new(self.leadership_ttl(), self.leadership.max_missed_renewals)
                .map_err(|error| ConfigError::Leadership(error.to_string()))?;
        }
        Ok(())
    }

    /// The work queues to declare.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Partitions`] or [`ConfigError::FeedEpochs`].
    pub fn queue_layout(&self) -> Result<QueueLayout, ConfigError> {
        let q = &self.queues;
        QueueLayout::new(
            partitions("child_work_partitions", q.child_work_partitions)?,
            partitions("rollup_partitions", q.rollup_partitions)?,
            partitions("delivery_partitions", q.delivery_partitions)?,
            q.feed_epochs.clone(),
        )
        .map_err(|_| ConfigError::FeedEpochs)
    }

    /// The producer identities the inbox verifies events against.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownSource`], [`ConfigError::ProducerInvalid`],
    /// [`ConfigError::ContractNotAdopted`] or [`ConfigError::ProducerMissing`].
    pub fn producer_policy(&self) -> Result<ProducerPolicy, ConfigError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-events
        let mut policy = ProducerPolicy::default();
        for (name, source) in &self.event_intake.sources {
            let inbox_source =
                InboxSource::parse(name).ok_or_else(|| ConfigError::UnknownSource(name.clone()))?;
            if source
                .producer
                .as_deref()
                .is_some_and(|producer| !is_producer_identity(producer))
            {
                return Err(ConfigError::ProducerInvalid(name.clone()));
            }
            if !source.enabled {
                continue;
            }
            if !ADOPTED_EVENT_CONTRACTS.contains(&inbox_source) {
                return Err(ConfigError::ContractNotAdopted(name.clone()));
            }
            let producer = source
                .producer
                .clone()
                .ok_or_else(|| ConfigError::ProducerMissing(name.clone()))?;
            policy = policy.with_event_producer(inbox_source, producer);
        }
        Ok(policy)
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-events
    }

    /// The election TTL.
    #[must_use]
    pub fn leadership_ttl(&self) -> Duration {
        Duration::from_secs(self.leadership.ttl_secs)
    }

    /// The stop budget of a leader's work.
    #[must_use]
    pub fn leadership_stop_timeout(&self) -> Duration {
        Duration::from_secs(self.leadership.stop_timeout_secs)
    }

    /// The longest pause between attempts to resolve the Cluster backend.
    #[must_use]
    pub fn leadership_resolve_retry_max(&self) -> Duration {
        Duration::from_secs(self.leadership.resolve_retry_max_secs)
    }

    /// The pause between exception sweeps.
    #[must_use]
    pub fn exception_sweep_interval(&self) -> Duration {
        Duration::from_secs(self.exception_sweep.interval_secs)
    }
}

/// A producer identity an event envelope can be matched against: 1-255 bytes, no surrounding
/// whitespace, no control characters.
fn is_producer_identity(producer: &str) -> bool {
    !producer.is_empty()
        && producer.len() <= MAX_PRODUCER_LEN
        && producer.trim() == producer
        && !producer.chars().any(char::is_control)
}

fn partitions(field: &'static str, value: u16) -> Result<Partitions, ConfigError> {
    if (1..=64).contains(&value) && value.is_power_of_two() {
        Ok(Partitions::of(value))
    } else {
        Err(ConfigError::Partitions { field, value })
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
