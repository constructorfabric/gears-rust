//! Typed configuration of the Rating gear (`gears.bss-rating.config`).
//!
//! Every event source is disabled by default and pull reads stay authoritative. A source can be
//! enabled only once its owner and Rating have adopted the same event contract (DESIGN §3.3 event
//! table, T-D-71); none has been, so enabling any source is refused at start-up.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use cluster_sdk::ElectionConfig;
use serde::Deserialize;
use toolkit_db::outbox::Partitions;
use uuid::Uuid;

use crate::domain::error::Cause;
use crate::domain::inbox::{InboxSource, ProducerPolicy};
use crate::domain::usage::feed::{FeedSubscription, MAX_PAGE_LIMIT, MeterId};
use crate::domain::usage::reader::ReaderSettings;
use crate::domain::usage::source::{SourceDefinition, TenantScope};
use crate::infra::queues::{MAX_FEED_EPOCHS, QueueLayout, QueuePartitions};

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
    /// The usage feed readers (feature 04).
    pub usage_feed: UsageFeedConfig,
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
    /// The feed epochs whose delivery queues are served (DESIGN §4.9); at most
    /// [`MAX_FEED_EPOCHS`].
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
    /// Election lease TTL, in seconds; at most [`MAX_LEADER_TTL_SECS`].
    pub ttl_secs: u64,
    /// Renewals a leader may miss before it loses the lease; at least 1.
    pub max_missed_renewals: u8,
    /// Seconds a leader's work has to stop after a loss before it is aborted; at most
    /// [`MAX_LEADER_STOP_SECS`].
    pub stop_timeout_secs: u64,
    /// The longest pause, in seconds, between attempts to resolve the Cluster backend; at most
    /// [`MAX_RESOLVE_RETRY_SECS`].
    pub resolve_retry_max_secs: u64,
}

/// The longest stop budget a leader's work may have: with the resign budget it must fit inside
/// the gear's 30 s stop budget, which itself stays inside the host's 35 s shutdown deadline.
pub const MAX_LEADER_STOP_SECS: u64 = 20;

/// The longest election TTL: joining waits at most one TTL, and a failover waits one out.
pub const MAX_LEADER_TTL_SECS: u64 = 300;

/// The longest pause between attempts to resolve the Cluster backend: a backend that comes back
/// is joined within it.
pub const MAX_RESOLVE_RETRY_SECS: u64 = 900;

/// The longest pause between exception sweeps: one day.
pub const MAX_SWEEP_INTERVAL_SECS: u64 = 86_400;

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
    /// Seconds between sweeps (15 minutes by default); at most [`MAX_SWEEP_INTERVAL_SECS`].
    pub interval_secs: u64,
}

impl Default for ExceptionSweepConfig {
    fn default() -> Self {
        Self { interval_secs: 900 }
    }
}

/// The usage feed readers (`rating.usage_feed`; feature 04, DESIGN [12 §3.3]).
///
/// [12 §3.3]: ../../docs/DESIGN.md#contract-12-3-3
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageFeedConfig {
    /// One canonical source per entry, at most [`MAX_SUBSCRIPTIONS`]. Empty by default: no feed
    /// is read.
    pub subscriptions: Vec<FeedSubscriptionConfig>,
    /// The generation of `subscriptions`: raise it with every change of them. An instance whose
    /// generation is older than the one the stored sources were reconciled under does not
    /// reconcile them, so an old instance restarting mid-rollout cannot revert a newer plan.
    pub config_generation: u64,
    /// Entries per page, 1 to 1 000.
    pub page_limit: u16,
    /// Seconds between polls once a source is caught up or paused.
    pub poll_interval_secs: u64,
    /// `max_queue_depth`: above this many `rating.child_work` messages the readers stop fetching,
    /// and they resume below 80 % of it.
    pub max_queue_depth: u64,
    /// The longest pause, in seconds, after a failed read (backoff 1 s → this).
    pub backoff_max_secs: u64,
    /// The collector's retention floor `F` in days (backfill window + replay horizon), which
    /// bounds the days a source loss taints.
    pub retention_floor_days: u32,
    /// Seconds after which one call to the feed, the meter registry or the tenant hierarchy is
    /// given up and backed off from.
    pub call_timeout_secs: u64,
}

impl Default for UsageFeedConfig {
    fn default() -> Self {
        Self {
            subscriptions: Vec::new(),
            config_generation: 0,
            page_limit: MAX_PAGE_LIMIT,
            poll_interval_secs: 30,
            max_queue_depth: 1_000_000,
            backoff_max_secs: 300,
            retention_floor_days: 125,
            call_timeout_secs: 30,
        }
    }
}

