//! A Kafka registry entry's settings (`docs/DESIGN.md` §1).
//!
//! Read by hand rather than through serde, so an error names the setting and
//! the rule it breaks and never quotes a value.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use toolkit_utils::var_expand::expand_env_vars;

/// The longest topic prefix: Kafka names are at most 249 characters, and the
/// plugin appends `.` and 32 hex digits.
pub const MAX_PREFIX: usize = 200;

/// Most fence-key producers one instance may hold open.
pub const MAX_PRODUCER_POOL: usize = 256;

/// librdkafka properties an entry may pass through to every client the plugin
/// creates. Everything that decides the plugin's guarantees - acks,
/// idempotence, transactions, isolation, offsets, groups - is the plugin's own
/// and not listed; so is security, which this plugin does not support yet.
const CLIENT_ALLOWED: &[&str] = &[
    "client.id",
    "client.dns.lookup",
    "broker.address.family",
    "broker.address.ttl",
    "connections.max.idle.ms",
    "socket.keepalive.enable",
    "socket.connection.setup.timeout.ms",
    "socket.send.buffer.bytes",
    "socket.receive.buffer.bytes",
    "socket.nagle.disable",
    "reconnect.backoff.ms",
    "reconnect.backoff.max.ms",
    "retry.backoff.ms",
    "retry.backoff.max.ms",
    "metadata.max.age.ms",
    "topic.metadata.refresh.interval.ms",
    "message.max.bytes",
    "compression.type",
    "linger.ms",
    "batch.size",
    "batch.num.messages",
    "queue.buffering.max.messages",
    "queue.buffering.max.kbytes",
    "fetch.max.bytes",
    "max.partition.fetch.bytes",
    "fetch.wait.max.ms",
    "queued.max.messages.kbytes",
    "statistics.interval.ms",
    "debug",
];

/// One registry entry's settings, defaults applied.
#[derive(Clone, PartialEq, Eq)]
pub struct KafkaSettings {
    /// `host:port[,host:port...]`, `${VAR}` expanded.
    pub bootstrap_servers: String,
    /// Every Kafka topic and transactional id of this entry starts with it.
    pub topic_prefix: String,
    /// Replicas of every topic the plugin creates.
    pub replication_factor: u16,
    /// `min.insync.replicas` of every topic the plugin creates.
    pub min_insync_replicas: u16,
    /// Partitions of the fence topic, created once.
    pub fence_partitions: u32,
    /// How often the gear polls heads, and a waiting read probes.
    pub poll_interval: Duration,
    /// Most consumers one instance holds at once.
    pub consumer_pool: usize,
    /// Most fence keys one instance keeps a transactional producer open for.
    pub producer_pool: usize,
    /// Bound of one request to the cluster: admin calls, offsets lookups,
    /// transaction steps.
    pub request_timeout: Duration,
    /// `transaction.timeout.ms` of the fence producers: how long an
    /// abandoned transaction holds back `read_committed` readers.
    pub transaction_timeout: Duration,
    /// Extra librdkafka properties, from the allow-list.
    pub client: BTreeMap<String, String>,
}

