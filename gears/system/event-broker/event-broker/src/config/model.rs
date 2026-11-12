use std::time::Duration;

use gts::GtsTypeId;
use serde::Deserialize;

use crate::domain::model::BarrierMode;

/// Transport-level safety cap on `infra::dispatcher::proxy_client::proxy`'s
/// raw, not-yet-typed proxied request body - bounds a read that bypasses any
/// typed extractor entirely, so none of axum's own implicit per-extractor
/// body limits apply to it. Set to axum's own default `Bytes`/`Json`/
/// `String`/`Form` extractor limit (2 MiB) so the dispatcher ends up with the
/// same bound every other handler in this codebase already has implicitly,
/// not a new, arbitrary number. Not operator-configurable - a safety
/// backstop, not a business rule - and distinct from
/// [`BatchConfig::max_payload_bytes`], which is a domain-level rule sized
/// specifically for `events:batch`, not a general transport-safety cap (the
/// dispatcher forwards both ingest and delivery routes, not just batch
/// publishes; see that field's own doc comment for the reverse
/// cross-reference).
pub const MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Which of the four deployment-mode wiring variants this instance runs as.
/// Drives [`crate::module::EventBrokerModule`]'s per-mode service/route
/// gating (the Deployment Modes table of `DESIGN.md` "4.1 Deployment Modes";
/// `docs/ADR/0007-service-decomposition.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    Standalone,
    ClusterIngest,
    ClusterDelivery,
    ClusterDispatcher,
}

/// The `[modules.event_broker]` operator config section.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventBrokerConfig {
    pub mode: DeploymentMode,

    #[serde(default)]
    pub batch: BatchConfig,
    #[serde(default)]
    pub publish: PublishConfig,
    #[serde(default)]
    pub subscription: SubscriptionConfig,
    #[serde(default)]
    pub streaming: StreamingConfig,
    #[serde(default)]
    pub loader: LoaderConfig,
    #[serde(default)]
    pub workers: WorkersConfig,
    #[serde(default)]
    pub registration: RegistrationConfig,
    #[serde(default)]
    pub provisioning: ProvisioningConfig,
    /// Per-topic deployment settings, keyed by topic GTS identifier. Empty by
    /// default, which makes every topic unresolvable - a deployment that serves
    /// any topic configures at least the instance-less key for its type.
    #[serde(default)]
    pub topics: TopicSettingsMap,
    /// Every backend instance this deployment runs, keyed by registry name
    /// (feature 0005 §2.1). Held as written: `crate::config::checks` validates
    /// the names, and each entry's provider validates its settings.
    #[serde(default)]
    pub backends: std::collections::BTreeMap<String, BackendEntry>,
    /// Which registry entry holds a topic's log, keyed the way `topics` is
    /// (feature 0005 §2.2). Held as written, for the same reason.
    #[serde(default)]
    pub backend_routing: std::collections::BTreeMap<String, String>,
    /// Named ingest outboxes and the topics each carries (feature 0005 §2.5),
    /// held as written. Only the reserved group `default` is accepted today;
    /// absent, it has 16 partitions.
    #[serde(default)]
    pub outbox_groups: std::collections::BTreeMap<String, OutboxGroupEntry>,
}

/// One `outbox_groups` entry, exactly as written.
///
/// ```yaml
/// outbox_groups:
///   default: { partitions: 16 }
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxGroupEntry {
    /// Ingest partitions the group drains through. Wider than the range it
    /// must fall in, so an out-of-range count reaches the startup checks and
    /// is reported there with its rule rather than as a bare parse error.
    pub partitions: u32,
    /// The topic patterns the group carries. `default` carries every topic and
    /// states none; held so that a `default` stating some is refused rather
    /// than silently ignored.
    #[serde(default)]
    pub topics: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BatchConfig {
    pub max_size: u32,
    /// Domain-level, operator-configurable ceiling for `events:batch`
    /// (`DESIGN.md` §2.2) - distinct from
    /// [`MAX_REQUEST_BODY_BYTES`](crate::config::MAX_REQUEST_BODY_BYTES),
    /// the dispatcher's fixed transport-level safety cap on the proxied body
    /// it forwards, which this field does not back (the dispatcher forwards
    /// more than just batch publishes).
    pub max_payload_bytes: u64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        // `DESIGN.md` §2.2 Constraints: 100 events / 1 MiB batch hard limit.
        Self {
            max_size: 100,
            max_payload_bytes: 1_048_576,
        }
    }
}