/// One feed subscription: the meters it reads and its configured tenant scope (the roots of the
/// tenant subtrees Rating reads it for). Both are part of the source's identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeedSubscriptionConfig {
    /// Meter ids, `gts.cf.core.uc.usage_record.v2~<segment>~`; 1 to 100.
    pub meters: Vec<String>,
    /// Root tenants; at least one.
    pub tenant_scope: Vec<Uuid>,
}

/// The longest poll interval: an hour.
pub const MAX_POLL_INTERVAL_SECS: u64 = 3_600;

/// The longest backoff after a failed read (DESIGN §4.4: 1 s → 5 min; up to 15 min allowed).
pub const MAX_FEED_BACKOFF_SECS: u64 = 900;

/// The longest retention floor a deployment may declare: ten years.
pub const MAX_RETENTION_FLOOR_DAYS: u32 = 3_650;

/// The longest time limit of one call to the feed, the meter registry or the tenant hierarchy.
pub const MAX_CALL_TIMEOUT_SECS: u64 = 300;

/// The most feed subscriptions: each is a reader task, a Cluster election and PDP checks per root.
pub const MAX_SUBSCRIPTIONS: usize = 32;

/// The resolved usage feed configuration.
#[derive(Debug, Clone)]
pub struct UsageFeedSettings {
    /// The configured sources.
    pub sources: Vec<SourceDefinition>,
    /// The generation of the configured sources.
    pub config_generation: u64,
    /// The readers' tuning.
    pub reader: ReaderSettings,
    /// The collector's retention floor.
    pub retention_floor: Duration,
}

/// Why a configuration is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A partition count is not a power of two in 1–64.
    #[error("queues.{field} must be a power of two between 1 and 64, got {value}")]
    Partitions { field: &'static str, value: u16 },
    /// The feed epochs are empty, zero, repeated or too many.
    #[error("queues.feed_epochs must list 1 to {MAX_FEED_EPOCHS} distinct epochs of at least 1")]
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
    /// A leadership or sweep duration is zero or longer than its bound, so the job it paces would
    /// never run or never retry.
    #[error("{field} must be between 1 and {max}, got {value}")]
    DurationOutOfRange {
        field: &'static str,
        value: u64,
        max: u64,
    },
    /// The election timing is one the Cluster SDK refuses, so no leader job could run; the Cluster
    /// SDK's error is the source.
    #[error("leadership: {0}")]
    Leadership(#[source] Cause),
    /// More feed subscriptions than [`MAX_SUBSCRIPTIONS`].
    #[error("usage_feed.subscriptions lists {0}, more than {MAX_SUBSCRIPTIONS}")]
    TooManySubscriptions(usize),
    /// The reader's tuning is refused.
    #[error("usage_feed: {0}")]
    ReaderSettings(String),
    /// A feed subscription names no meter, too many, a malformed one or no valid tenant scope.
    #[error("usage_feed.subscriptions[{index}]: {detail}")]
    FeedSubscription { index: usize, detail: String },
    /// Two feed subscriptions are the same source (the same meters and tenant scope).
    #[error("usage_feed.subscriptions[{index}] repeats an earlier subscription")]
    DuplicateSubscription { index: usize },
    /// The page limit is outside 1–1 000.
    #[error("usage_feed.page_limit must be between 1 and {MAX_PAGE_LIMIT}, got {0}")]
    PageLimit(u16),
    /// `max_queue_depth` is zero.
    #[error("usage_feed.max_queue_depth must be at least 1")]
    QueueDepth,
}

/// A configuration [`RatingConfig::resolve`] accepted, in the shape `init` builds the gear from, so
/// what was checked is what runs.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    /// The work queues to declare.
    pub queues: QueueLayout,
    /// The producer identities the inbox verifies events against.
    pub producers: ProducerPolicy,
    /// The election timing; `None` when leadership is disabled.
    pub election: Option<ElectionConfig>,
    /// The usage feed readers.
    pub usage: UsageFeedSettings,
}