impl fmt::Debug for KafkaSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KafkaSettings")
            .field("bootstrap_servers", &"<redacted>")
            .field("topic_prefix", &self.topic_prefix)
            .field("replication_factor", &self.replication_factor)
            .field("min_insync_replicas", &self.min_insync_replicas)
            .field("fence_partitions", &self.fence_partitions)
            .field("poll_interval", &self.poll_interval)
            .field("consumer_pool", &self.consumer_pool)
            .field("producer_pool", &self.producer_pool)
            .field("request_timeout", &self.request_timeout)
            .field("transaction_timeout", &self.transaction_timeout)
            .field("client", &self.client.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl KafkaSettings {
    /// Reads an entry's settings; an unknown setting or a broken rule is
    /// `InvalidConfig`.
    pub fn parse(
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, BackendError> {
        let mut bootstrap = None;
        let mut parsed = Self {
            bootstrap_servers: String::new(),
            topic_prefix: "evbk".to_owned(),
            replication_factor: 3,
            min_insync_replicas: 2,
            fence_partitions: 16,
            poll_interval: Duration::from_millis(500),
            consumer_pool: 8,
            producer_pool: 16,
            request_timeout: Duration::from_secs(10),
            transaction_timeout: Duration::from_secs(10),
            client: BTreeMap::new(),
        };
        for (name, value) in settings {
            match name.as_str() {
                "bootstrap_servers" => {
                    let raw = value
                        .as_str()
                        .ok_or_else(|| invalid("`bootstrap_servers` must be a string"))?;
                    let expanded = expand_env_vars(raw).map_err(|_| {
                        invalid("`bootstrap_servers` names an environment variable that is not set")
                    })?;
                    if expanded.trim().is_empty() {
                        return Err(invalid("`bootstrap_servers` must not be empty"));
                    }
                    bootstrap = Some(expanded);
                }
                "topic_prefix" => {
                    let prefix = value.as_str().filter(|p| is_prefix(p)).ok_or_else(|| {
                        invalid(format!(
                            "`topic_prefix` must be 1 to {MAX_PREFIX} characters of [a-zA-Z0-9._-]"
                        ))
                    })?;
                    prefix.clone_into(&mut parsed.topic_prefix);
                }
                "replication_factor" => {
                    parsed.replication_factor = small(value, "replication_factor")?;
                }
                "min_insync_replicas" => {
                    parsed.min_insync_replicas = small(value, "min_insync_replicas")?;
                }
                "fence_partitions" => {
                    parsed.fence_partitions = positive(value, "fence_partitions")?;
                }
                "poll_interval_ms" => parsed.poll_interval = millis(value, "poll_interval_ms")?,
                "consumer_pool" => {
                    parsed.consumer_pool = positive(value, "consumer_pool")? as usize;
                }
                "producer_pool" => {
                    parsed.producer_pool = positive(value, "producer_pool")? as usize;
                }
                "request_timeout_ms" => {
                    parsed.request_timeout = millis(value, "request_timeout_ms")?;
                }
                "transaction_timeout_ms" => {
                    parsed.transaction_timeout = millis(value, "transaction_timeout_ms")?;
                }
                "client" => parsed.client = client(value)?,
                other => {
                    return Err(invalid(format!("`{other}` is not a kafka backend setting")));
                }
            }
        }
        parsed.bootstrap_servers =
            bootstrap.ok_or_else(|| invalid("`bootstrap_servers` is required"))?;
        if parsed.request_timeout > parsed.transaction_timeout {
            return Err(invalid(
                "`request_timeout_ms` must not exceed `transaction_timeout_ms`: librdkafka refuses \
                 a socket timeout longer than the transaction timeout",
            ));
        }
        if parsed.producer_pool > MAX_PRODUCER_POOL {
            return Err(invalid(format!(
                "`producer_pool` must be at most {MAX_PRODUCER_POOL}: each producer runs \
                 librdkafka threads and a connection per broker"
            )));
        }
        if parsed.min_insync_replicas > parsed.replication_factor {
            return Err(invalid(
                "`min_insync_replicas` must not exceed `replication_factor`",
            ));
        }
        Ok(parsed)
    }
}

fn invalid(detail: impl fmt::Display) -> BackendError {
    BackendError::invalid_config(detail)
}

fn millis(value: &serde_json::Value, name: &str) -> Result<Duration, BackendError> {
    value
        .as_u64()
        .filter(|ms| *ms > 0 && u32::try_from(*ms).is_ok())
        .map(Duration::from_millis)
        .ok_or_else(|| {
            invalid(format!(
                "`{name}` must be a positive integer of milliseconds"
            ))
        })
}

fn positive(value: &serde_json::Value, name: &str) -> Result<u32, BackendError> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid(format!("`{name}` must be a positive integer")))
}

fn small(value: &serde_json::Value, name: &str) -> Result<u16, BackendError> {
    value
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| (1..=32).contains(n))
        .ok_or_else(|| invalid(format!("`{name}` must be an integer from 1 to 32")))
}

fn client(value: &serde_json::Value) -> Result<BTreeMap<String, String>, BackendError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("`client` must be an object of librdkafka properties"))?;
    let mut parsed = BTreeMap::new();
    for (name, value) in object {
        if !CLIENT_ALLOWED.contains(&name.as_str()) {
            return Err(invalid(format!(
                "`client.{name}` is not a librdkafka property this backend passes through"
            )));
        }
        let text = match value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => {
                return Err(invalid(format!(
                    "`client.{name}` must be a string, a number or a boolean"
                )));
            }
        };
        parsed.insert(name.clone(), text);
    }
    Ok(parsed)
}

/// A legal Kafka topic name fragment, short enough to prefix every name the
/// plugin builds.
pub fn is_prefix(prefix: &str) -> bool {
    (1..=MAX_PREFIX).contains(&prefix.len())
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && prefix != "."
        && prefix != ".."
}