/// The `[modules.event_broker.publish]` section: synchronous-publish
/// (`Prefer: wait=N`) tuning.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PublishConfig {
    /// The ceiling on a `Prefer: wait=N` publish's hold, in seconds. A request
    /// asking for more is capped to this; the broker never holds a publish open
    /// longer than the operator allows. Default 50, to stay inside the 60s
    /// request deadline the publish routes declare to api-gateway. In
    /// the ingest-serving modes (`standalone` and `cluster_ingest`) a startup
    /// check refuses a value above
    /// [`MAX_PUBLISH_WAIT_SECS`](crate::config::checks::MAX_PUBLISH_WAIT_SECS).
    pub max_wait_secs: u32,
}

impl Default for PublishConfig {
    fn default() -> Self {
        // Below the publish routes' declared api-gateway deadline; see
        // `MAX_PUBLISH_WAIT_SECS`.
        Self { max_wait_secs: 50 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SubscriptionConfig {
    /// How long a subscription may sit joined without ever opening a stream
    /// before it is reaped. Default 60. Must be at least 1: 0 would reap a
    /// joined subscription at the next sweep, at most a second after it
    /// joined.
    #[serde(deserialize_with = "deserialize_nonzero_secs")]
    pub join_timeout_secs: u32,
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            join_timeout_secs: 60,
        }
    }
}

/// Consumption-stream cadence (`GET /v1/events:stream`/`:sse`).
/// `event-broker-consumption-frames`'s own wording is explicit that the
/// heartbeat cadence is "a constant *configurable* idle cadence (default
/// 5s)" - not a fixed constant - since some intermediaries/load balancers
/// need a shorter cadence to keep an idle connection open than the default
/// assumes (matching gRPC's own per-channel-configurable keepalive
/// interval). Wired into `DeliveryServiceImpl::with_heartbeat_interval` when
/// `module.rs` constructs the service (lands with this change's Group 10 -
/// production routing wiring).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StreamingConfig {
    /// Idle cadence. Any frame resets it, so a delivering stream never pays for
    /// a heartbeat. Default 5. Must be at least 1: 0 would keep the heartbeat
    /// permanently due, so the session loop would never park.
    #[serde(deserialize_with = "deserialize_nonzero_secs")]
    pub heartbeat_interval_secs: u32,
    /// Events one read may return. Bounds a session's working set, and with the
    /// byte bound below is what stops one slow consumer pulling a partition's
    /// whole tail into memory. Default 256.
    pub read_batch_max_events: usize,
    /// Bytes one read may return. Both bounds apply; whichever binds first
    /// wins. Default 1 MiB.
    pub read_batch_max_bytes: usize,
    /// Events a partition may examine without delivering, before a
    /// `control:progress` frame is owed. Roughly four full read batches, so a
    /// stream whose filter rejects everything still reports where it has got
    /// to. Default 1000.
    ///
    /// Counted as the filter rejects them rather than taken as the distance
    /// between two sequence numbers: sequences may have gaps and are not
    /// populated contiguously, so that distance is not a number of events.
    pub progress_drift_threshold: usize,
    /// Floor between two `control:progress` frames. Without it a heavily
    /// filtered stream would emit one per batch, each carrying almost no new
    /// information. Default 30. Must be at least 1: 0 would keep the progress
    /// timer permanently due, so the session loop would never park.
    #[serde(deserialize_with = "deserialize_nonzero_secs")]
    pub progress_min_interval_secs: u32,
    /// How long a consume answer the PDP allowed stays in the decision memo,
    /// keyed by the exact request it answered, and how long each interest
    /// group of a member's consume scope serves before it is evaluated again.
    /// The first window of a scope a stream open evaluates is staggered:
    /// group `k` of `G` serves for `ttl x (1 + k / G)`. Default 60. Must be at
    /// least 1, and greater than `2 x authz_eval_deadline_secs + 2` where
    /// delivery runs (a startup check), since a stream's refresh worker
    /// re-evaluates a group one refresh lead (`StreamingConfig::refresh_lead`)
    /// before it expires and a shorter window would be due again the moment
    /// it is stored.
    #[serde(deserialize_with = "deserialize_nonzero_secs")]
    pub authz_scope_ttl_secs: u32,
    /// The longest the delivery service waits on the PDP: each consume
    /// evaluation of JOIN and stream open, each I and E a stream's scope
    /// refresh asks, and the one wave of single-type evaluations a read
    /// batch asks. A stream whose batch misses it ends without advancing
    /// past the batch. It also sets the refresh lead
    /// (`StreamingConfig::refresh_lead`) and so the startup check on
    /// `authz_scope_ttl_secs`. Default 10. Must be at least 1: 0 would fail
    /// every evaluation that does not answer at once.
    #[serde(deserialize_with = "deserialize_nonzero_secs")]
    pub authz_eval_deadline_secs: u32,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: 5,
            read_batch_max_events: 256,
            read_batch_max_bytes: 1024 * 1024,
            progress_drift_threshold: 1000,
            progress_min_interval_secs: 30,
            authz_scope_ttl_secs: 60,
            authz_eval_deadline_secs: 10,
        }
    }
}