impl RatingConfig {
    /// Checks the configuration.
    ///
    /// # Errors
    ///
    /// The first [`ConfigError`] found.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.resolve().map(drop)
    }

    /// Checks the configuration and builds what the gear runs from.
    ///
    /// # Errors
    ///
    /// The first [`ConfigError`] found.
    pub fn resolve(&self) -> Result<ResolvedConfig, ConfigError> {
        let queues = self.queue_layout()?;
        let producers = self.producer_policy()?;
        for (field, value, max) in [
            (
                "leadership.ttl_secs",
                self.leadership.ttl_secs,
                MAX_LEADER_TTL_SECS,
            ),
            (
                "leadership.stop_timeout_secs",
                self.leadership.stop_timeout_secs,
                MAX_LEADER_STOP_SECS,
            ),
            (
                "leadership.resolve_retry_max_secs",
                self.leadership.resolve_retry_max_secs,
                MAX_RESOLVE_RETRY_SECS,
            ),
            (
                "exception_sweep.interval_secs",
                self.exception_sweep.interval_secs,
                MAX_SWEEP_INTERVAL_SECS,
            ),
            (
                "usage_feed.poll_interval_secs",
                self.usage_feed.poll_interval_secs,
                MAX_POLL_INTERVAL_SECS,
            ),
            (
                "usage_feed.backoff_max_secs",
                self.usage_feed.backoff_max_secs,
                MAX_FEED_BACKOFF_SECS,
            ),
            (
                "usage_feed.retention_floor_days",
                u64::from(self.usage_feed.retention_floor_days),
                u64::from(MAX_RETENTION_FLOOR_DAYS),
            ),
            (
                "usage_feed.call_timeout_secs",
                self.usage_feed.call_timeout_secs,
                MAX_CALL_TIMEOUT_SECS,
            ),
        ] {
            if !(1..=max).contains(&value) {
                return Err(ConfigError::DurationOutOfRange { field, value, max });
            }
        }
        let election = if self.leadership.enabled {
            let election =
                ElectionConfig::new(self.leadership_ttl(), self.leadership.max_missed_renewals)
                    .map_err(|error| ConfigError::Leadership(Cause::new(error)))?;
            Some(election)
        } else {
            None
        };
        Ok(ResolvedConfig {
            queues,
            producers,
            election,
            usage: self.usage_feed_settings()?,
        })
    }

    /// The usage feed readers' sources and tuning; only [`Self::resolve`] calls it, after the
    /// duration ranges are checked.
    fn usage_feed_settings(&self) -> Result<UsageFeedSettings, ConfigError> {
        let feed = &self.usage_feed;
        if !(1..=MAX_PAGE_LIMIT).contains(&feed.page_limit) {
            return Err(ConfigError::PageLimit(feed.page_limit));
        }
        if feed.max_queue_depth == 0 {
            return Err(ConfigError::QueueDepth);
        }
        if feed.subscriptions.len() > MAX_SUBSCRIPTIONS {
            return Err(ConfigError::TooManySubscriptions(feed.subscriptions.len()));
        }
        let mut seen = BTreeSet::new();
        let mut sources = Vec::with_capacity(feed.subscriptions.len());
        for (index, subscription) in feed.subscriptions.iter().enumerate() {
            let invalid = |detail: String| ConfigError::FeedSubscription { index, detail };
            let meters = subscription
                .meters
                .iter()
                .map(|meter| MeterId::parse(meter.as_str()))
                .collect::<Result<Vec<_>, _>>()
                .and_then(FeedSubscription::new)
                .map_err(|e| invalid(e.to_string()))?;
            let scope = TenantScope::new(subscription.tenant_scope.iter().copied())
                .map_err(|e| invalid(e.to_string()))?;
            let definition = SourceDefinition::new(meters, scope);
            if !seen.insert(definition.source_id()) {
                return Err(ConfigError::DuplicateSubscription { index });
            }
            sources.push(definition);
        }
        let reader = ReaderSettings::new(
            feed.page_limit,
            Duration::from_secs(feed.poll_interval_secs),
            feed.max_queue_depth,
            Duration::from_secs(feed.backoff_max_secs),
            Duration::from_secs(feed.call_timeout_secs),
        )
        .map_err(|error| ConfigError::ReaderSettings(error.to_string()))?;
        Ok(UsageFeedSettings {
            sources,
            config_generation: feed.config_generation,
            reader,
            retention_floor: Duration::from_secs(u64::from(feed.retention_floor_days) * 86_400),
        })
    }

    /// The work queues to declare.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Partitions`] or [`ConfigError::FeedEpochs`].
    pub fn queue_layout(&self) -> Result<QueueLayout, ConfigError> {
        let q = &self.queues;
        let partitions = QueuePartitions {
            child_work: partitions("child_work_partitions", q.child_work_partitions)?,
            rollup: partitions("rollup_partitions", q.rollup_partitions)?,
            delivery: partitions("delivery_partitions", q.delivery_partitions)?,
        };
        QueueLayout::new(partitions, q.feed_epochs.clone()).map_err(|_| ConfigError::FeedEpochs)
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