/// The margin a refresh lead adds to its evaluation deadlines, for the indexed
/// candidate query that runs before them.
const REFRESH_MARGIN_SECS: u64 = 2;

impl StreamingConfig {
    /// How long before an interest group of a consume scope expires the
    /// stream's refresh worker re-evaluates it: one evaluation deadline per
    /// PDP call the refresh makes (I and E for an `ignore` group, E alone for
    /// a `respect` one) plus 2 seconds for the candidate query. At the
    /// defaults, 12 seconds for `respect` and 22 for `ignore`.
    #[must_use]
    pub fn refresh_lead(&self, barrier_mode: BarrierMode) -> Duration {
        let calls: u64 = match barrier_mode {
            BarrierMode::Respect => 1,
            BarrierMode::Ignore => 2,
        };
        Duration::from_secs(calls * u64::from(self.authz_eval_deadline_secs) + REFRESH_MARGIN_SECS)
    }

    /// Whether a scope window outlasts the longest refresh lead, the
    /// `ignore` one: `authz_scope_ttl_secs > 2 x authz_eval_deadline_secs +
    /// 2`. Otherwise a refreshed group would be due again the moment it is
    /// stored, and its refresh worker would evaluate without pause.
    #[must_use]
    pub fn outlasts_refresh_lead(&self) -> bool {
        Duration::from_secs(u64::from(self.authz_scope_ttl_secs))
            > self.refresh_lead(BarrierMode::Ignore)
    }
}

/// The per-instance loader: what fills partition caches ahead of readers, and
/// what bounds the memory it holds.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LoaderConfig {
    /// Concurrent backend reads across every partition on the instance.
    /// Default 16.
    ///
    /// Shared deliberately rather than per partition: one fetch serves every
    /// reader standing at the same position, so the pool bounds latency rather
    /// than the work sent to storage.
    pub pool_permits: usize,
    /// Events one backend fetch may return. Default 256.
    pub fetch_max_events: usize,
    /// How long the loader waits before looking again when a round issued no
    /// fetch. Default 50.
    ///
    /// A floor on latency rather than the mechanism for it. Pacing matters:
    /// an unpaced version of this loop was measured spinning and starving the
    /// tasks it exists to serve.
    pub tick_ms: u64,
    /// Shortest wait before re-polling a partition whose tail has not
    /// materialised. Default 1.
    pub poll_floor_ms: u64,
    /// Longest such wait. Default 64.
    ///
    /// A cluster notification can arrive before the backend has assigned the
    /// sequence, so an empty tail fetch is expected rather than exceptional and
    /// the poller is what keeps a parked reader live.
    pub poll_ceiling_ms: u64,
    /// Added to a demand's rank per round it went unserved. Default 10.
    ///
    /// Additive, not multiplicative, which is what makes eventual service a
    /// guarantee: credit grows independently of how few readers a demand
    /// serves, so any demand eventually outranks any fixed fan-out. Zero is
    /// pure fan-out and can starve a lagging reader indefinitely.
    pub starvation_weight: usize,
    /// Bytes one partition's resident segments may occupy. Default 8 MiB.
    pub residency_limit_bytes: usize,
    /// How wide a gap between reader clusters must be before the span between
    /// them is dropped, in events. Default 16384.
    ///
    /// Dropping it costs the lagging cluster a refetch and buys back the memory
    /// between them; never zero in effect, since a window always keeps at least
    /// the sequence a reader wants next.
    pub gap_threshold_events: usize,
}

impl Default for LoaderConfig {
    fn default() -> Self {
        Self {
            pool_permits: 16,
            fetch_max_events: 256,
            tick_ms: 50,
            poll_floor_ms: 1,
            poll_ceiling_ms: 64,
            starvation_weight: 10,
            residency_limit_bytes: 8 * 1024 * 1024,
            gap_threshold_events: 16_384,
        }
    }
}

/// How topics are provisioned on their storage backends (feature 0006).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProvisioningConfig {
    /// An attempt no instance has acted on for this long may be replaced by a
    /// new one. Must exceed the longest a backend plausibly takes to answer one
    /// `provision` call. Default 5 minutes.
    #[serde(
        default = "default_attempt_timeout",
        with = "toolkit_utils::humantime_serde"
    )]
    pub attempt_timeout: std::time::Duration,
}

impl Default for ProvisioningConfig {
    fn default() -> Self {
        Self {
            attempt_timeout: default_attempt_timeout(),
        }
    }
}

const fn default_attempt_timeout() -> std::time::Duration {
    std::time::Duration::from_mins(5)
}

/// Broker-owned background workers: `reaper` (expired subscriptions +
/// idempotency-key cleanup) and the specification refresh. Event retention is
/// not among them: each storage backend enforces it on its own schedule.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[allow(clippy::struct_field_names)]
pub struct WorkersConfig {
    pub reaper_interval_secs: u32,
    /// How often the ingest role re-reads `types-registry` and re-resolves what
    /// it holds. Default 60.
    ///
    /// A cadence, not a bound: a topic registered while the process is running
    /// becomes resolvable within one tick. Sixty seconds because nothing
    /// registers a topic at runtime today - `types-registry` commits
    /// configuration-seeded entities once, at boot - so the interval covers an
    /// operator using its REST API, not a hot path.
    pub specification_refresh_interval_secs: u32,
}

impl Default for WorkersConfig {
    fn default() -> Self {
        Self {
            reaper_interval_secs: 60,
            specification_refresh_interval_secs: 60,
        }
    }
}

/// `DirectoryService` self-registration address in `cluster_ingest`/
/// `cluster_delivery` mode - unused
/// in `standalone`/`cluster_dispatcher` mode. Mirrors `grpc-hub`'s
/// `listen_addr`/`advertise_addr` pattern
/// (`gears/system/grpc-hub/src/gear.rs`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RegistrationConfig {
    /// The address this instance registers when `advertise_addr` is unset, and
    /// whose port an `advertise_addr` without one takes. The broker binds
    /// nothing to it: the REST listener is the API gateway's.
    pub listen_addr: String,
    /// The address other instances (the dispatcher, in cluster mode) reach
    /// this one at, if different from `listen_addr`. Accepted forms:
    /// `host:<u16>` (literal host and port; `:0` means "use `listen_addr`'s
    /// port") or `host` alone (`listen_addr`'s port is appended).
    /// `None` falls back to `listen_addr` unless its host is a wildcard
    /// address (`0.0.0.0`), which fails `init` in cluster mode rather than
    /// registering an unroutable address.
    #[serde(default)]
    pub advertise_addr: Option<String>,
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:0".to_owned(),
            advertise_addr: None,
        }
    }
}

/// Partitions a topic has when no configuration entry states a count.
///
/// Configuration always carries an entry for the instance-less key of the topic
/// base type, and this is the count it supplies: a deployment that names no
/// topics at all still serves every topic registered against it. An entry an
/// operator writes overrides this, and a topic's own entry overrides both.
///
/// Eight rather than one: a partition count cannot be changed on a live topic,
/// so the value that needs no thought errs towards leaving room for a consumer
/// group to parallelise rather than towards collapsing it.
pub const BUILT_IN_PARTITIONS: i32 = 8;

/// Default retention duration when nothing states one.
/// A week: long enough that a consumer group down over a weekend still
/// catches up, short enough that an unattended process does not fill a disk.
pub const DEFAULT_RETENTION_DURATION: std::time::Duration = std::time::Duration::from_hours(24 * 7);

/// Why a topic's settings could not be resolved.
///
/// Every variant is a startup-time operator mistake, not a runtime condition:
/// resolution reads configuration only, so the same topic either always
/// resolves or never does.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopicSettingsError {
    /// A partition count that cannot describe a partitioned topic.
    #[error("topic '{topic}' configures {partitions} partitions; the minimum is 1")]
    PartitionsOutOfRange { topic: String, partitions: i32 },

    /// The topic identifier does not decompose into a type and an instance
    /// part, so no instance-less key could be derived from it.
    #[error("topic identifier '{topic}' is not a well-formed GTS instance id: {reason}")]
    MalformedTopicId { topic: String, reason: String },
}

/// Per-topic deployment settings, keyed by topic GTS identifier.
///
/// A key whose instance part is empty (`gts.cf.core.events.topic.v1~`) supplies
/// the settings for every topic of that type; a fully qualified key
/// (`gts.cf.core.events.topic.v1~cf.billing.usage.v1`) supplies them for that
/// one topic and takes precedence.
///
/// Both live in the same map, distinguished only by whether the key names an
/// instance, because that is already how GTS expresses "the type itself"
/// rather than an instance of it - a separate `defaults` block would put the
/// default somewhere structurally different from the overrides it backs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct TopicSettingsMap {
    pub entries: std::collections::BTreeMap<String, TopicSettingsEntry>,
}

/// One entry in [`TopicSettingsMap`], exactly as written.
///
/// Every field is optional so that a fully qualified entry overrides only what
/// it names and inherits the rest from the instance-less entry for its type -
/// the common case is a topic that needs more partitions and nothing else, and
/// making it restate everything would be a place for the two to drift.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopicSettingsEntry {
    /// Partitions this topic has. Unstated here means this entry says nothing
    /// about the count, so resolution falls through to the instance-less entry
    /// for the topic's type and then to [`BUILT_IN_PARTITIONS`] - never to one,
    /// which would silently collapse a topic's parallelism while every consumer
    /// of it still looked healthy.
    #[serde(default)]
    pub partitions: Option<i32>,
    /// How this topic's partitions are kept bounded. Each bound resolves on its
    /// own, so an entry may raise a duration without restating a byte bound.
    /// That is only expressible because [`RetentionSize`] distinguishes "this
    /// entry says nothing" from "this topic is explicitly unbounded"; with a
    /// bare `Option` the second would be unsayable against a bounded default.
    ///
    /// Written at the topic level rather than in a backend's settings because the broker
    /// resolves the effective bound itself: a topic may declare a retention of
    /// its own and an operator may override it, so the value cannot live inside
    /// a document only one provider understands. Enforcing it is the backend's.
    #[serde(default)]
    pub retention: Option<RetentionSettingsEntry>,
}

/// One `backends` entry, exactly as written: which backend type, and whatever
/// that backend's own settings are.
///
/// ```yaml
/// backends:
///   main:
///     type: gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~
///     path: /var/lib/event-broker/event_log.db
/// ```
///
/// No `deny_unknown_fields` here, and it is not an omission: everything beside
/// `type` belongs to the backend, whose own settings type rejects a key it
/// does not define. Denying unknown fields at this level would put the gear in
/// charge of what a backend it knows nothing about may be configured with, and
/// `serde` cannot combine that attribute with a flattened remainder anyway.
#[derive(Clone, Deserialize)]
pub struct BackendEntry {
    /// The GTS backend type, derived from `gts.cf.core.events.backend.v1~`.
    pub r#type: GtsTypeId,
    /// Everything else in the entry, held as written and handed to the
    /// backend's provider verbatim. `${VAR}` references stay unexpanded here;
    /// the provider expands them when it builds the backend.
    #[serde(flatten)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// Prints the type and how many settings there are, never their values: a
/// settings value may be a credential, and a `{:?}` of the configuration ends
/// up in panics, assertions and logs (feature 0005 §2.4, AC-5).
impl std::fmt::Debug for BackendEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendEntry")
            .field("type", &self.r#type)
            .field("settings", &format_args!("<{} keys>", self.settings.len()))
            .finish()
    }
}

/// A topic entry's `retention` block, exactly as written.
///
/// Every field is optional in the sense that matters for resolution: an absent
/// one is a statement about nothing, not a statement of a default value. Only
/// the resolved [`RetentionSettings`] carries concrete bounds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionSettingsEntry {
    /// Age past which an event may be removed, as a humantime string (`30d`,
    /// `12h`). Unstated here falls through to the entry for the topic's type,
    /// then to the topic's own declared retention, and last to
    /// [`DEFAULT_RETENTION_DURATION`].
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    pub duration: Option<std::time::Duration>,
    /// Bytes past which the oldest events may be removed. Omit the key to
    /// state nothing about the bound; write `null` to state that this topic
    /// has none.
    ///
    /// The unit is in the name because the value carries none. A human-readable
    /// form (`128mb`) is the intended end state and will arrive with a shared
    /// platform helper, the way `duration` already rides `humantime`; until that
    /// exists, an unqualified integer would be ambiguous.
    ///
    /// What the figure counts is the event payload plus the projected columns,
    /// not the space the database finally occupies. Indexes and per-row storage
    /// overhead sit on top of it and are deliberately unaccounted, so a bound of
    /// 128,000,000 permits a file meaningfully larger than that - by roughly
    /// half again. The bound exists to stop unbounded growth, not to predict a
    /// file size.
    #[serde(default, deserialize_with = "deserialize_retention_size")]
    pub size_bytes: RetentionSize,
}

/// What an entry says about a partition's byte bound.
///
/// Three states rather than an `Option`, because two of them are the same value
/// and different statements: a topic inheriting a bounded default and a topic
/// released from it both end up unbounded, but only the second overrides
/// anything. An `Option` would make the release unsayable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RetentionSize {
    /// The key was absent. Resolution looks at the next source.
    #[default]
    Unstated,
    /// The key was present and `null`. This topic is bounded by duration alone,
    /// whatever a broader entry says.
    Unbounded,
    /// The key named a byte count.
    Bytes(u64),
}

impl RetentionSize {
    /// Whether this entry made a statement at all, which is what decides
    /// between using it and falling through to the next source.
    #[must_use]
    pub fn is_stated(self) -> bool {
        !matches!(self, Self::Unstated)
    }

    /// The bound as the backend takes it: `None` is unbounded.
    #[must_use]
    pub fn bytes(self) -> Option<u64> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::Unstated | Self::Unbounded => None,
        }
    }
}

/// A whole number of seconds that must be at least 1. Each field using it
/// says why 0 is invalid for it.
fn deserialize_nonzero_secs<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let secs = u32::deserialize(deserializer)?;
    if secs == 0 {
        return Err(serde::de::Error::custom("must be at least 1 second"));
    }
    Ok(secs)
}

/// Absent is handled by `serde(default)`, so reaching here means the key was
/// written: `null` is a deliberate "no bound", and a number is the bound.
fn deserialize_retention_size<'de, D>(deserializer: D) -> Result<RetentionSize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<u64>::deserialize(deserializer)? {
        None => RetentionSize::Unbounded,
        Some(bytes) => RetentionSize::Bytes(bytes),
    })
}

/// How a partition is kept bounded, fully resolved.
///
/// Both bounds apply per partition, independently of every other partition; an
/// event past either may be removed, never one within both. Removal always
/// takes the oldest sequences first, so what remains is a suffix of what was
/// stored. How soon past a bound it happens is the topic's storage backend's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetentionSettings {
    pub duration: std::time::Duration,
    pub size_bytes: Option<u64>,
}

/// A topic's own settings entry and the instance-less entry for its type,
/// either of which may be absent. Returned by [`EventBrokerConfig::entries_for`].
type TopicEntryPair<'a> = (
    Option<&'a TopicSettingsEntry>,
    Option<&'a TopicSettingsEntry>,
);

impl EventBrokerConfig {
    /// The two entries that can reach one topic: its own, and the
    /// instance-less entry for its type. Either may be absent.
    ///
    /// Exposed rather than folded here because the fold has a third tier this
    /// type cannot see - what the topic's own specification declares - and a
    /// ladder split across two files would be two places to get the order
    /// wrong. `crate::domain::resolution` applies all of them.
    ///
    /// # Errors
    /// [`TopicSettingsError::MalformedTopicId`] when the identifier does not
    /// decompose into a type and an instance part, so no instance-less key
    /// could be derived from it.
    pub fn entries_for(
        &self,
        topic: &toolkit_gts::GtsInstanceId,
    ) -> Result<TopicEntryPair<'_>, TopicSettingsError> {
        let topic_id = topic.as_ref();
        let type_id = type_id_of(topic_id)?;
        Ok((
            self.topics.entries.get(topic_id),
            self.topics.entries.get(&type_id),
        ))
    }
}

/// The instance-less key for a topic identifier: everything up to and
/// including the type's trailing `~`.
fn type_id_of(topic_id: &str) -> Result<String, TopicSettingsError> {
    let parsed =
        gts::GtsId::try_new(topic_id).map_err(|e| TopicSettingsError::MalformedTopicId {
            topic: topic_id.to_owned(),
            reason: e.to_string(),
        })?;
    parsed
        .get_type_id()
        .ok_or_else(|| TopicSettingsError::MalformedTopicId {
            topic: topic_id.to_owned(),
            reason: "identifier carries no type segment to key a default on".to_owned(),
        })
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;
